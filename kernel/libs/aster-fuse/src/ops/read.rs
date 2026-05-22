// SPDX-License-Identifier: MPL-2.0

//! `FUSE_READ` reads bytes from an open file handle.
//!
//! The request body contains [`ReadIn`] with the handle, offset, and maximum
//! byte count. The reply body is raw file data, and the operation returns the
//! bytes actually provided by the server.

use bitflags::bitflags;
use ostd::mm::{Infallible, VmReader, VmWriter};

use crate::{FuseError, FuseFileHandle, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct ReadIn {
    /// File handle to read from.
    fh: FuseFileHandle,
    /// File or directory offset to start reading from.
    offset: u64,
    /// Maximum number of bytes to read.
    size: u32,
    /// FUSE-specific read flags.
    read_flags: ReadFlags,
    /// Lock owner for lock-aware reads.
    lock_owner: u64,
    /// POSIX open flags associated with the handle.
    flags: u32,
    padding: u32,
}

impl ReadIn {
    pub const fn new(fh: FuseFileHandle, offset: u64, size: u32, flags: u32) -> Self {
        Self {
            fh,
            offset,
            size,
            read_flags: ReadFlags::empty(),
            lock_owner: 0,
            flags,
            padding: 0,
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }
}

bitflags! {
    /// Flags for `FUSE_READ` and `FUSE_READDIR` requests.
    ///
    /// Reference: <https://elixir.bootlin.com/linux/v6.18/source/include/uapi/linux/fuse.h#L536-L539>
    #[repr(C)]
    #[derive(Pod)]
    pub struct ReadFlags: u32 {
        /// `lock_owner` is valid and should be used for lock-aware reads.
        const READ_LOCKOWNER = 1 << 1;
    }
}

pub struct ReadOperation {
    read_in: ReadIn,
}

impl ReadOperation {
    pub fn new(read_in: ReadIn) -> Self {
        Self { read_in }
    }
}

impl FuseOperation for ReadOperation {
    type Output = usize;

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Read
    }

    fn body_len(&self) -> usize {
        size_of::<ReadIn>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.read_in)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::payload(self.read_in.size() as usize)
    }

    fn parse_reply(
        payload_len: usize,
        _reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        Ok(payload_len)
    }
}
