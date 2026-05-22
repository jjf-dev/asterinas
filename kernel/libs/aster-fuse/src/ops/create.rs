// SPDX-License-Identifier: MPL-2.0

//! `FUSE_CREATE` creates and opens a regular file in one operation.
//!
//! The request body contains [`CreateIn`] followed by the null-terminated child
//! name under the parent directory node. The reply body contains an [`EntryOut`]
//! for the created inode followed by an [`OpenOut`] for the open file handle.

use ostd::mm::{Infallible, VmReader, VmWriter};

use super::util;
use crate::{
    EntryOut, FuseError, FuseOpcode, FuseOperation, FuseResult, OpenOut, ReplyExpectation,
    ops::open::FuseOpenFlags,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod)]
pub struct CreateIn {
    /// Open flags for the newly created file.
    flags: u32,
    /// File type and permission bits for the new inode.
    mode: u32,
    /// Process umask of the requesting client, applied by the server when creating the inode.
    umask: u32,
    /// FUSE-specific open flags.
    open_flags: FuseOpenFlags,
}

impl CreateIn {
    pub const fn new(flags: u32, mode: u32) -> Self {
        Self {
            flags,
            mode,
            umask: 0,
            open_flags: FuseOpenFlags::empty(),
        }
    }
}

/// The reply contains both the created inode's entry and the open file handle.
pub struct CreateOperation<'a> {
    create_in: CreateIn,
    name: &'a str,
}

impl<'a> CreateOperation<'a> {
    pub fn new(create_in: CreateIn, name: &'a str) -> Self {
        Self { create_in, name }
    }
}

impl FuseOperation for CreateOperation<'_> {
    type Output = (EntryOut, OpenOut);

    fn opcode(&self) -> FuseOpcode {
        FuseOpcode::Create
    }

    fn body_len(&self) -> usize {
        util::name_body_len(size_of::<CreateIn>(), self.name)
    }

    fn write_body(&mut self, writer: &mut VmWriter<'_, Infallible>) -> FuseResult<()> {
        if writer.avail() < self.body_len() {
            return Err(FuseError::BufferTooSmall);
        }

        writer.write_val(&self.create_in).unwrap();
        writer.write(&mut VmReader::from(self.name.as_bytes()));
        writer.write(&mut VmReader::from(util::NAME_TERMINATOR));

        Ok(())
    }

    fn reply_expectation(&self) -> ReplyExpectation {
        ReplyExpectation::payload(size_of::<EntryOut>() + size_of::<OpenOut>())
    }

    fn parse_reply(
        payload_len: usize,
        reader: &mut VmReader<'_, Infallible>,
    ) -> FuseResult<Self::Output> {
        if payload_len < size_of::<EntryOut>() + size_of::<OpenOut>() {
            return Err(FuseError::MalformedResponse);
        }

        let entry_out = reader.read_val().unwrap();
        let open_out = reader.read_val().unwrap();

        Ok((entry_out, open_out))
    }
}
