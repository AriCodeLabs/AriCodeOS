# ADR 0038: DMA buffer lifetime

- **Date:** 2026-10-20
- **Status:** Accepted, revised after two audits on 2026-10-21 (see
  "Audit"; incident 0004, robustness audit 1 finding M4;
  amends ADR 0005 and the Phase 16, 28 and 33 notes in
  `docs/architecture.md`)

## Context

A DMA buffer is a kernel object (`hw::DmaBuffer`) whose frames go back to
the PMM when the last reference to the object goes. `mmio_map` maps a
buffer into the calling process, but the mapping recorded only `(virt,
phys, span)`: it did not count as a reference. A server that keeps a
client's buffer mapped after closing its handle (the compositor with every
window buffer; arifs and net-core with their client buffer caches; the
block servers, which map a client's buffer on every request and never
unmap it) therefore kept a writable CPU mapping of frames that were freed
as soon as the client closed its own handle or exited. Reproduced by
`test=dma_lifetime` (incident 0004): all four frames of a 16 KiB buffer
were free in the PMM while the server still mapped them. A physical
use-after-free across protection domains.

The device side had the same gap: `pci::claim` turns bus mastering on and
nothing turned it off when the driver died. With an IOMMU the dead
driver's domain is withdrawn at reap; without one (the reference laptop)
the device could keep writing to the physical addresses the dead driver
had programmed, which were freed with the driver's buffers.

## Options considered

(a) **A mapping holds the buffer.** Each foreign entry of a DMA buffer
keeps its `ObjectRef`; the frames are freed when the last handle *and* the
last mapping are gone. No TLB shootdown on the free path (the frames are
never freed while mapped), no reverse map. Cost: a server can keep a
client's memory allocated after the client is gone, and without a way to
drop a mapping a long-lived server (compositor, arifs, block drivers)
would accumulate every buffer it ever mapped.

(b) **Revocation on drop.** `DmaBuffer::drop` unmaps the buffer from
every process that mapped it, with a TLB shootdown, as it already does for
IOMMU domains. Needs a reverse map from buffer to `(process, virtual
range)` and a shootdown from the drop site. The last reference is often
dropped in `reap()` with interrupts disabled, where `smp::flush_others`
cannot run (it waits for the other CPUs with interrupts on), so it would
also need deferred work. Worse, it moves the failure to the server: a
client that closes its handle in the middle of a request makes the
compositor or a driver fault on an unmapped page and be killed. A client
must not be able to kill its server.

(c) **Both.** Keeps the cost of (b) for no gain once (a) holds: with (a)
there is never a mapping of freed frames to revoke.

**Decision: (a), plus an explicit `mmio_unmap`.** It is correct on SMP with
the least new machinery: frames are only freed when no page table points at
them, and the one place that removes a live mapping (`mmio_unmap`) runs in
the process's own syscall, on the CPU that runs its only thread. The
pieces:

1. `Process::foreign` entries carry `Option<ObjectRef>` (DMA buffers; MMIO
   regions need none, their frames never belong to the PMM). Reaping the
   process unmaps the entries and only then drops the references.
2. `mmio_unmap(virt)` (syscall 36, ABI 5) removes the mapping `mmio_map`
   returned: page table entries cleared under the process lock, the range
   invalidated with `invlpg` on this CPU (`vmm::flush_own_user_range`),
   and only then the addresses given back and the reference dropped. No
   IPI: a process has one thread, running here, and every other CPU that
   ran it has since loaded another CR3 (the scheduler loads the next
   thread's CR3 whenever it differs, and a kernel thread or another
   process always has a different one), which drops non-global entries.
   When processes get more threads this must become a shootdown to the
   CPUs running them.
3. The addresses of the mapping area come from a per-process
   `aricode_vranges::VirtRanges` (first fit, split, merge, the top lowered
   when its last range goes): an unmapped range is reused, so the page
   tables covering it are reused too. `unmap_4k` still never frees an
   empty page table; what tables cost is bounded by the largest set of
   ranges mapped at once (and by the end of user space), not by how many
   map/unmap cycles happened. `map_range_4k` is all or nothing: a range it
   cannot finish (no frame for a page table) is rolled back, so no leaf is
   left that the process table does not know about.
4. Servers drop what they do not need: the compositor unmaps a window
   buffer with the last window shown from it (two windows may share one
   buffer: the kernel returns the same mapping); the block servers
   (virtio-blk, usb-input, usb-ehci, which map a client buffer only for its
   IOMMU entry), arifs and net-core share a bounded cache
   (`client_buffer::ClientBuffers<8>`) that unmaps the oldest mapping to
   make room. The old caches kept mapping past 8 entries without recording
   them. A client cycling more than 8 buffers makes the cache miss every
   time: that costs a map and an unmap syscall per request (no IPI, no new
   page tables), not memory.
5. The `mmio_map` deduplication by physical range stays and becomes exact:
   a mapped buffer cannot be freed, so the same range is the same buffer.
   `mmio_map` holds its own reference until the IOMMU has the buffer, and
   if the IOMMU refuses it a mapping made by that call is removed again
   (the caller never learns the address).

## Security and accounting

- **Who pays for pinned frames.** A buffer's frames stay charged to the
  process that allocated it: `dma_alloc` charges its quota cumulatively and
  never refunds (Phase 28), so a buffer that outlives its client through a
  server's mapping costs nobody anything new.
- **Bound on what a server pins.** The DMA bytes a process keeps mapped
  (`Process::dma_mapped`, own and foreign buffers) count against its quota
  when it has one: `mmio_map` beyond it is refused with `Limit` and a
  `DmaQuota` event. Applications launched with a manifest (`memory`) are
  bounded that way; a process cannot map more than it could have
  allocated. System services started without a quota (the compositor,
  arifs, netd, the drivers) are part of the base image and bounded by their
  own code instead: the compositor by its window slots, the caches by 8
  entries.
- **What "bounded" covers.** The bounds above are on pinned DMA frames.
  Kernel memory a process makes the kernel spend on mappings (page-table
  frames, uncharged to anyone, as for every other mapping) is bounded by
  the largest set of ranges it maps at once, thanks to range reuse; it is
  not charged to a quota.
- **What a server can do with a pinned buffer.** Nothing new: it already
  had a capability to it. What changes is that it can no longer read or
  write anyone else's memory after the client is gone; it sees the
  client's last contents.
- **What a client can do to a server.** Nothing: closing its handle or
  exiting never unmaps anything in the server, so it cannot make the
  server fault (the reason (b) was rejected).
- **IOMMU.** Unchanged: `mmio_map` of a client buffer adds it to the
  server's domain, and `DmaBuffer::drop` removes it from every domain
  before the frames return. `mmio_unmap` leaves the IOMMU entry; the
  device keeps access to the client's buffer while the buffer lives, as
  before.
- **Visibility.** Dropping the last mapping of a buffer whose handles are
  all gone logs `user: process N dropped the last mapping of a DMA buffer
  it had no handle to (K KiB at 0x...)`, once per mapping (bounded by the
  mappings a process can have). It is normal for the compositor with a
  client that exited; it is what to look for when memory seems held.

## Bus mastering and drivers that die (H2)

**Claims leave bus mastering off; the driver turns it on after its reset.**
Clearing bus mastering pauses a device, it does not reset it: a function
without FLR (the laptop's Realtek 10ec:8136 and Intel xHCI 8086:1e31 have
none) keeps its receiver enabled and its old ring addresses. If the next
claim turned bus mastering back on, the device would write into memory
reused during the supervisor's backoff until the new driver reached its
own reset. So `pci::claim` writes the command register with memory space
on, INTx off and bus mastering off, and every driver calls
`sys::pci::enable_bus_master(CFG)` once its device is reset, before giving
it any address: the virtio transport (virtio-blk, virtio-net,
virtio-input) right after `DEVICE_STATUS` reads 0, rtl-net after the chip
reset, usb-input after HCRST completes and usb-ehci after HCRESET. The
first claim after boot does the same (the firmware's state is not
trusted either). Quarantining the dead driver's frames until the device is
reset was the alternative; it keeps memory forever for every death of a
function without FLR and still leaves the old rings live, so it is only
the fallback below.

`release_address_space` runs in this order:

1. Take the dead process's capabilities out of its table.
2. `pci::stop_claims_of`: every function whose claim (the configuration
   object `pci::claim` hands out) is held only by that table clears bus
   mastering (command register bit 2, PCI Local Bus 6.2.2). A claim held
   elsewhere too is left alone with a warning and a `claim held elsewhere`
   fault event (its `ClaimToken` stops the device when it goes). A
   function that still reads back as bus master gets a warning, a `bus
   master stuck on` fault event, and the dead process's DMA buffers (held
   or mapped) are quarantined: kept for good, never reused.
3. Unmap and free the address space, dropping the foreign references
   (DMA buffers whose last holder was a mapping are freed here).
4. Withdraw the IOMMU domain; drop the capabilities (the driver's own DMA
   buffers, its IRQ lines, which mask MSI/MSI-X).
5. `pci::reset_released`: a function stopped in step 2, still unclaimed,
   that advertises FLR (PCI Express Device Capabilities bit 28, or the
   Advanced Features capability) is reset unless `pci.flr=off` is on the
   command line (recovery knob for real hardware, logged) or it still has
   transactions pending after 1 ms (Transactions Pending, PCIe 7.5.3.5 /
   AF TP; skipped with a `flr skipped: tx pending` fault event, bus
   mastering being off already). The PCIe Device Control and Device
   Control 2 registers are saved and the FLR started. Nothing else waits
   here (`reap()` runs with interrupts disabled).
6. The next `pci::claim` of that function waits until 100 ms have passed
   since the FLR started (PCIe Base 6.6.2) and until the function answers
   configuration reads (vendor id not 0xFFFF, up to 1 s; otherwise the
   claim fails with a `flr: function not ready` fault event and the
   supervisor retries later), sleeping without locks. It then restores the
   header as the firmware left it at boot (BARs, cache line size, latency
   timer, interrupt line; never BIST nor the dead driver's values) and the
   saved Device Control registers (without the FLR bit), and checks again
   under the PCI lock that no new FLR started meanwhile before claiming.

Stop, then free: no frame of the dead driver goes back to the PMM before
its device has stopped mastering the bus (`drivers.pci_reap` checks the
order of the log lines with the runner's new `serial_order`).

The claim is also a capability that can be closed or passed on: the
configuration object carries a `pci::ClaimToken`, and dropping the last
reference to it, by any path, clears bus mastering (unless a newer claim
of the same function exists, checked with a per-function generation). The
early stop in the reap is kept for the ordering. The token's object is
created last in `claim`, after everything that can fail, so it is never
dropped with the PCI locks held.

## Consequences

- ABI 5: `mmio_unmap`; `mmio_map` documents that it deduplicates and that
  a mapping keeps a DMA buffer alive.
- `test=dma_lifetime` (`memory.dma_lifetime`): a mapping keeps the frames
  after the last handle, they return on reap and on `mmio_unmap`, and
  mapping beyond the quota is refused. `test=pci_reap`
  (`drivers.pci_reap`): bus mastering cleared on reap for the virtio-blk
  and for an `igb` that advertises FLR (scenario option `flr_device`),
  FLR started for the latter, both claimed again with the same header.
- Every driver must call `sys::pci::enable_bus_master` after its reset; a
  driver that forgets gets a device that never completes DMA (visible at
  once in its first request), not a silent hazard.
- Not validated on hardware yet: the drivers' new bus-mastering order and
  FLR on the reference laptop (its Realtek and Intel xHCI advertise no FLR;
  only the order matters there). `pci.flr=off` is the knob if a reset
  misbehaves.

## Audit (2026-10-21)

Two independent audits of `537759f` confirmed the ordering, the lock order,
the TLB handling and the FLR detection, and found:

- **High (H1):** `mmio_next` only grew and `unmap_4k` never frees tables,
  so per-request map/unmap cycles took new addresses and kept a page
  table for every 2 MiB crossed. Fixed by range reuse (`VirtRanges`,
  item 3), caches instead of per-request mappings in the block servers,
  and no IPI per unmap (item 2). The "bounded" claims were corrected
  ("What bounded covers").
- **Medium (H1):** a partial `map_range_4k` left untracked leaves pointing
  at the DMA frames. Fixed (all or nothing, host test).
- **Low (H1):** an IOMMU failure in `mmio_map` kept a mapping the caller
  could not learn; `mmio_map` did not hold the buffer until the IOMMU
  had it. Fixed (item 5).
- **High (H2):** the next claim turned bus mastering back on for a device
  that had never been reset. Fixed: claims leave it off, drivers enable it
  after their reset.
- **Medium (H2):** race between `finish_reset` and a new FLR (re-checked
  under the lock); silent skips in `stop_claims_of` (now warnings, fault
  events and quarantine); a claim dropped outside a reap (`ClaimToken`);
  FLR without checking pending transactions, readiness or Device Control
  (all added, plus `pci.flr=off`).
- **Low (H2):** BARs restored from the dead driver's state and BIST
  written (now the boot-time header, bytes for the byte fields); test
  gaps (AF TP alone vs FLR, a handle-table `iter` mutant, the order of the
  stop and free lines) closed. `drivers.pci_reap` checks bookkeeping, the
  command register and the order of events; QEMU's devices do not keep
  writing after their driver dies, so the hazard itself is not emulated.

