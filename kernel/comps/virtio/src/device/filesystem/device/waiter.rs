// SPDX-License-Identifier: MPL-2.0

//! Waitable reply state for submitted FUSE requests.
//!
//! This module defines [`FuseWaiter`], which lets synchronous callers block for
//! a FUSE reply and lets asynchronous users integrate a request into an
//! [`io_util::batch::IoBatch`].

use aster_fuse::{FuseCompletion, FuseError, FuseOperation, FuseStatus, OutHeader};
use io_util::{IoError, batch::IoCompletion};
use ostd::{
    mm::io::util::HasVmReaderWriter,
    sync::{LocalIrqDisabled, SpinLock, WaitQueue},
};
use smallvec::SmallVec;

use crate::device::filesystem::pool::ReplyDmaBuf;

/// Reply buffers owned by one submitted FUSE request.
pub(super) type ReplyBufs = SmallVec<[ReplyDmaBuf; 2]>;

/// A waiter for one submitted FUSE request.
#[must_use]
pub struct FuseWaiter {
    reply_bufs: ReplyBufs,
    status: SpinLock<FuseStatus, LocalIrqDisabled>,
    wait_queue: WaitQueue,
}

impl FuseWaiter {
    /// Creates a waiter for a request's reply buffers.
    pub(super) fn new(reply_bufs: ReplyBufs) -> Self {
        Self {
            reply_bufs,
            status: SpinLock::new(FuseStatus::Pending),
            wait_queue: WaitQueue::new(),
        }
    }

    fn reply_buf(&self) -> Result<&ReplyDmaBuf, FuseError> {
        let Some(reply_buf) = self.reply_bufs().first() else {
            return Err(FuseError::MalformedResponse);
        };
        Ok(reply_buf)
    }

    /// Parses a typed FUSE operation reply from the payload bytes.
    pub(super) fn parse_reply<Op: FuseOperation>(
        &self,
        payload_len: usize,
    ) -> Result<Op::Output, FuseError> {
        let reply_buf = self.reply_buf()?;

        let mut reader = reply_buf.reader().unwrap();
        reader.skip(size_of::<OutHeader>());

        Op::parse_reply(payload_len, &mut reader)
    }

    /// Waits until the FUSE request completes.
    ///
    /// # Locking
    ///
    /// This method may sleep. Callers must not call it while holding a
    /// spinlock, IRQ-disabled guard, or any other lock that cannot be held
    /// across sleep.
    pub(super) fn wait(&self) -> FuseCompletion {
        // FIXME: There is no timeout logic. If the host virtio-fs daemon stalls,
        // the guest driver task will block indefinitely. Adding timeout support
        // is non-trivial: simply dropping the in-flight request is not safe,
        // because the host may still hold descriptors pointing to the guest's
        // DMA buffers. A proper timeout path would require restoring the
        // virtqueue state, which in turn likely necessitates a full device reset.
        self.wait_queue.wait_until(|| {
            let status = *self.status.lock();
            status.has_completed()
        })
    }

    /// Records completion and wakes waiters.
    pub(super) fn wake_completed(&self, completion: FuseCompletion) {
        let should_wake = {
            let mut current_status = self.status.lock();
            if !current_status.is_pending() {
                false
            } else {
                *current_status = FuseStatus::Completed(completion);
                true
            }
        };

        if should_wake {
            self.wait_queue.wake_all();
        }
    }

    /// Returns reply DMA buffers that hold the FUSE reply.
    pub(super) fn reply_bufs(&self) -> &[ReplyDmaBuf] {
        self.reply_bufs.as_slice()
    }
}

impl IoCompletion for FuseWaiter {
    fn wait(&self) -> Result<(), IoError> {
        match self.wait() {
            FuseCompletion::Complete(_) => Ok(()),
            FuseCompletion::MalformedResponse | FuseCompletion::RemoteError(_) => {
                Err(IoError::Failed)
            }
        }
    }
}
