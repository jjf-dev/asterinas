// SPDX-License-Identifier: MPL-2.0

use alloc::sync::Arc;

use ostd::mm::{VmReader, VmWriter};

use super::inode::VirtioFsInode;
use crate::{
    events::IoEvents,
    fs::{
        file::{FileIo, SeekFrom, StatusFlags},
        vfs::inode::InodeIo,
    },
    prelude::*,
    process::signal::{PollHandle, Pollable},
};

pub(super) struct VirtioFsHandle {
    inode: Arc<VirtioFsInode>,
    fh: u64,
    flags: u32,
    cache_enabled: bool,
}

impl VirtioFsHandle {
    pub(super) fn new(inode: Arc<VirtioFsInode>, fh: u64, flags: u32, cache_enabled: bool) -> Self {
        Self {
            inode,
            fh,
            flags,
            cache_enabled,
        }
    }
}

impl Drop for VirtioFsHandle {
    fn drop(&mut self) {
        if self.cache_enabled {
            let _ = self.inode.flush_page_cache();
        }
        if let Some(fs) = self.inode.try_fs_ref() {
            let _ = fs
                .device
                .fuse_release(self.inode.nodeid(), self.fh, self.flags);
        }
    }
}

impl Pollable for VirtioFsHandle {
    fn poll(&self, mask: IoEvents, _poller: Option<&mut PollHandle>) -> IoEvents {
        let events = IoEvents::IN | IoEvents::OUT;
        events & mask
    }
}

impl InodeIo for VirtioFsHandle {
    fn read_at(
        &self,
        offset: usize,
        writer: &mut VmWriter,
        _status_flags: StatusFlags,
    ) -> Result<usize> {
        if self.cache_enabled {
            self.inode.cached_read_at(offset, writer)
        } else {
            self.inode.direct_read_at(offset, writer)
        }
    }

    fn write_at(
        &self,
        offset: usize,
        reader: &mut VmReader,
        status_flags: StatusFlags,
    ) -> Result<usize> {
        let offset = if status_flags.contains(StatusFlags::O_APPEND) {
            self.inode.revalidate_attr()?;
            self.inode.size()
        } else {
            offset
        };

        if self.cache_enabled {
            self.inode.cached_write_at(offset, reader)
        } else {
            self.inode.direct_write_at(offset, reader)
        }
    }
}

impl FileIo for VirtioFsHandle {
    fn check_seekable(&self) -> Result<()> {
        Ok(())
    }

    fn is_offset_aware(&self) -> bool {
        true
    }

    fn seek(&self, offset: &Mutex<usize>, pos: SeekFrom) -> Result<usize> {
        const SEEK_SET: u32 = 0;
        const SEEK_CUR: u32 = 1;
        const SEEK_END: u32 = 2;

        let (in_offset, whence) = match pos {
            SeekFrom::Start(off) => (
                i64::try_from(off)
                    .map_err(|_| Error::with_message(Errno::EOVERFLOW, "offset too large"))?,
                SEEK_SET,
            ),
            SeekFrom::Current(diff) => (diff as i64, SEEK_CUR),
            SeekFrom::End(diff) => (diff as i64, SEEK_END),
        };

        let fs = self.inode.fs_ref();
        let new_offset = fs
            .device
            .fuse_lseek(self.inode.nodeid(), self.fh, in_offset, whence)
            .map_err(Error::from)?;

        if new_offset < 0 {
            return_errno_with_message!(Errno::EINVAL, "seek returned negative offset")
        }

        let new_offset = usize::try_from(new_offset)
            .map_err(|_| Error::with_message(Errno::EOVERFLOW, "seek result too large"))?;
        *offset.lock() = new_offset;
        Ok(new_offset)
    }
}
