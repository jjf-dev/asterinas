// SPDX-License-Identifier: MPL-2.0

//! `FUSE_LSEEK` computes a file offset for an open file handle.
//!
//! The request body contains [`LseekIn`] with the handle, base offset, and
//! seek mode. The reply body contains [`LseekOut`].

use ostd::mm::{Infallible, VmReader, VmWriter};

use super::util::read_payload;
use crate::{FuseError, FuseFileHandle, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct LseekIn {
    fh: FuseFileHandle,
    /// Base offset for the seek operation.
    offset: i64,
    /// Determines how `offset` is interpreted when computing the new file position.
    whence: u32,
    padding: u32,
}

impl LseekIn {
    pub const fn new(fh: FuseFileHandle, offset: i64, whence: u32) -> Self {
        Self {
            fh,
            offset,
            whence,
            padding: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct LseekOut {
    offset: i64,
}

impl LseekOut {
    /// Returns the file offset computed by the server.
    pub fn offset(&self) -> i64 {
        self.offset
    }
}

pub struct LseekOperation {
    lseek_in: LseekIn,
}

impl LseekOperation {
    pub fn new(lseek_in: LseekIn) -> Self {
        Self { lseek_in }
    }
}

impl FuseOperation for LseekOperation {
    type Output = LseekOut;

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Lseek
    }

    fn body_len(&self) -> usize {
        size_of::<LseekIn>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.lseek_in)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::payload(size_of::<LseekOut>())
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        read_payload(payload_len, reader)
    }
}
