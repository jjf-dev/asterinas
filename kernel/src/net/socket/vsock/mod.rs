// SPDX-License-Identifier: MPL-2.0

mod addr;
mod backend;
mod stream;

pub use addr::VsockSocketAddr;
pub use stream::VsockStreamSocket;

pub(in crate::net) fn init() {
    backend::init();
}
