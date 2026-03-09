# Asterinas Vsock Fix Notes

- Fixed the guest TX path to use the vsock payload length, not `header + payload`,
  when filling `VirtioVsockHdr.len` and when accounting pending/credited bytes.
  Before this fix, the first `RW` packet on an established connection could be
  rejected by `vhost-vsock` with `EINVAL`.

- Fixed `TxBufferBuilder` to allow a packet to fill the DMA segment exactly.
  The previous strict `<` assertion could panic on legal full-size vsock packets
  during the large-transfer test.
