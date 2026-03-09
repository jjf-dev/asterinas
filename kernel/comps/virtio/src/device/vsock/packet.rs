// SPDX-License-Identifier: MPL-2.0

use aster_network::{RxBuffer, TxBuffer, TxBufferBuilder};
use ostd::{
    Result,
    mm::{Infallible, VmReader, VmWriter},
};

use crate::device::vsock::{
    buffer::{RX_BUFFER_POOL, TX_BUFFER_LEN, TX_BUFFER_POOL},
    header::VirtioVsockHdr,
};

pub struct TxPacket(TxBuffer);

impl TxPacket {
    pub fn new(header: &VirtioVsockHdr) -> Result<Self> {
        Ok(Self::new_builder()?.build(header))
    }

    pub fn new_builder() -> Result<TxPacketBuilder> {
        TxBuffer::new_builder(TX_BUFFER_POOL.get().unwrap()).map(TxPacketBuilder)
    }

    pub(super) fn inner(&self) -> &TxBuffer {
        &self.0
    }
}

pub struct TxPacketBuilder(TxBufferBuilder<VirtioVsockHdr>);

impl TxPacketBuilder {
    pub const MAX_NBYTES: usize = TX_BUFFER_LEN - size_of::<VirtioVsockHdr>();

    pub fn append<F>(&mut self, f: F) -> Result<usize>
    where
        F: FnOnce(VmWriter<Infallible>) -> Result<usize>,
    {
        self.0.append(f)
    }

    pub fn payload_len(&self) -> usize {
        self.0.packet_len()
    }

    pub fn build(self, header: &VirtioVsockHdr) -> TxPacket {
        TxPacket(self.0.build(header))
    }
}

pub struct RxPacket(RxBuffer);

impl RxPacket {
    pub(super) fn new() -> Result<Self> {
        RxBuffer::new(size_of::<VirtioVsockHdr>(), RX_BUFFER_POOL.get().unwrap()).map(Self)
    }

    pub(super) fn set_len(&mut self, len: usize) {
        self.0.set_packet_len(len - size_of::<VirtioVsockHdr>());
    }

    pub(super) fn inner(&self) -> &RxBuffer {
        &self.0
    }

    pub fn header(&self) -> VirtioVsockHdr {
        self.0.buf().read_val::<VirtioVsockHdr>().unwrap()
    }

    pub fn payload_len(&self) -> usize {
        self.0.packet_len()
    }

    pub fn payload(&self) -> VmReader<'_, Infallible> {
        self.0.packet()
    }
}
