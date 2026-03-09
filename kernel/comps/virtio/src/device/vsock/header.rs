// SPDX-License-Identifier: MPL-2.0

use bitflags::bitflags;
use int_to_c_enum::TryFromInt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromInt)]
#[repr(u16)]
pub enum VirtioVsockType {
    Stream = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromInt)]
#[repr(u16)]
pub enum VirtioVsockOp {
    Request = 1,
    Response = 2,
    Rst = 3,
    Shutdown = 4,
    Rw = 5,
    CreditUpdate = 6,
    CreditRequest = 7,
}

bitflags! {
    pub struct VirtioVsockShutdownFlags: u32 {
        const RECEIVE = 1;
        const SEND = 2;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromInt)]
#[repr(u32)]
pub enum VirtioVsockEventId {
    TransportReset = 0,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, Pod)]
pub struct VirtioVsockHdr {
    pub src_cid: u64,
    pub dst_cid: u64,
    pub src_port: u32,
    pub dst_port: u32,
    pub len: u32,
    pub type_: u16,
    pub op: u16,
    pub flags: u32,
    pub buf_alloc: u32,
    pub fwd_cnt: u32,
}

impl VirtioVsockHdr {
    pub const LEN: usize = size_of::<Self>();

    #[expect(
        clippy::too_many_arguments,
        reason = "the wire header fields map directly to the virtio-vsock specification"
    )]
    pub const fn new(
        src_cid: u64,
        dst_cid: u64,
        src_port: u32,
        dst_port: u32,
        len: u32,
        op: VirtioVsockOp,
        flags: u32,
        buf_alloc: u32,
        fwd_cnt: u32,
    ) -> Self {
        Self {
            src_cid,
            dst_cid,
            src_port,
            dst_port,
            len,
            type_: VirtioVsockType::Stream as u16,
            op: op as u16,
            flags,
            buf_alloc,
            fwd_cnt,
        }
    }

    pub const fn src_cid(&self) -> u64 {
        self.src_cid
    }

    pub const fn dst_cid(&self) -> u64 {
        self.dst_cid
    }

    pub const fn src_port(&self) -> u32 {
        self.src_port
    }

    pub const fn dst_port(&self) -> u32 {
        self.dst_port
    }

    pub const fn flags(&self) -> u32 {
        self.flags
    }

    pub const fn buf_alloc(&self) -> u32 {
        self.buf_alloc
    }

    pub const fn fwd_cnt(&self) -> u32 {
        self.fwd_cnt
    }

    pub fn op(&self) -> Option<VirtioVsockOp> {
        VirtioVsockOp::try_from(self.op).ok()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod)]
pub struct VirtioVsockEvent {
    pub id: u32,
}
