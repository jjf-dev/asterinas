// SPDX-License-Identifier: MPL-2.0

use alloc::collections::btree_map::BTreeMap;

use crate::{
    error::{Errno, Error},
    net::socket::vsock::{
        addr::{VMADDR_CID_ANY, VMADDR_PORT_ANY, VsockSocketAddr},
        backend::{
            Connection, Listener,
            space::{VsockSpace, vsock_space},
        },
    },
    prelude::{Result, return_errno_with_message},
    process::signal::Pollee,
};

/// Represents one ownership of a bound local vsock port.
#[derive(Debug)]
pub(in crate::net::socket::vsock) struct BoundPort {
    port: u32,
}

pub(super) struct PortTable {
    next_ephemeral_port: u32,
    usage: BTreeMap<u32, usize>,
}

impl PortTable {
    const EPHEMERAL_PORT_START: u32 = 49152;

    pub(super) fn new() -> Self {
        Self {
            next_ephemeral_port: Self::EPHEMERAL_PORT_START,
            usage: BTreeMap::new(),
        }
    }

    fn next_ephemeral_port_after(port: u32) -> u32 {
        let mut next_port = if port == u32::MAX {
            Self::EPHEMERAL_PORT_START
        } else {
            port + 1
        };
        if next_port < Self::EPHEMERAL_PORT_START || next_port == VMADDR_PORT_ANY {
            next_port = Self::EPHEMERAL_PORT_START;
        }
        next_port
    }
}

impl BoundPort {
    pub(in crate::net::socket::vsock) fn new_bind(addr: VsockSocketAddr) -> Result<Self> {
        let vsock_space = vsock_space()?;

        let guest_cid = vsock_space.guest_cid();
        if addr.cid != VMADDR_CID_ANY && addr.cid as u64 != guest_cid {
            return_errno_with_message!(Errno::EADDRNOTAVAIL, "the vsock cid is not local");
        }

        if addr.port == VMADDR_PORT_ANY {
            return Self::new_ephemeral();
        }

        let mut ports = vsock_space.lock_ports();
        let usage = ports.usage.entry(addr.port).or_insert(0);
        if *usage != 0 {
            return_errno_with_message!(Errno::EADDRINUSE, "the vsock port is already in use");
        }
        *usage += 1;
        Ok(Self { port: addr.port })
    }

    pub(in crate::net::socket::vsock) fn new_ephemeral() -> Result<Self> {
        let vsock_space = vsock_space()?;
        let mut ports = vsock_space.lock_ports();

        let start_port = ports.next_ephemeral_port;
        let mut current_port = start_port;

        loop {
            let usage = ports.usage.entry(current_port).or_insert(0);
            if *usage == 0 {
                *usage += 1;
                ports.next_ephemeral_port = PortTable::next_ephemeral_port_after(current_port);
                return Ok(Self { port: current_port });
            }

            current_port = PortTable::next_ephemeral_port_after(current_port);
            if current_port == start_port {
                return_errno_with_message!(
                    Errno::EADDRINUSE,
                    "no ephemeral vsock ports are available"
                );
            }
        }
    }

    pub(super) fn new_shared(bound_port: &BoundPort) -> BoundPort {
        let vsock_space = bound_port.vsock_space();

        let mut ports = vsock_space.lock_ports();
        let usage = ports.usage.entry(bound_port.port).or_insert(0);
        *usage += 1;
        BoundPort {
            port: bound_port.port,
        }
    }

    pub(in crate::net::socket::vsock) fn connect(
        self,
        remote_addr: VsockSocketAddr,
        pollee: &Pollee,
    ) -> core::result::Result<Connection, (Error, BoundPort)> {
        let vsock_space = self.vsock_space();
        vsock_space.new_connection(self, remote_addr, pollee)
    }

    pub(in crate::net::socket::vsock) fn listen(
        self,
        backlog: usize,
        pollee: &Pollee,
    ) -> core::result::Result<Listener, (Error, BoundPort)> {
        let vsock_space = self.vsock_space();
        vsock_space.new_listener(self, backlog, pollee)
    }

    pub(in crate::net::socket::vsock) fn local_addr(&self) -> VsockSocketAddr {
        VsockSocketAddr {
            cid: self.vsock_space().guest_cid() as u32,
            port: self.port,
        }
    }

    pub(super) fn vsock_space(&self) -> &'static VsockSpace {
        vsock_space().unwrap()
    }

    pub(super) const fn port(&self) -> u32 {
        self.port
    }
}

impl Drop for BoundPort {
    fn drop(&mut self) {
        use alloc::collections::btree_map::Entry;

        let mut ports = self.vsock_space().lock_ports();
        let Entry::Occupied(mut usage) = ports.usage.entry(self.port) else {
            return;
        };
        *usage.get_mut() -= 1;
        if *usage.get() == 0 {
            usage.remove();
        }
    }
}
