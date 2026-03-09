// SPDX-License-Identifier: MPL-2.0

use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use aster_softirq::BottomHalfDisabled;
use aster_virtio::device::vsock::{
    header::{VirtioVsockHdr, VirtioVsockOp, VirtioVsockShutdownFlags},
    packet::{RxPacket, TxPacket, TxPacketBuilder},
    queue::TxCompletion,
};
use ostd::sync::SpinLock;
use takeable::Takeable;

use crate::{
    events::IoEvents,
    net::socket::{
        util::{SendRecvFlags, SockShutdownCmd},
        vsock::{
            addr::VsockSocketAddr,
            backend::{
                BoundPort, CREDIT_UPDATE_THRESHOLD, DEFAULT_CLOSE_TIMEOUT, DEFAULT_CONNECT_TIMEOUT,
                DEFAULT_PENDING_TX_BYTES, DEFAULT_RX_BUF_SIZE,
            },
        },
    },
    prelude::*,
    process::signal::Pollee,
    time::Timer,
    util::{MultiRead, MultiWrite},
};

pub(in crate::net::socket::vsock) struct Connection {
    inner: Takeable<Arc<ConnectionInner>>,
}

pub(super) struct ConnectionInner {
    conn_id: ConnId,
    bound_port: BoundPort,
    pollee: Pollee,
    state: SpinLock<ConnectionState, BottomHalfDisabled>,
    available_tx_bytes: AtomicUsize,
}

struct ConnectionState {
    phase: Phase,
    error: Option<Error>,
    rx_queue: RxQueue,
    credit: CreditState,
    shutdown: ShutdownState,
    /// Tracks the deadline for leaving [`Phase::Connecting`] or [`Phase::Closing`].
    ///
    /// INVARIANT: This is `Some(_)` if and only if the phase is
    /// [`Phase::Connecting`] or [`Phase::Closing`].
    timer: Option<TimerState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Represents the initial state of a newly created connection.
    ///
    /// INVARIANT: In this state, the peer endpoint is fully closed; that is,
    /// both `peer_write_closed` and `peer_read_closed` are `true`.
    Connecting,

    /// Represents the state reached from `Connecting` when the connection
    /// request is rejected or times out.
    ///
    /// INVARIANT: In this state, the peer endpoint is fully closed.
    ConnectFailed,

    /// Represents the initial state of an accepted connection, or the state
    /// reached from `Connecting` when the connection request succeeds.
    Connected,

    /// Represents the state reached from `Connected` when the socket is closed
    /// locally, but the peer has not reset the connection.
    ///
    /// INVARIANT: In this state, the local endpoint is fully closed; that is,
    /// both `local_write_closed` and `local_read_closed` are `true`.
    Closing,

    /// Represents the state reached from `Connected` or `Closing` when the peer
    /// fully shuts down the connection or resets it, or when a close timeout
    /// expires in `Closing`.
    ///
    /// INVARIANT: In this state, the peer endpoint is fully closed.
    Closed,
}

struct RxQueue {
    packets: VecDeque<RxPacket>,
    used_bytes: usize,
    read_offset: usize,
}

struct CreditState {
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    local_fwd_cnt: u32,
    last_reported_fwd_cnt: u32,
    credit_request_pending: bool,
    tx_cnt: u32,
}

struct ShutdownState {
    local_read_closed: bool,
    local_write_closed: bool,
    peer_read_closed: bool,
    peer_write_closed: bool,
}

struct TimerState {
    generation: u64,
    #[expect(dead_code)]
    timer: Arc<Timer>,
}

impl Connection {
    pub(super) fn new(inner: Arc<ConnectionInner>) -> Self {
        Self {
            inner: Takeable::new(inner),
        }
    }

    pub(in crate::net::socket::vsock) fn local_addr(&self) -> VsockSocketAddr {
        VsockSocketAddr {
            cid: self.inner.conn_id.local_cid as u32,
            port: self.inner.conn_id.local_port,
        }
    }

    pub(in crate::net::socket::vsock) fn remote_addr(&self) -> VsockSocketAddr {
        VsockSocketAddr {
            cid: self.inner.conn_id.peer_cid as u32,
            port: self.inner.conn_id.peer_port,
        }
    }
}

pub(in crate::net::socket::vsock) enum ConnectResult {
    Connecting(Connection),
    Connected(Connection),
    Failed(BoundPort, Error),
}

impl Connection {
    pub(in crate::net::socket::vsock) fn has_connect_result(&self) -> bool {
        let state = self.inner.state.lock();
        match state.phase {
            Phase::ConnectFailed => Arc::strong_count(&self.inner) == 1,
            Phase::Connecting => false,
            Phase::Connected | Phase::Closing | Phase::Closed => true,
        }
    }

    pub(in crate::net::socket::vsock) fn finish_connect(mut self) -> ConnectResult {
        let mut state = self.inner.state.lock();
        match state.phase {
            Phase::ConnectFailed if Arc::strong_count(&self.inner) == 1 => {
                let error = state.error.take();
                drop(state);
                ConnectResult::Failed(
                    Arc::into_inner(self.inner.take()).unwrap().bound_port,
                    error.unwrap(),
                )
            }
            Phase::ConnectFailed | Phase::Connecting => {
                drop(state);
                ConnectResult::Connecting(self)
            }
            Phase::Connected | Phase::Closing | Phase::Closed => {
                drop(state);
                ConnectResult::Connected(self)
            }
        }
    }

    pub(in crate::net::socket::vsock) fn test_and_clear_error(&self) -> Result<()> {
        let mut state = self.inner.state.lock();
        state.test_and_clear_error(&self.inner)
    }
}

impl Connection {
    pub(in crate::net::socket::vsock) fn try_recv(
        &mut self,
        writer: &mut dyn MultiWrite,
        _flags: SendRecvFlags,
    ) -> Result<usize> {
        let mut packet_pool = [const { None }; 8];

        let Some(mut packets) = self.inner.state.lock().grab_packets_to_recv(
            &self.inner,
            &mut packet_pool[..],
            writer.sum_lens(),
        )?
        else {
            return Ok(0);
        };

        let result = packets.copy_to_userspace(writer);
        let recv_len = *result.as_ref().unwrap_or(&0);

        self.inner
            .state
            .lock()
            .ungrab_packets_and_finish_recv(&self.inner, packets, recv_len);

        self.inner.pollee.invalidate();

        result
    }
}

struct PoppedRxPackets<'a> {
    packets: &'a mut [Option<RxPacket>],
    read_offset: usize,
}

impl PoppedRxPackets<'_> {
    fn copy_to_userspace(&mut self, writer: &mut dyn MultiWrite) -> Result<usize> {
        let mut read_offset = self.read_offset;
        let mut total_write_len = 0;

        for (i, packet) in self.packets.iter().enumerate() {
            let packet = packet.as_ref().unwrap();

            let mut payload = packet.payload();
            payload.skip(read_offset);

            let write_len = writer.write(&mut payload)?;
            read_offset += write_len;
            total_write_len += write_len;

            if payload.has_remain() {
                self.skip_packets(i);
                self.read_offset = read_offset;
                return Ok(total_write_len);
            }

            read_offset = 0;
        }

        self.packets = &mut [];
        self.read_offset = 0;
        Ok(total_write_len)
    }

    fn skip_packets(&mut self, n: usize) {
        let mut packets = &mut [][..];
        core::mem::swap(&mut self.packets, &mut packets);
        packets = &mut packets[n..];
        core::mem::swap(&mut self.packets, &mut packets);
    }
}

impl ConnectionState {
    fn grab_packets_to_recv<'a>(
        &mut self,
        conn: &ConnectionInner,
        packet_pool: &'a mut [Option<RxPacket>],
        max_bytes: usize,
    ) -> Result<Option<PoppedRxPackets<'a>>> {
        let Some(packets) = self.pop_rx_packets(&mut packet_pool[..], max_bytes) else {
            self.test_and_clear_error(conn)?;

            if self.shutdown.local_read_closed || self.shutdown.peer_write_closed {
                return Ok(None);
            }
            return_errno_with_message!(Errno::EAGAIN, "the receive buffer is empty");
        };

        Ok(Some(packets))
    }

    fn pop_rx_packets<'a>(
        &mut self,
        packet_pool: &'a mut [Option<RxPacket>],
        mut max_bytes: usize,
    ) -> Option<PoppedRxPackets<'a>> {
        let mut read_offset = None;
        let mut num_packets = 0;

        for packet_opt in packet_pool.iter_mut() {
            *packet_opt = self.rx_queue.packets.pop_front();
            let Some(packet_ref) = packet_opt.as_ref() else {
                break;
            };

            num_packets += 1;

            if read_offset.is_none() {
                read_offset = Some(self.rx_queue.read_offset);
                self.rx_queue.read_offset = 0;
            }

            let payload_len = packet_ref.payload_len();
            if payload_len >= max_bytes {
                break;
            } else {
                max_bytes -= payload_len;
            }
        }

        read_offset.map(|read_offset| PoppedRxPackets {
            packets: &mut packet_pool[0..num_packets],
            read_offset,
        })
    }

    fn ungrab_packets_and_finish_recv(
        &mut self,
        conn: &ConnectionInner,
        packets: PoppedRxPackets,
        recv_len: usize,
    ) {
        self.undo_pop_rx_packets(packets);

        self.rx_queue.used_bytes -= recv_len;
        self.credit.local_fwd_cnt = self.credit.local_fwd_cnt.wrapping_add(recv_len as u32);

        self.send_credit_update_header_if_needed(conn);
    }

    fn undo_pop_rx_packets(&mut self, packets: PoppedRxPackets) {
        debug_assert_eq!(self.rx_queue.read_offset, 0);

        if packets.packets.is_empty() {
            return;
        }

        debug_assert!(packets.read_offset < packets.packets[0].as_ref().unwrap().payload_len());

        for packet_opt in packets.packets.iter_mut().rev() {
            self.rx_queue.packets.push_front(packet_opt.take().unwrap());
        }
        self.rx_queue.read_offset = packets.read_offset;
    }

    fn send_credit_update_header_if_needed(&mut self, conn: &ConnectionInner) {
        let new_credit = self
            .credit
            .local_fwd_cnt
            .wrapping_sub(self.credit.last_reported_fwd_cnt);
        if new_credit < CREDIT_UPDATE_THRESHOLD {
            return;
        }

        // No need to report credit updates if the peer cannot send new data.
        if self.shutdown.peer_write_closed || self.shutdown.local_read_closed {
            return;
        }

        let _ = self.send_packet(conn, VirtioVsockOp::CreditUpdate, 0);
    }
}

impl Connection {
    pub(in crate::net::socket::vsock) fn try_send(
        &mut self,
        reader: &mut dyn MultiRead,
        _flags: SendRecvFlags,
    ) -> Result<usize> {
        let max_bytes = reader.sum_lens();
        if max_bytes == 0 {
            return Ok(0);
        }

        let mut packet_pool = [const { None }; 8];

        let num_bytes = self.alloc_send_buffers(&mut packet_pool[..], max_bytes)?;

        // TODO: If the user sends too many small packets, we'll exhaust a large amount of kernel
        // memory. We need to support merging small packets to avoid this.

        Self::copy_to_send_buffers(&mut packet_pool[..], reader, num_bytes)?;

        self.build_and_send_tx_packets(&mut packet_pool[..])?;

        self.inner.pollee.invalidate();

        Ok(num_bytes)
    }

    fn alloc_send_buffers(
        &mut self,
        packet_pool: &mut [Option<TxPacketBuilder>],
        max_bytes: usize,
    ) -> Result<usize> {
        let mut state = self.inner.state.lock();

        state.test_and_clear_error(&self.inner)?;

        if state.shutdown.local_write_closed || state.shutdown.peer_read_closed {
            return_errno_with_message!(Errno::EPIPE, "the connection is closed for writing");
        }

        let buffer_bytes = self.inner.available_tx_bytes.load(Ordering::Relaxed);
        if buffer_bytes == 0 {
            return_errno_with_message!(Errno::EAGAIN, "the pending queue is full");
        }

        let credit_bytes = state.check_peer_credit(&self.inner)?;
        debug_assert_ne!(credit_bytes, 0);

        let max_bytes = max_bytes.min(buffer_bytes).min(credit_bytes);
        let mut num_bytes = 0;

        for packet_opt in packet_pool.iter_mut() {
            *packet_opt = Some(TxPacket::new_builder()?);

            num_bytes += TxPacketBuilder::MAX_NBYTES;
            if num_bytes >= max_bytes {
                num_bytes = max_bytes;
                break;
            }
        }

        Ok(num_bytes)
    }

    fn copy_to_send_buffers(
        packet_pool: &mut [Option<TxPacketBuilder>],
        reader: &mut dyn MultiRead,
        num_bytes: usize,
    ) -> Result<()> {
        let mut remaining_bytes = num_bytes;

        for packet_opt in packet_pool.iter_mut() {
            if remaining_bytes == 0 {
                break;
            }

            let packet_mut = packet_opt.as_mut().unwrap();
            let bytes_written = packet_mut.append(|mut writer| {
                writer.limit(remaining_bytes);
                Ok(reader.read(&mut writer)?)
            })?;
            remaining_bytes -= bytes_written;
        }

        debug_assert_eq!(remaining_bytes, 0);

        Ok(())
    }

    fn build_and_send_tx_packets(&self, packet_pool: &mut [Option<TxPacketBuilder>]) -> Result<()> {
        let mut state = self.inner.state.lock();

        if state.shutdown.local_write_closed || state.shutdown.peer_read_closed {
            return_errno_with_message!(Errno::EPIPE, "the connection is closed for writing");
        }

        let vsock_space = self.inner.bound_port.vsock_space();
        let mut tx = vsock_space.device().lock_tx();

        let mut num_bytes = 0;
        let mut num_bytes_in_pending = 0;

        for packet_opt in packet_pool.iter_mut() {
            let Some(packet_builder) = packet_opt.take() else {
                break;
            };

            let nbytes = packet_builder.payload_len();
            let packet = state.make_tx_packet(&self.inner, packet_builder);

            match tx.try_send(packet) {
                Ok(()) => (),
                Err(pending) => {
                    let completion = ReleasePendingBytes {
                        connection: (*self.inner).clone(),
                        bytes: nbytes,
                    };
                    pending.push_pending(Some(Box::new(completion)));

                    num_bytes_in_pending += nbytes;
                }
            }

            num_bytes += nbytes;
        }

        let buffer_bytes = self
            .inner
            .available_tx_bytes
            .fetch_sub(num_bytes_in_pending, Ordering::Relaxed);
        debug_assert!(buffer_bytes >= num_bytes_in_pending);

        state.consume_peer_credit(num_bytes);

        Ok(())
    }
}

impl ConnectionState {
    fn check_peer_credit(&mut self, conn: &ConnectionInner) -> Result<usize> {
        let peer_free = self.peer_credit();

        if peer_free != 0 {
            return Ok(peer_free);
        }

        if !self.credit.credit_request_pending
            && self.send_packet(conn, VirtioVsockOp::CreditRequest, 0)
        {
            self.credit.credit_request_pending = true;
        }

        return_errno_with_message!(Errno::EAGAIN, "the peer has no receive credit");
    }

    fn peer_credit(&self) -> usize {
        let alloc = self.credit.peer_buf_alloc;
        let used = self.credit.tx_cnt.wrapping_sub(self.credit.peer_fwd_cnt);
        alloc.saturating_sub(used) as usize
    }

    fn consume_peer_credit(&mut self, num_bytes: usize) {
        self.credit.tx_cnt = self.credit.tx_cnt.wrapping_add(num_bytes as u32);
    }
}

struct ReleasePendingBytes {
    connection: Arc<ConnectionInner>,
    bytes: usize,
}

impl TxCompletion for ReleasePendingBytes {}

impl Drop for ReleasePendingBytes {
    fn drop(&mut self) {
        self.connection
            .available_tx_bytes
            .fetch_add(self.bytes, Ordering::Relaxed);
        self.connection.pollee.notify(IoEvents::OUT);
    }
}

impl Connection {
    pub(in crate::net::socket::vsock) fn shutdown(&self, cmd: SockShutdownCmd) -> Result<()> {
        self.shutdown_or_drop_common(cmd, false);
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.inner.is_usable() {
            return;
        }

        self.shutdown_or_drop_common(SockShutdownCmd::SHUT_RDWR, true);
    }
}

impl Connection {
    fn shutdown_or_drop_common(&self, cmd: SockShutdownCmd, on_drop: bool) {
        let mut state = self.inner.state.lock();

        let mut notify_events = IoEvents::empty();
        let mut shutdown_flags = VirtioVsockShutdownFlags::empty();

        if cmd.shut_read() && !state.shutdown.local_read_closed {
            state.shutdown.local_read_closed = true;
            shutdown_flags |= VirtioVsockShutdownFlags::RECEIVE;
            notify_events |= IoEvents::IN | IoEvents::RDHUP | IoEvents::HUP;
        }

        if cmd.shut_write() && !state.shutdown.local_write_closed {
            state.shutdown.local_write_closed = true;
            shutdown_flags |= VirtioVsockShutdownFlags::SEND;
            notify_events |= IoEvents::HUP;
        }

        let local_fully_closed =
            state.shutdown.local_read_closed && state.shutdown.local_write_closed;
        let peer_fully_closed = state.shutdown.peer_read_closed && state.shutdown.peer_write_closed;

        if !local_fully_closed || !peer_fully_closed {
            if !shutdown_flags.is_empty() {
                let _ =
                    state.send_packet(&self.inner, VirtioVsockOp::Shutdown, shutdown_flags.bits());
            }

            if on_drop {
                debug_assert!(local_fully_closed);
                state.phase = Phase::Closing;
                state.arm_timeout(&self.inner, DEFAULT_CLOSE_TIMEOUT);
            }

            drop(state);
        } else if state.phase != Phase::Closed {
            state.phase = Phase::Closed;
            let _ = state.send_packet(&self.inner, VirtioVsockOp::Rst, 0);
            drop(state);

            let vsock_space = self.inner.bound_port.vsock_space();
            vsock_space.remove_connection(&self.inner);
        }

        if !on_drop {
            self.inner.pollee.notify(notify_events);
        }
    }
}

impl Connection {
    pub(in crate::net::socket::vsock) fn check_io_events(&self) -> IoEvents {
        // The socket layer handles the `Connecting` and `ConnectFailed` phases. The `Closing`
        // phase indicates that the socket file has been closed. None of them will reach this
        // method.
        //
        // This method only needs to work for the `Connected` and `Closed` phases. Most of the
        // logic below is not very intuitive, but it aims to mimic Linux behavior as much as
        // possible.

        let state = self.inner.state.lock();
        let mut events = IoEvents::empty();

        let local_fully_closed =
            state.shutdown.local_read_closed && state.shutdown.local_write_closed;
        let peer_fully_closed = state.shutdown.peer_read_closed && state.shutdown.peer_write_closed;

        if !state.rx_queue.packets.is_empty() {
            events |= IoEvents::IN;
        }

        if state.shutdown.peer_write_closed || state.shutdown.local_read_closed {
            events |= IoEvents::IN | IoEvents::RDHUP;
        }

        // Most sockets tend to report EPOLLOUT once the write side has been shut down. However,
        // the logic for vsock appears to be different.
        if !state.shutdown.local_write_closed {
            if state.peer_credit() != 0
                && self.inner.available_tx_bytes.load(Ordering::Relaxed) != 0
            {
                events |= IoEvents::OUT;
            }

            if peer_fully_closed {
                events |= IoEvents::OUT;
            }
        }

        if local_fully_closed
            || (state.shutdown.peer_write_closed && state.shutdown.local_write_closed)
        {
            events |= IoEvents::HUP;
        }

        if state.error.is_some() {
            events |= IoEvents::ERR;
        }

        events
    }

    pub(in crate::net::socket::vsock) fn pollee(&self) -> &Pollee {
        &self.inner.pollee
    }
}

impl ConnectionState {
    fn test_and_clear_error(&mut self, conn: &ConnectionInner) -> Result<()> {
        if let Some(error) = self.error.take() {
            conn.pollee.invalidate();
            return Err(error);
        }

        Ok(())
    }

    #[must_use]
    fn send_packet(&mut self, conn: &ConnectionInner, op: VirtioVsockOp, flags: u32) -> bool {
        let header = VirtioVsockHdr::new(
            conn.conn_id.local_cid,
            conn.conn_id.peer_cid,
            conn.conn_id.local_port,
            conn.conn_id.peer_port,
            0,
            op,
            flags,
            DEFAULT_RX_BUF_SIZE as u32,
            self.credit.local_fwd_cnt,
        );

        if conn.bound_port.vsock_space().send_packet(&header) {
            self.credit.last_reported_fwd_cnt = self.credit.local_fwd_cnt;
            true
        } else {
            false
        }
    }

    fn make_tx_packet(
        &mut self,
        conn: &ConnectionInner,
        packet_builder: TxPacketBuilder,
    ) -> TxPacket {
        let header = VirtioVsockHdr::new(
            conn.conn_id.local_cid,
            conn.conn_id.peer_cid,
            conn.conn_id.local_port,
            conn.conn_id.peer_port,
            packet_builder.payload_len() as u32,
            VirtioVsockOp::Rw,
            0,
            DEFAULT_RX_BUF_SIZE as u32,
            self.credit.local_fwd_cnt,
        );
        self.credit.last_reported_fwd_cnt = self.credit.local_fwd_cnt;

        packet_builder.build(&header)
    }

    fn arm_timeout(&mut self, conn: &ConnectionInner, duration: Duration) {
        use super::timer::{next_timer_generation, push_timer_event};
        use crate::time::{clocks::JIFFIES_TIMER_MANAGER, timer::Timeout};

        let timer_manager = JIFFIES_TIMER_MANAGER.get().unwrap();

        let conn_id = conn.conn_id;
        let generation = next_timer_generation();

        let timer = timer_manager.create_timer(move |_guard| {
            push_timer_event(conn_id, generation);
        });
        timer.lock().set_timeout(Timeout::After(duration));

        self.timer = Some(TimerState { generation, timer });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ConnId {
    pub(super) local_cid: u64,
    pub(super) peer_cid: u64,
    pub(super) local_port: u32,
    pub(super) peer_port: u32,
}

impl ConnId {
    pub(super) fn from_port_and_remote(port: &BoundPort, remote: VsockSocketAddr) -> Self {
        Self {
            local_cid: port.vsock_space().guest_cid(),
            peer_cid: remote.cid as u64,
            local_port: port.port(),
            peer_port: remote.port,
        }
    }

    pub(super) fn from_incoming_header(header: &VirtioVsockHdr) -> Self {
        Self {
            local_cid: header.dst_cid(),
            peer_cid: header.src_cid(),
            local_port: header.dst_port(),
            peer_port: header.src_port(),
        }
    }
}

impl ConnectionInner {
    pub(super) fn new_connecting(
        bound_port: BoundPort,
        conn_id: &ConnId,
        pollee: Pollee,
    ) -> Arc<Self> {
        pollee.invalidate();

        let this = Self::new(bound_port, conn_id, pollee, Phase::Connecting);

        let mut state = this.state.lock();
        let _ = state.send_packet(&this, VirtioVsockOp::Request, 0);
        state.arm_timeout(&this, DEFAULT_CONNECT_TIMEOUT);
        drop(state);

        this
    }

    pub(super) fn new_connected(
        bound_port: BoundPort,
        conn_id: &ConnId,
        header: &VirtioVsockHdr,
    ) -> Arc<Self> {
        let this = Self::new(bound_port, conn_id, Pollee::new(), Phase::Connected);

        let mut state = this.state.lock();
        state.update_peer_credit(&this, header);
        let _ = state.send_packet(&this, VirtioVsockOp::Response, 0);
        drop(state);

        this
    }

    fn new(bound_port: BoundPort, conn_id: &ConnId, pollee: Pollee, phase: Phase) -> Arc<Self> {
        debug_assert_eq!(bound_port.port(), conn_id.local_port);

        let peer_fully_closed = phase != Phase::Connected;

        let state = ConnectionState {
            phase,
            error: None,
            rx_queue: RxQueue {
                packets: VecDeque::new(),
                used_bytes: 0,
                read_offset: 0,
            },
            credit: CreditState {
                peer_buf_alloc: 0,
                peer_fwd_cnt: 0,
                local_fwd_cnt: 0,
                last_reported_fwd_cnt: 0,
                credit_request_pending: false,
                tx_cnt: 0,
            },
            shutdown: ShutdownState {
                local_read_closed: false,
                local_write_closed: false,
                peer_read_closed: peer_fully_closed,
                peer_write_closed: peer_fully_closed,
            },
            timer: None,
        };

        Arc::new(Self {
            conn_id: *conn_id,
            bound_port,
            pollee,
            state: SpinLock::new(state),
            available_tx_bytes: AtomicUsize::new(DEFAULT_PENDING_TX_BYTES),
        })
    }

    pub(super) const fn conn_id(&self) -> ConnId {
        self.conn_id
    }

    pub(super) fn pollee(&self) -> &Pollee {
        &self.pollee
    }

    pub(super) fn on_response(&self, header: &VirtioVsockHdr) -> Result<()> {
        let mut state = self.state.lock();

        if state.phase != Phase::Connecting {
            state.active_rst(self);
            return_errno_with_message!(Errno::EISCONN, "the connection is established");
        }

        state.update_peer_credit(self, header);

        state.phase = Phase::Connected;
        state.shutdown.peer_read_closed = false;
        state.shutdown.peer_write_closed = false;
        state.timer = None;

        drop(state);
        self.pollee.notify(IoEvents::OUT);

        Ok(())
    }

    pub(super) fn on_rst(&self) {
        let mut state = self.state.lock();

        state.do_rst();

        // The caller will notify the pollee _after_ removing the connection from the table.
    }

    pub(super) fn on_shutdown(&self, header: &VirtioVsockHdr) -> bool {
        let mut state = self.state.lock();
        let mut notify_events = IoEvents::empty();

        if state.phase == Phase::Connecting {
            state.active_rst(self);
            return true;
        }

        let flags = VirtioVsockShutdownFlags::from_bits_truncate(header.flags());
        if flags.contains(VirtioVsockShutdownFlags::SEND) && !state.shutdown.peer_write_closed {
            state.shutdown.peer_write_closed = true;
            notify_events |= IoEvents::IN | IoEvents::OUT | IoEvents::RDHUP | IoEvents::HUP;
        }
        if flags.contains(VirtioVsockShutdownFlags::RECEIVE) && !state.shutdown.peer_read_closed {
            state.shutdown.peer_read_closed = true;
            notify_events |= IoEvents::OUT;
        }

        if notify_events.is_empty() {
            return false;
        }

        let local_fully_closed =
            state.shutdown.local_read_closed && state.shutdown.local_write_closed;
        let peer_fully_closed = state.shutdown.peer_read_closed && state.shutdown.peer_write_closed;

        let should_remove = if local_fully_closed && peer_fully_closed {
            state.phase = Phase::Closed;
            let _ = state.send_packet(self, VirtioVsockOp::Rst, 0);
            true
        } else {
            false
        };

        drop(state);
        self.pollee.notify(notify_events);

        should_remove
    }

    pub(super) fn on_rw(&self, header: &VirtioVsockHdr, packet: RxPacket) -> Result<()> {
        let mut state = self.state.lock();

        if state.shutdown.peer_write_closed {
            // We don't check `local_read_closed` because the peer cannot immediately know this
            // information.
            state.active_rst(self);
            return_errno_with_message!(Errno::ENOTCONN, "the connection is not established");
        }

        let len = packet.payload_len();
        if state.rx_queue.used_bytes + len > DEFAULT_RX_BUF_SIZE {
            state.active_rst(self);
            return_errno_with_message!(Errno::ENOMEM, "the receive queue is full");
        }

        state.update_peer_credit(self, header);

        if len != 0 {
            state.rx_queue.used_bytes += len;
            state.rx_queue.packets.push_back(packet);
        }

        // TODO: If the peer sends too many small packets, we'll exhaust a large amount of kernel
        // memory. We need to support merging small packets to avoid this.

        drop(state);
        self.pollee.notify(IoEvents::IN);

        Ok(())
    }

    pub(super) fn on_credit_update(&self, header: &VirtioVsockHdr) -> Result<()> {
        let mut state = self.state.lock();

        if state.phase == Phase::Connecting {
            state.active_rst(self);
            return_errno_with_message!(Errno::ENOTCONN, "the connection is not established");
        }

        state.update_peer_credit(self, header);

        state.credit.credit_request_pending = false;

        Ok(())
    }

    pub(super) fn on_credit_request(&self, header: &VirtioVsockHdr) -> Result<()> {
        let mut state = self.state.lock();

        if state.phase == Phase::Connecting {
            state.active_rst(self);
            return_errno_with_message!(Errno::ENOTCONN, "the connection is not established");
        }

        state.update_peer_credit(self, header);

        let _ = state.send_packet(self, VirtioVsockOp::CreditUpdate, 0);

        Ok(())
    }

    pub(super) fn on_timeout(&self, generation: u64) -> bool {
        let mut state = self.state.lock();

        let Some(timer) = state.timer.as_ref() else {
            return false;
        };
        if timer.generation != generation {
            return false;
        }

        state.active_rst(self);

        // If the connection resets before this method is reached, the timer will already be set to
        // `None`, so we won't get here. Therefore, we know that the connection timed out.
        if state.phase == Phase::ConnectFailed {
            state.error = Some(Error::with_message(
                Errno::ETIMEDOUT,
                "the connection timed out",
            ));
        }

        true
    }

    pub(super) fn active_rst(&self) {
        let mut state = self.state.lock();

        state.active_rst(self);
    }
}

impl ConnectionState {
    fn active_rst(&mut self, conn: &ConnectionInner) {
        if self.do_rst() {
            let _ = self.send_packet(conn, VirtioVsockOp::Rst, 0);
        }

        // The caller will notify the pollee _after_ removing the connection from the table.
    }

    fn do_rst(&mut self) -> bool {
        match self.phase {
            Phase::Connecting => {
                self.phase = Phase::ConnectFailed;
                self.error = Some(Error::with_message(
                    Errno::ECONNRESET,
                    "the connection is refused",
                ));
                self.timer = None;

                true
            }
            Phase::Connected => {
                self.phase = Phase::Closed;
                self.error = Some(Error::with_message(
                    Errno::ECONNRESET,
                    "the connection is reset",
                ));
                self.shutdown.peer_read_closed = true;
                self.shutdown.peer_write_closed = true;

                true
            }
            Phase::Closing => {
                self.phase = Phase::Closed;
                self.shutdown.peer_read_closed = true;
                self.shutdown.peer_write_closed = true;
                self.timer = None;

                true
            }

            Phase::ConnectFailed | Phase::Closed => false,
        }
    }

    fn update_peer_credit(&mut self, conn: &ConnectionInner, header: &VirtioVsockHdr) {
        let mut should_notify = false;

        should_notify |= self.credit.peer_buf_alloc != header.buf_alloc();
        self.credit.peer_buf_alloc = header.buf_alloc();

        should_notify |= self.credit.peer_fwd_cnt != header.fwd_cnt();
        self.credit.peer_fwd_cnt = header.fwd_cnt();

        if should_notify {
            conn.pollee.notify(IoEvents::OUT);
        }
    }
}
