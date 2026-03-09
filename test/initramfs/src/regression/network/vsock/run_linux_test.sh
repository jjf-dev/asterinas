#!/bin/sh

# SPDX-License-Identifier: MPL-2.0

set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BUILD_DIR=${BUILD_DIR:-"${SCRIPT_DIR}/../../../../build/vsock_linux"}
BIN_DIR="${BUILD_DIR}/initramfs/test/network/vsock"
HOST_BIN="${BIN_DIR}/vsock_test_host"
GUEST_BIN="${BIN_DIR}/vsock_err"

make --no-print-directory -C "${SCRIPT_DIR}" TEST_PLATFORM=linux \
	BUILD_DIR="${BUILD_DIR}"

"${HOST_BIN}" --bind-cid="${VSOCK_TEST_BIND_CID:-1}" \
	--connect-cid="${VSOCK_TEST_CONNECT_CID:-1}" &
HOST_PID=$!
trap 'kill "${HOST_PID}" 2>/dev/null || true; wait "${HOST_PID}" 2>/dev/null || true' EXIT

sleep 0.2
VSOCK_TEST_PEER_CID="${VSOCK_TEST_PEER_CID:-1}" "${GUEST_BIN}"

wait "${HOST_PID}"
trap - EXIT
