// SPDX-License-Identifier: MPL-2.0

use super::*;

impl FileSystemDevice {
    pub(super) fn parse_tag(raw_tag: &[u8; 36]) -> String {
        let len = raw_tag
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(raw_tag.len());

        match core::str::from_utf8(&raw_tag[..len]) {
            Ok(tag) => tag.to_string(),
            Err(_) => "<invalid-tag>".to_string(),
        }
    }

    pub(super) fn alloc_unique(&self) -> u64 {
        self.unique_id_alloc.alloc() as u64
    }

    fn alloc_to_device_buf(&self, size: usize) -> FsDmaBuf {
        self.dma_pools.alloc_to_device(size)
    }

    fn alloc_from_device_buf(&self, size: usize) -> FsDmaBuf {
        self.dma_pools.alloc_from_device(size)
    }

    fn write_cstr_to_buf(&self, buf: &FsDmaBuf, value: &str) {
        let name_slice = Slice::new(buf.clone(), 0..(value.len() + 1));
        {
            let mut writer = buf.writer().unwrap();
            let mut value_reader = VmReader::from(value.as_bytes());
            let _ = writer.write(&mut value_reader);
            let nul: [u8; 1] = [0u8];
            let mut nul_reader = VmReader::from(&nul[..]);
            let _ = writer.write(&mut nul_reader);
        }
        name_slice
            .mem_obj()
            .sync_to_device(name_slice.offset().clone())
            .unwrap();
    }

    pub(super) fn prepare_in_header_buf(&self, in_header: InHeader) -> Slice<FsDmaBuf> {
        let in_header_buf = self.alloc_to_device_buf(size_of::<InHeader>());
        let in_header_slice = Slice::new(in_header_buf.clone(), 0..size_of::<InHeader>());
        in_header_slice.write_val(0, &in_header).unwrap();
        in_header_slice
            .mem_obj()
            .sync_to_device(in_header_slice.offset().clone())
            .unwrap();
        in_header_slice
    }

    pub(super) fn prepare_in_payload_buf<T: Pod>(&self, in_payload: T) -> Slice<FsDmaBuf> {
        let in_payload_buf = self.alloc_to_device_buf(size_of::<T>());
        let in_payload_slice = Slice::new(in_payload_buf.clone(), 0..size_of::<T>());
        in_payload_slice.write_val(0, &in_payload).unwrap();
        in_payload_slice
            .mem_obj()
            .sync_to_device(in_payload_slice.offset().clone())
            .unwrap();
        in_payload_slice
    }

    pub(super) fn prepare_in_name_buf(&self, name: &str) -> Slice<FsDmaBuf> {
        let in_name_buf = self.alloc_to_device_buf(name.len() + 1);
        let in_name_slice = Slice::new(in_name_buf.clone(), 0..(name.len() + 1));
        self.write_cstr_to_buf(&in_name_buf, name);
        in_name_slice
    }

    pub(super) fn prepare_in_data_buf(&self, data: &[u8]) -> Slice<FsDmaBuf> {
        let in_data_buf = self.alloc_to_device_buf(data.len());
        let in_data_slice = Slice::new(in_data_buf.clone(), 0..data.len());
        {
            let mut writer = in_data_buf.writer().unwrap();
            let mut data_reader = VmReader::from(data);
            let _ = writer.write(&mut data_reader);
        }
        in_data_slice
            .mem_obj()
            .sync_to_device(in_data_slice.offset().clone())
            .unwrap();
        in_data_slice
    }

    pub(super) fn prepare_out_header_buf(&self) -> Slice<FsDmaBuf> {
        let out_header_buf = self.alloc_from_device_buf(size_of::<OutHeader>());
        Slice::new(out_header_buf.clone(), 0..size_of::<OutHeader>())
    }

    pub(super) fn prepare_out_payload_buf(&self, size: usize) -> Slice<FsDmaBuf> {
        let out_payload_buf = self.alloc_from_device_buf(size);
        Slice::new(out_payload_buf.clone(), 0..size)
    }

    pub(super) fn prepare_request_slices<T: Pod>(
        &self,
        in_header: InHeader,
        in_payload: T,
        out_payload_size: usize,
    ) -> (
        Slice<FsDmaBuf>,
        Slice<FsDmaBuf>,
        Slice<FsDmaBuf>,
        Slice<FsDmaBuf>,
    ) {
        let in_header_slice = self.prepare_in_header_buf(in_header);
        let in_payload_slice = self.prepare_in_payload_buf(in_payload);
        let out_header_slice = self.prepare_out_header_buf();
        let out_payload_buf = self.alloc_from_device_buf(out_payload_size);
        let out_payload_slice = Slice::new(out_payload_buf.clone(), 0..out_payload_size);
        (
            in_header_slice,
            in_payload_slice,
            out_header_slice,
            out_payload_slice,
        )
    }
}
