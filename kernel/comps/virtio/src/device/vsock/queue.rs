// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, collections::vec_deque::VecDeque, vec::Vec};

use aster_network::{RxBuffer, TxBuffer};
use aster_util::slot_vec::SlotVec;
use ostd::{
    mm::{
        dma::{DmaStream, FromDevice},
        io::util::HasVmReaderWriter,
    },
    warn,
};

use crate::{
    device::vsock::{
        header::{VirtioVsockEventId, VirtioVsockHdr},
        packet::{RxPacket, TxPacket},
    },
    queue::VirtQueue,
    transport::VirtioTransport,
};

pub struct TxQueue {
    queue: VirtQueue,
    inflight: Vec<Option<TxPacket>>,
    pending: VecDeque<PendingTx>,
}

impl TxQueue {
    pub(super) const QUEUE_INDEX: u16 = 1;
    const QUEUE_SIZE: u16 = 64;

    pub(super) fn new(transport: &mut dyn VirtioTransport) -> Self {
        let queue = VirtQueue::new(Self::QUEUE_INDEX, Self::QUEUE_SIZE, transport)
            .expect("creating send queue fails");

        let inflight = (0..Self::QUEUE_SIZE).map(|_| None).collect();
        let pending = VecDeque::new();

        Self {
            queue,
            inflight,
            pending,
        }
    }

    pub(super) fn free_processed_tx_buffers(&mut self) {
        while let Ok((token, _)) = self.queue.pop_used() {
            debug_assert!(self.inflight[token as usize].is_some());
            self.inflight[token as usize] = None;
        }

        while self.queue.available_desc() >= 1 {
            let Some(pending) = self.pending.pop_front() else {
                break;
            };
            let PendingTx { packet, completion } = pending;

            if let Some(completion) = completion {
                completion.on_pending_submit();
            }

            let token = self
                .queue
                .add_dma_bufs::<TxBuffer, TxBuffer>(&[packet.inner()], &[])
                .unwrap();

            debug_assert!(self.inflight[token as usize].is_none());
            self.inflight[token as usize] = Some(packet);
        }

        if self.queue.should_notify() {
            self.queue.notify();
        }
    }

    pub fn try_send(&mut self, packet: TxPacket) -> core::result::Result<(), TxPendingGuard<'_>> {
        if !self.pending.is_empty() || self.queue.available_desc() == 0 {
            return Err(TxPendingGuard {
                queue: self,
                packet,
            });
        }

        let token = self
            .queue
            .add_dma_bufs::<TxBuffer, TxBuffer>(&[packet.inner()], &[])
            .unwrap();

        debug_assert!(self.inflight[token as usize].is_none());
        self.inflight[token as usize] = Some(packet);

        if self.queue.should_notify() {
            self.queue.notify();
        }

        Ok(())
    }
}

pub struct PendingTx {
    packet: TxPacket,
    completion: Option<Box<dyn TxCompletion>>,
}

pub trait TxCompletion: Send + Sync {
    fn on_pending_submit(self: Box<Self>) {}
}

pub struct TxPendingGuard<'a> {
    queue: &'a mut TxQueue,
    packet: TxPacket,
}

impl TxPendingGuard<'_> {
    pub fn push_pending(self, completion: Option<Box<dyn TxCompletion>>) {
        self.queue.pending.push_back(PendingTx {
            packet: self.packet,
            completion,
        });
    }
}

pub struct RxQueue {
    queue: VirtQueue,
    buffers: SlotVec<RxPacket>,
    pending: Option<RxPacket>,
}

impl RxQueue {
    pub(super) const QUEUE_INDEX: u16 = 0;
    const QUEUE_SIZE: u16 = 64;

    pub(super) fn new(transport: &mut dyn VirtioTransport) -> Self {
        let mut queue = VirtQueue::new(Self::QUEUE_INDEX, Self::QUEUE_SIZE, transport)
            .expect("creating recv queue fails");

        let mut buffers = SlotVec::new();
        for index in 0..Self::QUEUE_SIZE {
            let buffer = RxPacket::new().expect("allocating recv packet fails");
            let token = queue
                .add_dma_bufs::<RxBuffer, RxBuffer>(&[], &[buffer.inner()])
                .unwrap();
            assert_eq!(token, index);
            assert_eq!(buffers.put(buffer) as u16, index);
        }

        if queue.should_notify() {
            queue.notify();
        }

        Self {
            queue,
            buffers,
            pending: None,
        }
    }

    pub fn recv(&mut self) -> Option<RxPacket> {
        while let Some((mut packet, len)) = self.recv_impl() {
            // As with all other virtio devices, we currently assume that the virtio transport
            // layer is trustworthy, and therefore so is `len`.
            //
            // However, we do not assume that the peer is trustworthy, meaning that `len` may be
            // smaller than the header size.
            if len < size_of::<VirtioVsockHdr>() {
                continue;
            }

            packet.set_len(len);
            return Some(packet);
        }

        None
    }

    fn recv_impl(&mut self) -> Option<(RxPacket, usize)> {
        if self.pending.is_none() {
            self.pending = RxPacket::new().ok();
        }
        if self.pending.is_none() {
            warn!("allocating recv packet fails");
            return None;
        }

        let (token, len) = self.queue.pop_used().ok()?;
        let packet = self.buffers.remove(token as usize).unwrap();

        let new_packet = self.pending.take().unwrap();
        let new_token = self
            .queue
            .add_dma_bufs::<RxBuffer, RxBuffer>(&[], &[new_packet.inner()])
            .unwrap();
        debug_assert_eq!(new_token, token);
        self.buffers.put_at(new_token as usize, new_packet);

        if self.queue.should_notify() {
            self.queue.notify();
        }

        Some((packet, len as usize))
    }
}

pub(super) struct EventQueue {
    queue: VirtQueue,
    buffer: DmaStream<FromDevice>,
}

impl EventQueue {
    pub(super) const QUEUE_INDEX: u16 = 2;
    const QUEUE_SIZE: u16 = 1;

    pub(super) fn new(transport: &mut dyn VirtioTransport) -> Self {
        let mut queue = VirtQueue::new(Self::QUEUE_INDEX, Self::QUEUE_SIZE, transport)
            .expect("creating event queue fails");

        let buffer = DmaStream::alloc_uninit(1, false).expect("allocating event buffer fails");
        let token = queue
            .add_dma_bufs::<DmaStream<FromDevice>, DmaStream<FromDevice>>(&[], &[&buffer])
            .unwrap();
        debug_assert_eq!(token, 0);

        if queue.should_notify() {
            queue.notify();
        }

        Self { queue, buffer }
    }

    pub(super) fn recv(&mut self) -> Option<VirtioVsockEventId> {
        let (token, len) = self.queue.pop_used().ok()?;
        debug_assert_eq!(token, 0);
        debug_assert_eq!(len as usize, size_of::<VirtioVsockEventId>());

        self.buffer
            .sync_from_device(0..size_of::<VirtioVsockEventId>())
            .unwrap();
        let event_id = self.buffer.reader().unwrap().read_val::<u32>().unwrap();
        let event_id = VirtioVsockEventId::try_from(event_id).ok();

        let token = self
            .queue
            .add_dma_bufs::<DmaStream<FromDevice>, DmaStream<FromDevice>>(&[], &[&self.buffer])
            .unwrap();
        debug_assert_eq!(token, 0);

        event_id
    }
}
