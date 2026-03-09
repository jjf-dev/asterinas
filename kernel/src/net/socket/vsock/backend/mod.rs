// SPDX-License-Identifier: MPL-2.0

mod connection;
mod listener;
mod port;
mod space;
mod timer;

use core::time::Duration;

pub(super) use connection::{ConnectResult, Connection};
pub(super) use listener::Listener;
pub(super) use port::BoundPort;

// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/net/vmw_vsock/af_vsock.c#L136>
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/net/vmw_vsock/virtio_transport_common.c#L82>
const DEFAULT_CLOSE_TIMEOUT: Duration = Duration::from_secs(8);
// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/net/vmw_vsock/af_vsock.c#L137>
const DEFAULT_RX_BUF_SIZE: usize = 256 * 1024;
// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/net/vmw_vsock/af_vsock.c#L138>
const DEFAULT_PENDING_TX_BYTES: usize = 256 * 1024;
// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/include/linux/socket.h#L313>
const MAX_BACKLOG: usize = 4096;
// Reference: <https://elixir.bootlin.com/linux/v6.16.8/source/include/linux/virtio_vsock.h#L114>
const VIRTIO_VSOCK_MAX_PKT_BUF_SIZE: usize = 64 * 1024;

const CREDIT_UPDATE_THRESHOLD: u32 = if DEFAULT_RX_BUF_SIZE / 4 < VIRTIO_VSOCK_MAX_PKT_BUF_SIZE {
    (DEFAULT_RX_BUF_SIZE / 4) as u32
} else {
    VIRTIO_VSOCK_MAX_PKT_BUF_SIZE as u32
};

fn process_rx_callback() {
    if let Ok(vsock_space) = space::vsock_space() {
        vsock_space.process_rx();
    }
}

fn process_event_callback() {
    if let Ok(vsock_space) = space::vsock_space() {
        vsock_space.process_transport_event();
    }
}

pub(super) fn init() {
    use aster_virtio::device::vsock::DEVICE_NAME;

    let Some(device) = aster_virtio::device::vsock::get_device(DEVICE_NAME) else {
        return;
    };

    device.init_rx_callback(process_rx_callback);
    device.init_event_callback(process_event_callback);
    space::init(device);

    timer::init();
}
