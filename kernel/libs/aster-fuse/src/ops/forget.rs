// SPDX-License-Identifier: MPL-2.0

//! `FUSE_FORGET` releases lookup references held for an inode.
//!
//! The request body contains [`ForgetIn`] for the inode named by the request
//! header. `FUSE_FORGET` is a one-way notification; the server sends no reply.

use ostd::mm::{Infallible, VmReader, VmWriter};

use crate::{FuseError, FuseOpcode, FuseOperation, FuseResult, ReplyExpectation};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct ForgetIn {
    /// Number of lookup references being released.
    nlookup: u64,
}

impl ForgetIn {
    pub const fn new(nlookup: u64) -> Self {
        Self { nlookup }
    }
}

pub struct ForgetOperation {
    forget_in: ForgetIn,
}

impl ForgetOperation {
    pub fn new(forget_in: ForgetIn) -> Self {
        Self { forget_in }
    }
}

impl FuseOperation for ForgetOperation {
    type Output = ();

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Forget
    }

    fn body_len(&self) -> usize {
        size_of::<ForgetIn>()
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        writer
            .write_val(&self.forget_in)
            .map_err(|_| FuseError::BufferTooSmall)
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::None
    }

    fn parse_reply(
        _payload_len: usize,
        _reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        Ok(())
    }
}
