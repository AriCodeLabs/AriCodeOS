# Incident 0004: a CPU mapping of a DMA buffer outlives the buffer

- **Opened:** 2026-10-17. **Status:** fixed on branch
  `fix/dma-mapping-lifetime` (H1 reproduced 2026-10-20, fix and H2 in the
  same branch; ADR 0038), audited twice (2026-10-21) and the findings
  fixed; pending merge. Not yet exercised on the reference laptop.
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
  entries, invalidates the range on every CPU (`vmm::flush_user_range`;
  revised after the audit: own CPU only, see "Audit") and then drops the
  reference; `Process::dma_mapped` bounds the DMA a
  process keeps mapped by its quota (`Limit` and a `DmaQuota` event).
- Services: the compositor unmaps a window buffer with its last window;
  virtio-blk, usb-input and usb-ehci map a client buffer for one request
  (`client_buffer::MappedBuffer`; replaced by the cache after the
  audit); arifs and net-core share
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

## Audit (2026-10-21)

Two independent audits of `537759f` verified the ordering, the lock order,
the TLB handling and the FLR detection, and found two highs, which are
fixed in commit `ec2f89f` (details and the full list in ADR 0038,
"Audit"):

- **H1, high: unbounded page-table growth.** `mmio_next` only grew and
  `unmap_4k` never frees tables: every map/unmap cycle (the block servers
  mapped per request) took new addresses and kept one page-table frame per
  2 MiB crossed, about 8 MiB per million block requests in drivers
  without a quota; an application cycling 9+ buffers through fsd made its
  cache miss every time, with a whole-TLB IPI per miss. Fix: the mapping
  area reuses freed ranges (`aricode_vranges::VirtRanges`, host test with a
  million cycles that never move the top; `test=dma_lifetime` checks the
  range comes back), block servers use the bounded cache, `mmio_unmap`
  invalidates only its own CPU (a process has one thread; the argument is
  in ADR 0038). ADR 0038's "bounded" claims now say what is bounded
  (pinned frames) and what is bounded differently (page tables: by the
  ranges mapped at once).
- **H1, medium:** a `map_range_4k` that failed half-way left untracked
  leaves on the DMA frames (use after free, a double free at reap, a range
  stuck `AlreadyMapped`). Now all or nothing (host test).
- **H1, low:** an IOMMU failure in `mmio_map` kept a mapping the caller
  never learnt; the buffer was not held until the IOMMU had it. Fixed.
- **H2, high: the next claim re-enabled bus mastering on a device that was
  never reset.** Without FLR (the laptop's Realtek and Intel xHCI), the
  device keeps its receiver and ring addresses; frames reused during the
  supervisor's backoff would be written as soon as `claim` turned bus
  mastering on. QEMU hides it (virtio clears DRIVER_OK on its own). Fix:
  `claim` leaves bus mastering off and each driver turns it on with
  `sys::pci::enable_bus_master` after resetting its device.
- **H2, medium:** claim versus a new FLR (re-checked under the lock);
  silent skips in `stop_claims_of` (warnings, fault events, quarantine of
  the dead driver's DMA buffers when bus mastering stays on); a claim
  dropped outside a reap (`ClaimToken`); FLR without checking pending
  transactions, readiness or the PCIe Device Control registers (added,
  with the `pci.flr=off` switch for the laptop).
- **H2, low:** BARs restored from the dead driver's state and BIST written
  (now the boot-time header); test gaps closed (AF TP alone is not FLR,
  a stronger `iter` test, and `serial_order` in `drivers.pci_reap` checks
  that bus mastering is cleared before the dead driver's frames are
  freed). `drivers.pci_reap` still proves the kernel's bookkeeping and
  ordering, not a device that keeps writing (QEMU does not emulate one).

Results after the fix: `cargo xtask check` clean, host tests green,
`cargo xtask test --all` 160 QEMU scenarios, 0 failed; `cargo xtask test --all --cpus 4`
160 QEMU scenarios, 0 failed.

## Open

- FLR on the reference laptop's functions runs only when a driver dies and
  is restarted (its Realtek and Intel xHCI advertise none); watch the first
  such restart there (the log lines above, then the driver's own start);
  `pci.flr=off` turns FLR off.
- Pending transactions are waited for only 1 ms before the FLR (`reap()`
  runs with interrupts off); a function still busy is not reset (fault
  event), and its next driver starts from its own reset with bus
  mastering off.
- The block servers still need a CPU mapping only to get the IOMMU entry;
  an IOMMU-only mapping call would avoid it.
- Out of scope, found by the audit: virtio-blk closes the client's buffer
  handle after a request that failed or timed out, while the device may
  still own the buffer. Its cache mapping keeps the buffer alive for now,
  but an eviction could free it under a device that never completed.
  usb-input and usb-ehci keep such a buffer (`held_buffer`,
  `buffer_unsafe`); virtio-blk should do the same, or reset the device.
- The drivers' new bus-mastering order (claim leaves it off, the driver
  enables it after its reset) has run only in QEMU.

## Second audit (2026-10-21, commit c6df9a9)

An independent re-audit of the follow-up commits found nothing serious:
both previous high findings and their medium and low findings are fixed.
It verified the local-only TLB invalidation in `mmio_unmap` against the
scheduler (one thread per process, CR3 reloaded on every switch to
another address space, no PCID, no global user pages, migration inside
the syscall covered), the `VirtRanges` allocator (random model test,
mutations caught), that every claimant enables bus mastering after its
device reset and before programming DMA, the `ClaimToken` race and lock
order, the FLR checks and the `serial_order` check. Full suite on c6df9a9:
160 scenarios with 0 failed with 1 CPU and with 4 CPUs.

Left open by that audit:

- a buffer released after a failed request while the device may still
  own it (virtio-blk timeout, xHCI Disable Slot): incident 0017;
- the firmware USB handoff now runs with bus mastering off (usb-input,
  usb-ehci): watch the first boot on the laptop's Intel xHCI for a Host
  System Error or a handoff timeout;
- a claim transferred to another process skips the stop and the
  quarantine at reap (no driver does it);
- quarantine is bounded per incident, not overall, if a driver with a
  stuck device keeps being restarted;
- the readiness poll after FLR treats any vendor id but 0xFFFF as ready
  (CRS visibility would read 0x0001).
