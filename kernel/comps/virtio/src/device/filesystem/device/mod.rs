// SPDX-License-Identifier: MPL-2.0

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{cmp, hint::spin_loop, mem::size_of};

use aster_util::mem_obj_slice::Slice;
use ostd::{
    arch::trap::TrapFrame,
    debug,
    info,
    mm::{VmIo, VmReader, VmWriter, io::util::HasVmReaderWriter},
    sync::{LocalIrqDisabled, SpinLock, Waiter, Waker},
    timer::{Jiffies, TIMER_FREQ},
    warn,
};
use ostd_pod::Pod;
use spin::Once;

use super::{
    DEVICE_NAME,
    config::{FileSystemFeatures, VirtioFsConfig},
    pool::{FsDmaBuf, FsDmaPools},
    protocol::*,
};
use crate::{
    device::VirtioDeviceError,
    id_alloc::SyncIdAlloc,
    queue::{QueueError, VirtQueue},
    transport::{VirtioTransport, VirtioTransportError},
};

const HIPRIO_QUEUE_INDEX: u16 = 0;
const DEFAULT_QUEUE_SIZE: u16 = 128;
const UNIQUE_ID_ALLOC_CAPACITY: usize = 4096;
const REQUEST_WAIT_TIMEOUT_JIFFIES: u64 = 10 * TIMER_FREQ;
const O_RDWR: u32 = 2;

static FILESYSTEM_DEVICES: Once<SpinLock<Vec<Arc<FileSystemDevice>>>> = Once::new();

#[derive(Debug, Clone)]
pub struct VirtioFsDirEntry {
    pub ino: u64,
    pub offset: u64,
    pub type_: u32,
    pub name: String,
}

struct RequestWaitState {
    completed: bool,
    waker: Option<Arc<Waker>>,
}

struct FsRequestQueue {
    queue: SpinLock<VirtQueue, LocalIrqDisabled>,
    pending_requests: SpinLock<BTreeMap<u16, usize>>,
    request_states: SpinLock<BTreeMap<usize, RequestWaitState>>,
}

impl FsRequestQueue {
    fn new(queue: VirtQueue) -> Self {
        Self {
            queue: SpinLock::new(queue),
            pending_requests: SpinLock::new(BTreeMap::new()),
            request_states: SpinLock::new(BTreeMap::new()),
        }
    }
}

impl core::fmt::Debug for FsRequestQueue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FsRequestQueue")
            .field("queue", &self.queue)
            .field(
                "pending_requests_len",
                &self.pending_requests.disable_irq().lock().len(),
            )
            .field(
                "request_states_len",
                &self.request_states.disable_irq().lock().len(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum QueueSelector {
    Hiprio,
    Request(usize),
}

pub struct FileSystemDevice {
    transport: SpinLock<Box<dyn VirtioTransport>, LocalIrqDisabled>,
    hiprio_queue: FsRequestQueue,
    request_queues: Vec<FsRequestQueue>,
    dma_pools: Arc<FsDmaPools>,
    unique_id_alloc: SyncIdAlloc,
    tag: String,
    notify_supported: bool,
}

impl core::fmt::Debug for FileSystemDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FileSystemDevice")
            .field("transport", &self.transport)
            .field("hiprio_queue", &self.hiprio_queue)
            .field("request_queues", &self.request_queues)
            .field("tag", &self.tag)
            .field("notify_supported", &self.notify_supported)
            .finish()
    }
}

mod client;
mod helpers;
mod virtio_ops;

pub fn get_device_by_tag(tag: &str) -> Option<Arc<FileSystemDevice>> {
    let devices = FILESYSTEM_DEVICES.get()?;
    let devices = devices.disable_irq().lock();
    let found = devices.iter().find(|device| device.tag == tag).cloned();
    if found.is_none() {
        let available_tags: Vec<&str> = devices.iter().map(|device| device.tag.as_str()).collect();
        warn!(
            "[virtiofs-debug] device tag '{}' not found; available tags: {:?}",
            tag, available_tags
        );
    }
    found
}

fn config_space_change(_: &TrapFrame) {
    debug!("Virtio-FS device configuration space change");
}

fn map_transport_err(_: VirtioTransportError) -> VirtioDeviceError {
    VirtioDeviceError::QueueUnknownError
}
