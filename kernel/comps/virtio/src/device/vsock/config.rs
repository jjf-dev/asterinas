// SPDX-License-Identifier: MPL-2.0

use core::mem::offset_of;

use aster_util::safe_ptr::SafePtr;
use bitflags::bitflags;

use crate::transport::{ConfigManager, VirtioTransport};

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod)]
pub(super) struct VirtioVsockConfig {
    guest_cid_low: u32,
    guest_cid_high: u32,
}

impl VirtioVsockConfig {
    pub(super) fn new_manager(transport: &dyn VirtioTransport) -> ConfigManager<Self> {
        ConfigManager::new(
            transport
                .device_config_mem()
                .map(|memory| SafePtr::new(memory, 0)),
            transport.device_config_bar(),
        )
    }

    pub(super) fn read_guest_cid(config_manager: &ConfigManager<Self>) -> u64 {
        let guest_cid_low = config_manager
            .read_once::<u32>(offset_of!(Self, guest_cid_low))
            .unwrap_or(0);
        let guest_cid_high = config_manager
            .read_once::<u32>(offset_of!(Self, guest_cid_high))
            .unwrap_or(0);
        (u64::from(guest_cid_high) << 32) | u64::from(guest_cid_low)
    }
}

bitflags! {
    pub(super) struct VsockFeatures: u64 {
        const VIRTIO_VSOCK_F_STREAM    = 1 << 0;
        const VIRTIO_VSOCK_F_SEQPACKET = 1 << 1;
    }
}

impl VsockFeatures {
    pub(super) fn supported_features() -> Self {
        Self::VIRTIO_VSOCK_F_STREAM
    }
}
