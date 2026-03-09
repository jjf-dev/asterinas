// SPDX-License-Identifier: MPL-2.0

use alloc::sync::Arc;
use core::marker::PhantomData;

use ostd::{
    Result,
    mm::{
        Daddr, HasDaddr, HasSize, Infallible, VmReader, VmWriter,
        dma::{FromDevice, ToDevice},
    },
};
use ostd_pod::Pod;

use crate::dma_pool::{DmaPool, DmaSegment};

pub struct TxBuffer {
    segment: DmaSegment<ToDevice>,
    nbytes: usize,
}

impl TxBuffer {
    pub fn new<H: Pod>(header: &H, packet: &[u8], pool: &Arc<DmaPool<ToDevice>>) -> Result<Self> {
        let mut builder = Self::new_builder::<H>(pool)?;

        builder
            .append(|mut writer| {
                assert!(writer.avail() >= packet.len());
                Ok(writer.write(&mut VmReader::from(packet)))
            })
            .unwrap();

        Ok(builder.build(header))
    }

    pub fn new_builder<H: Pod>(pool: &Arc<DmaPool<ToDevice>>) -> Result<TxBufferBuilder<H>> {
        assert!(size_of::<H>() <= pool.segment_size());

        let segment = pool.alloc_segment()?;

        let builder = TxBufferBuilder {
            segment,
            nbytes: size_of::<H>(),
            _phantom: PhantomData,
        };
        Ok(builder)
    }

    fn sync_to_device(&self) {
        self.segment.sync_to_device(0..self.nbytes).unwrap();
    }
}

impl HasSize for TxBuffer {
    fn size(&self) -> usize {
        self.nbytes
    }
}

impl HasDaddr for TxBuffer {
    fn daddr(&self) -> Daddr {
        self.segment.daddr()
    }
}

pub struct TxBufferBuilder<H> {
    segment: DmaSegment<ToDevice>,
    nbytes: usize,
    _phantom: PhantomData<H>,
}

impl<H: Pod> TxBufferBuilder<H> {
    pub fn append<F>(&mut self, f: F) -> Result<usize>
    where
        F: FnOnce(VmWriter<Infallible>) -> Result<usize>,
    {
        let mut writer = self.segment.writer().unwrap();
        writer.skip(self.nbytes);

        let bytes_written = f(writer)?;
        self.nbytes += bytes_written;
        debug_assert!(self.nbytes <= self.segment.size());

        Ok(bytes_written)
    }

    pub const fn packet_len(&self) -> usize {
        self.nbytes - size_of::<H>()
    }

    pub fn build(self, header: &H) -> TxBuffer {
        self.segment
            .writer()
            .unwrap()
            .write(&mut VmReader::from(header.as_bytes()));

        let tx_buffer = TxBuffer {
            segment: self.segment,
            nbytes: self.nbytes,
        };
        tx_buffer.sync_to_device();
        tx_buffer
    }
}

pub struct RxBuffer {
    segment: DmaSegment<FromDevice>,
    header_len: usize,
    packet_len: usize,
}

impl RxBuffer {
    pub fn new(header_len: usize, pool: &Arc<DmaPool<FromDevice>>) -> Result<Self> {
        assert!(header_len <= pool.segment_size());

        let segment = pool.alloc_segment()?;
        Ok(Self {
            segment,
            header_len,
            packet_len: 0,
        })
    }

    pub const fn packet_len(&self) -> usize {
        self.packet_len
    }

    pub fn set_packet_len(&mut self, packet_len: usize) {
        assert!(self.header_len.checked_add(packet_len).unwrap() <= self.segment.size());
        self.packet_len = packet_len;
    }

    pub fn packet(&self) -> VmReader<'_, Infallible> {
        self.segment
            .sync_from_device(self.header_len..self.header_len + self.packet_len)
            .unwrap();

        let mut reader = self.segment.reader().unwrap();
        reader.skip(self.header_len).limit(self.packet_len);
        reader
    }

    pub fn buf(&self) -> VmReader<'_, Infallible> {
        self.segment
            .sync_from_device(0..self.header_len + self.packet_len)
            .unwrap();

        let mut reader = self.segment.reader().unwrap();
        reader.limit(self.header_len + self.packet_len);
        reader
    }
}

impl HasSize for RxBuffer {
    fn size(&self) -> usize {
        self.segment.size()
    }
}

impl HasDaddr for RxBuffer {
    fn daddr(&self) -> Daddr {
        self.segment.daddr()
    }
}
