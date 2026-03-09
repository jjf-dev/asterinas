// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, string::ToString, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use aster_softirq::BottomHalfDisabled;
use ostd::{
    arch::trap::TrapFrame,
    debug,
    sync::{SpinLock, SpinLockGuard},
};
use spin::Once;

use crate::{
    device::{
        VirtioDeviceError,
        vsock::{
            DEVICE_NAME,
            config::{VirtioVsockConfig, VsockFeatures},
            header::VirtioVsockEventId,
            queue::{EventQueue, RxQueue, TxQueue},
        },
    },
    transport::{ConfigManager, VirtioTransport},
};

pub struct VsockDevice {
    config_manager: ConfigManager<VirtioVsockConfig>,
    guest_cid: AtomicU64,
    tx_queue: SpinLock<TxQueue, BottomHalfDisabled>,
    rx_queue: SpinLock<RxQueue, BottomHalfDisabled>,
    rx_callback: Once<fn()>,
    event_queue: SpinLock<EventQueue, BottomHalfDisabled>,
    event_callback: Once<fn()>,
    transport: SpinLock<Box<dyn VirtioTransport>>,
}

impl VsockDevice {
    pub(crate) fn negotiate_features(features: u64) -> u64 {
        (VsockFeatures::from_bits_truncate(features) & VsockFeatures::supported_features()).bits()
    }

    pub(crate) fn init(mut transport: Box<dyn VirtioTransport>) -> Result<(), VirtioDeviceError> {
        let config_manager = VirtioVsockConfig::new_manager(transport.as_ref());
        let guest_cid = VirtioVsockConfig::read_guest_cid(&config_manager);

        let tx_queue = TxQueue::new(transport.as_mut());
        let rx_queue = RxQueue::new(transport.as_mut());
        let event_queue = EventQueue::new(transport.as_mut());

        let device = Arc::new(Self {
            config_manager,
            guest_cid: AtomicU64::new(guest_cid),
            tx_queue: SpinLock::new(tx_queue),
            rx_queue: SpinLock::new(rx_queue),
            rx_callback: Once::new(),
            event_queue: SpinLock::new(event_queue),
            event_callback: Once::new(),
            transport: SpinLock::new(transport),
        });

        let mut transport = device.transport.lock();
        let weak_device = Arc::downgrade(&device);
        transport
            .register_queue_callback(
                RxQueue::QUEUE_INDEX,
                Box::new(move |_: &TrapFrame| super::schedule_rx(&weak_device)),
                true,
            )
            .unwrap();
        let weak_device = Arc::downgrade(&device);
        transport
            .register_queue_callback(
                TxQueue::QUEUE_INDEX,
                Box::new(move |_: &TrapFrame| super::schedule_tx(&weak_device)),
                true,
            )
            .unwrap();
        let weak_device = Arc::downgrade(&device);
        transport
            .register_queue_callback(
                EventQueue::QUEUE_INDEX,
                Box::new(move |_: &TrapFrame| super::schedule_event(&weak_device)),
                true,
            )
            .unwrap();
        transport
            .register_cfg_callback(Box::new(config_space_change))
            .unwrap();
        transport.finish_init();
        drop(transport);

        super::register_device(DEVICE_NAME.to_string(), device);
        Ok(())
    }

    pub fn lock_tx(&self) -> SpinLockGuard<'_, TxQueue, BottomHalfDisabled> {
        self.tx_queue.lock()
    }

    pub fn lock_rx(&self) -> SpinLockGuard<'_, RxQueue, BottomHalfDisabled> {
        self.rx_queue.lock()
    }

    pub fn guest_cid(&self) -> u64 {
        self.guest_cid.load(Ordering::Relaxed)
    }

    pub fn init_rx_callback(&self, callback: fn()) {
        self.rx_callback.call_once(|| callback);
    }

    pub(super) fn process_rx(&self) {
        if let Some(callback) = self.rx_callback.get() {
            (callback)();
        }
    }

    pub fn init_event_callback(&self, callback: fn()) {
        self.event_callback.call_once(|| callback);
    }

    pub(super) fn process_event(&self) {
        let mut event_queue = self.event_queue.lock();

        let Some(event_id) = event_queue.recv() else {
            return;
        };
        match event_id {
            VirtioVsockEventId::TransportReset => (),
        }

        drop(event_queue);

        if let Some(callback) = self.event_callback.get() {
            (callback)();
        }
    }

    pub fn reload_guest_id(&self) {
        let guest_cid = VirtioVsockConfig::read_guest_cid(&self.config_manager);
        self.guest_cid.store(guest_cid, Ordering::Relaxed);
    }
}

fn config_space_change(_: &TrapFrame) {
    debug!("virtio-vsock config change");
}
