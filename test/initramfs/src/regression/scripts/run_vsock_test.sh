#!/bin/sh

# SPDX-License-Identifier: MPL-2.0

# To successfully run the vsock test, you should run `vsock_test_host` on the
# Linux host before booting Asterinas. The helper should use
# `--bind-cid=2 --connect-cid=3` so that guest-to-host and host-to-guest tests
# both hit the expected CIDs.

set -e

VSOCK_DIR=/test/network/vsock
cd ${VSOCK_DIR}

echo "Start vsock test......"
VSOCK_TEST_PEER_CID=${VSOCK_TEST_PEER_CID:-2} ./vsock_err
echo "Vsock test passed."
