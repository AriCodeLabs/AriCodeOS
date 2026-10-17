# Incident 0004: a CPU mapping of a DMA buffer outlives the buffer

- **Opened:** 2026-10-17. **Status:** open (found by review, not yet
  reproduced).
- **Found by:** robustness audit 1 (`docs/audits/robustness-audit-1.md`,
  finding M4), while reviewing branch `fix/robustness-incidents`. The
  defect predates that branch; the branch does not change it and does not
  fix it.
- **Environment:** any; worst without an IOMMU (the reference laptop has
  none).

## What the code does (evidence)

1. `kernel/src/user/process.rs`, `map_foreign` (around line 651): maps the
   physical range of an MMIO or DMA object into the calling process and
   records only `(virt, start, span)` in `Process::foreign`. It keeps no
   reference to the `DmaBuffer` object. The mapping stays until the process
   is reaped (`release_address_space` unmaps `foreign` without freeing).
2. `kernel/src/hw.rs`, `impl Drop for DmaBuffer` (around line 249): when
   the last reference to a DMA object goes, the range is removed from every
   IOMMU domain (`iommu::unmap_everywhere`) and its frames go back to the
   PMM. CPU page tables of other processes that mapped the range are not
   touched.
3. `services/compositor/src/main.rs` (around line 407): after a window
   request the compositor closes the client's buffer handle and keeps the
   mapping ("The content buffer stays mapped; the handle is no longer
   needed"). Servers do the same through `sdk/rust/src/block.rs`
   `map_client_buffer` (around line 135; virtio-blk, usb-input): the
   mapping is reused per request and outlives the handle.
4. Consequence: when the client closes its own handle (or exits), the
   `DmaBuffer` is dropped, its frames return to the PMM and can be handed
   to anyone (another process's page tables, the kernel heap, another DMA
   buffer), while the server still has them mapped writable. A server that
   writes into a stale buffer, or reads it, touches someone else's memory:
   a physical use-after-free across protection domains.
5. `kernel/src/pci.rs`, `claim` (around line 284) enables memory space and
   bus mastering on the device; nothing clears bus mastering when the
   driver dies. With an IOMMU the dead driver's domain is withdrawn at
   reap; without one the device can keep writing to physical addresses
   programmed by the dead driver, including freed frames.

## Hypotheses to confirm

- H1 A server mapping outlives a client buffer in normal operation
  (compositor windows closed by a client that exits; a block client that
  exits after a request), so the frames are reused while still mapped.
- H2 Without an IOMMU, a driver that dies with DMA in flight leaves the
  device writing to frames that are freed and reused.

## Plan (failure protocol)

1. **Reproduce.** A test program pair: a client allocates a DMA buffer,
   has a server map it, closes its handle and exits; the kernel self-test
   then checks whether the server's page tables still map frames that the
   PMM considers free (a debug check over `Process::foreign` against the
   PMM bitmap). A scenario with the compositor: open and close a window
   from a program that exits, then allocate memory until the frames are
   reused.
2. **Study.** Three independent reviews: ownership of DMA objects and
   foreign mappings in the kernel; the IOMMU and bus-master lifecycle on
   driver death (PCI command register, FLR where available); the SDK and
   services that map client buffers (compositor, virtio-blk, usb-input,
   fsd).
3. **Fix candidates** (to choose after the study): `foreign` entries hold
   the `ObjectRef` of the DMA buffer so frames are freed only after every
   mapping is gone; or `DmaBuffer::drop` unmaps every CPU mapping (with a
   TLB shootdown) like it does for IOMMU domains; clear bus mastering (and
   reset the function where possible) when a claimed device's driver is
   reaped.
4. **Regression.** The reproduction above becomes a scenario that fails
   before the fix and passes after.

## Open

Everything: no reproduction yet. Not fixed in `fix/robustness-incidents`.
