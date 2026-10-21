# Incident 0017: a buffer the device may still own is released after a failed request

- Opened: 2026-10-21. Status: open, found by reading (not reproduced).
- Found by: the second independent audit of `fix/dma-mapping-lifetime`
  (incident 0004), finding M1. Predates that branch; the branch delays it
  (bounded client-buffer cache) but does not fix it.
- Environment: any; harmful without an IOMMU (the reference laptop has
  none). With an IOMMU the late write becomes an IOMMU fault.

## What the code does

- virtio-blk: `request()` returns an error on timeout and leaves the
  descriptor chain in the ring, and the server closes the client's buffer
  handle anyway (`services/virtio-blk/src/main.rs`, after `dev.request`).
  If the client then closes its own handle and eight later requests evict
  the cached mapping, the buffer's frames return to the PMM while the
  device may still complete the old request and write into them.
- usb-input (xHCI): `slot_disabled` closes `held_buffer` on any Disable
  Slot completion, even when the slot was not released and the controller
  may still use the buffer (`services/usb-input/src/main.rs`, around
  line 1164).

## Plan

1. A driver keeps (quarantines) a buffer whose request it could not
   retire from the device: until the device is reset, the queue is
   reset, or the slot is confirmed released.
2. Record a fault event when a buffer is quarantined this way.
3. Regression: a QEMU scenario with an injected timeout (virtio-blk) and
   the xHCI Disable Slot path, checking the buffer is not released.
