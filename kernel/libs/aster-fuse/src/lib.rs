// SPDX-License-Identifier: MPL-2.0 AND BSD-3-Clause
//
// This file is partially derived from the virtiofsd project:
// https://gitlab.com/virtio-fs/virtiofsd/-/blob/main/src/fuse.rs
//
// Original source:
// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Licensed under the BSD-3-Clause license.
//
// Modifications made by The Asterinas Authors are licensed under MPL-2.0.
// Copyright 2026-present The Asterinas Authors.

//! Provides FUSE protocol definitions shared by in-kernel clients.
//!
//! This crate contains the transport-independent on-wire pieces of the FUSE
//! protocol: request and reply headers, payload layouts, opcodes, flags, and
//! common constants.
//!
//! The main entry points are:
//!
//! - [`FuseOperation`], which describes one typed FUSE request/reply pair.
//! - [`FuseError`] and [`FuseResult`], which report encoding and decoding
//!   failures.
//! - POD-compatible protocol structs such as [`InHeader`] and [`OutHeader`].
//! - Per-operation request and reply structs under [`mod@ops`].
#![no_std]
#![deny(unsafe_code)]

extern crate alloc;
#[macro_use]
extern crate ostd_pod;

mod attr;
mod dirent;
mod error;
mod header;
mod ids;
mod operation;
pub mod ops;
mod status;

pub use self::{
    attr::{Attr, EntryOut},
    dirent::{DirOffset, Dirent, DirentType, FuseDirEntry},
    error::{FuseError, FuseResult},
    header::{InHeader, OutHeader},
    ids::{FuseFileHandle, FuseGeneration, FuseNodeId, FuseUnique, LookupCount},
    operation::{FuseOpcode, FuseOperation, ReplyExpectation},
    ops::{
        create::{CreateIn, CreateOperation},
        forget::{ForgetIn, ForgetOperation},
        getattr::{FuseAttrOut, GetattrFlags, GetattrIn, GetattrOperation},
        init::{FuseInitFlags, FuseInitFlags2, InitIn, InitOperation, InitOut},
        link::{LinkIn, LinkOperation},
        lookup::LookupOperation,
        lseek::{LseekIn, LseekOperation, LseekOut},
        mkdir::{MkdirIn, MkdirOperation},
        mknod::{MknodIn, MknodOperation},
        open::{FuseOpenFlags, OpenIn, OpenOperation, OpenOut, OpendirOperation},
        read::{ReadIn, ReadOperation},
        readdir::ReaddirOperation,
        readlink::{MAX_READLINK_LEN, ReadlinkOperation},
        release::{ReleaseFlags, ReleaseIn, ReleaseKind, ReleaseOperation},
        rmdir::RmdirOperation,
        setattr::{SetattrIn, SetattrOperation, SetattrValid},
        statfs::{Kstatfs, StatfsOperation, StatfsOut},
        unlink::UnlinkOperation,
        write::{WriteFlags, WriteIn, WriteOperation, WriteOut},
    },
    status::{FuseCompleteFn, FuseCompletion, FuseStatus},
};

/// The root inode ID used by the FUSE protocol.
pub const FUSE_ROOT_ID: FuseNodeId = FuseNodeId::new(1);

/// The major FUSE protocol version supported by this crate.
pub const FUSE_KERNEL_VERSION: u32 = 7;

/// The minor FUSE protocol version supported by this crate.
pub const FUSE_KERNEL_MINOR_VERSION: u32 = 38;

/// Minimum `max_write` value enforced by the client.
///
/// Even if the daemon reports a smaller `max_write` in `FUSE_INIT`, the client
/// uses at least one page (4096 bytes) per write request.
pub const MIN_MAX_WRITE: u32 = 4096;
