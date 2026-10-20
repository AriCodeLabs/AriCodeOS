# Incident 0004: a CPU mapping of a DMA buffer outlives the buffer

- **Opened:** 2026-10-17. **Status:** fixed on branch
  `fix/dma-mapping-lifetime` (H1 reproduced 2026-10-20, fix and H2 in the
  same branch; ADR 0038), pending audit and merge. FLR not yet exercised on
  the reference laptop.
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

## Reproduction (2026-10-20)

H1 is reproduced by the kernel self-test `test=dma_lifetime` (scenario
`memory.dma_lifetime`, `tests/qemu/memory_dma_lifetime.toml`) with the test
program `dma_holder`:

1. The kernel allocates a 16 KiB DMA buffer (the "client") and hands a
   `READ|WRITE` handle to `dma_holder` (the "server").
2. `dma_holder` maps it (`mmio_map`), closes its handle and writes a magic
   word into it, exactly like the compositor with a window buffer; then it
   sleeps with the mapping in place.
3. The kernel sees the magic word through the physical map and drops its
   own reference: the client's last handle is gone.
4. The debug check reads the server's page tables back through
   `Process::foreign` and asks the PMM bitmap about every mapped frame.

Run on the code of `develop` at `f1cc426` (release 0.12.0) plus the test
alone, `cargo xtask test --level full --area memory`, 1 CPU, QEMU q35 with
the emulated IOMMU (`build/scenarios/memory.dma_lifetime/serial.log`):

```text
[INFO ] requested test: dma_lifetime
[INFO ] user: process 1 'dma_holder' created (1 segments, 4 KiB, entry 0x400000, thread Tid(3))
============================
ARICODEOS KERNEL PANIC
============================
Where: kernel/src/user/process.rs:630:9
Reason: dma lifetime: process 1 still maps 4 of 4 DMA frame(s) the PMM considers free (incident 0004)
CPU: 0
============================
[xtask] 13 QEMU scenarios, 1 failed
xtask: failed scenarios: memory.dma_lifetime
```

All four frames of the buffer are free in the PMM while the server still
maps them writable: any later allocation (a page table, the kernel heap,
another process's DMA buffer) gets memory that `dma_holder` can read and
write. The mechanism described above is confirmed. The scenario is
expected to fail until the fix that follows it on the same branch.

## Normal operation (H1 confirmed)

After the fix, the full suite shows the case outside the self-test: in
`smp.shell_user`, `fsd` (arifs) drops the last mapping of 4 KiB buffers of
`hello_app` runs that had already exited when its client buffer cache
evicts them (`user: process 7 dropped the last mapping of a DMA buffer it
had no handle to (4 KiB at 0x4a7000)`, 15 times). Before the fix those
frames were back in the PMM from the moment each `hello_app` exited while
fsd still mapped them; and arifs mapped past its 8 cached entries without
recording them. H1 happens in normal operation, not only in the test.

## Study and decision

The options (mapping holds the buffer; revocation with a TLB shootdown on
drop; both) were compared in ADR 0038. Chosen: **a foreign mapping of a DMA
buffer holds a reference to it**, plus an explicit `mmio_unmap` so servers
release what they no longer need. Revocation was rejected: the last
reference is often dropped in `reap()` with interrupts disabled, where the
shootdown cannot run, it needs a reverse map, and it would let a client
make its server fault by closing a handle mid-request. The study was done
by one agent reading the kernel, SDK and services (not three independent
reviews as `docs/FAILURE-PROTOCOL.md` asks); the reproduction confirms the
mechanism and the audit step remains.

## Fix (commit 6acfd28)

- `kernel/src/user/process.rs`: `Process::foreign` entries carry the
  buffer's `ObjectRef`; reaping unmaps the entries and only then drops the
  references; `unmap_foreign` (syscall `mmio_unmap`, 36, ABI 5) clears the
  entries, invalidates the range on every CPU (`vmm::flush_user_range`)
  and then drops the reference; `Process::dma_mapped` bounds the DMA a
  process keeps mapped by its quota (`Limit` and a `DmaQuota` event).
- Services: the compositor unmaps a window buffer with its last window;
  virtio-blk, usb-input and usb-ehci map a client buffer for one request
  (`client_buffer::MappedBuffer`); arifs and net-core share
  `client_buffer::ClientBuffers<8>`, which unmaps the oldest mapping.
- H2, `kernel/src/pci.rs` and `release_address_space`: before anything of
  a dead process is freed, every PCI function whose claim it held alone
  clears bus mastering (`stop_claims_of`); after its memory and
  capabilities are released, an unclaimed function that advertises FLR
  (PCIe or Advanced Features capability, `aricode_pci::flr_method`) is
  reset (`reset_released`); the next claim waits out 100 ms and restores
  the BARs (`finish_reset`). Logged on both steps.
- Visibility: `dropped the last mapping of a DMA buffer it had no handle
  to` (once per mapping), `bus mastering cleared before its memory is
  freed`, `function level reset started`, `header restored after the
  function level reset`.

## Tests

- `memory.dma_lifetime` (`test=dma_lifetime`): fails on `c6559a8` (log
  above), passes on `6acfd28`: the frames stay allocated while mapped
  after the last handle, return when the server is reaped and when it
  calls `mmio_unmap` while alive, and `mmio_map` beyond a 4 KiB quota is
  refused.
- `drivers.pci_reap` (`test=pci_reap`, new scenario option `flr_device`
  adds an unused `igb`, which advertises FLR): a driver dies by a fault
  holding a claimed function and a DMA buffer kept only by its mapping.
  The log shows the order, bus mastering first and the frames after:

  ```text
  [WARN ] user: process 2 'pci_reaped' killed: #PF read
  [INFO ] pci: 00:04.0 driver (process 2) gone: bus mastering cleared before its memory is freed (command 0x0407 -> 0x0403)
  [INFO ] user: process 2 dropped the last mapping of a DMA buffer it had no handle to (16 KiB at 0x377000): its frames return to the PMM now
  [INFO ] pci: 00:04.0 function level reset started (its driver is gone)
  [INFO ] pci: 00:04.0 header restored after the function level reset
  [INFO ] pci: 00:04.0 8086:10c9 claimed again (the previous driver died) (MSI-X entry 0)
  ```

  The virtio-blk (no FLR) is checked the same way, and both functions are
  claimed again with the same BARs.
- Host tests: `aricode-pci` (FLR detection, restored header dwords),
  `aricode-handles` (`iter`), `aricode-sys` (`ClientBuffers` bound and
  eviction).
- `cargo xtask check` clean; `cargo xtask test --all`: 160 QEMU scenarios,
  0 failed (bus mastering is now also cleared in `iommu.isolation`,
  `strict.usb_halt`, `strict.usb_hid_halt` and `strict.ehci_halt`, where a
  driver exits); `cargo xtask test --all --cpus 4`: 160 QEMU scenarios, 0 failed.

## Open

- FLR on the reference laptop's functions runs only when a driver dies and
  is restarted; watch the first such restart there (the log lines above,
  then the driver's own start).
- Pending transactions are not waited for before the FLR (bus mastering
  is already off and `reap()` must stay bounded).
- The audit step of the failure protocol (a review agent against this
  incident) before merging.
