// SPDX-License-Identifier: MPL-2.0

mod buffer;
mod config;
pub mod device;
pub mod header;
pub mod packet;
pub mod queue;

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};

use aster_softirq::Taskless;
use ostd::sync::{LocalIrqDisabled, SpinLock};
use spin::Once;

use crate::device::vsock::device::VsockDevice;

pub const DEVICE_NAME: &str = "Virtio-Vsock";

struct Component {
    devices: SpinLock<BTreeMap<String, Arc<VsockDevice>>>,
    rx_pending: SpinLock<Vec<Weak<VsockDevice>>, LocalIrqDisabled>,
    tx_pending: SpinLock<Vec<Weak<VsockDevice>>, LocalIrqDisabled>,
    event_pending: SpinLock<Vec<Weak<VsockDevice>>, LocalIrqDisabled>,
    rx_taskless: Arc<Taskless>,
    tx_taskless: Arc<Taskless>,
    event_taskless: Arc<Taskless>,
}

static COMPONENT: Once<Component> = Once::new();

pub(crate) fn init() {
    buffer::init();
    COMPONENT.call_once(|| Component {
        devices: SpinLock::new(BTreeMap::new()),
        rx_pending: SpinLock::new(Vec::new()),
        tx_pending: SpinLock::new(Vec::new()),
        event_pending: SpinLock::new(Vec::new()),
        rx_taskless: Taskless::new(process_pending_rx),
        tx_taskless: Taskless::new(process_pending_tx),
        event_taskless: Taskless::new(process_pending_event),
    });
}

pub fn register_device(name: String, device: Arc<VsockDevice>) {
    let component = COMPONENT.get().unwrap();
    component.devices.lock().insert(name, device);
}

pub fn get_device(name: &str) -> Option<Arc<VsockDevice>> {
    let component = COMPONENT.get().unwrap();
    component.devices.lock().get(name).cloned()
}

pub fn all_devices() -> Vec<(String, Arc<VsockDevice>)> {
    let component = COMPONENT.get().unwrap();
    component
        .devices
        .lock()
        .iter()
        .map(|(name, device)| (name.clone(), device.clone()))
        .collect()
}

fn schedule_rx(device: &Weak<VsockDevice>) {
    let component = COMPONENT.get().unwrap();
    component.rx_pending.lock().push(device.clone());
    component.rx_taskless.schedule();
}

fn schedule_tx(device: &Weak<VsockDevice>) {
    let component = COMPONENT.get().unwrap();
    component.tx_pending.lock().push(device.clone());
    component.tx_taskless.schedule();
}

fn schedule_event(device: &Weak<VsockDevice>) {
    let component = COMPONENT.get().unwrap();
    component.event_pending.lock().push(device.clone());
    component.event_taskless.schedule();
}

fn process_pending_rx() {
    let component = COMPONENT.get().unwrap();
    let devices = take_pending(&component.rx_pending);

    for device in devices {
        if let Some(device) = device.upgrade() {
            device.process_rx();
        }
    }
}

fn process_pending_tx() {
    let component = COMPONENT.get().unwrap();
    let devices = take_pending(&component.tx_pending);

    for device in devices {
        if let Some(device) = device.upgrade() {
            device.lock_tx().free_processed_tx_buffers();
        }
    }
}

fn process_pending_event() {
    let component = COMPONENT.get().unwrap();
    let devices = take_pending(&component.event_pending);

    for device in devices {
        if let Some(device) = device.upgrade() {
            device.process_event();
        }
    }
}

fn take_pending(
    pending: &SpinLock<Vec<Weak<VsockDevice>>, LocalIrqDisabled>,
) -> Vec<Weak<VsockDevice>> {
    let mut pending = pending.lock();
    core::mem::take(&mut *pending)
}
