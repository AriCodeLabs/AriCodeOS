# ADR 0038: DMA buffer lifetime

- **Date:** 2026-10-20
- **Status:** Accepted (incident 0004, robustness audit 1 finding M4;
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
the process's own syscall, with interrupts enabled and no locks, where the
existing `smp::flush_others` applies. The pieces:

1. `Process::foreign` entries carry `Option<ObjectRef>` (DMA buffers; MMIO
   regions need none, their frames never belong to the PMM). Reaping the
   process unmaps the entries and only then drops the references.
2. `mmio_unmap(virt)` (syscall 36, ABI 5) removes the mapping `mmio_map`
   returned: page table entries cleared under the process lock, the range
   invalidated on this CPU and on the others (`vmm::flush_user_range`:
   `invlpg` per page plus one whole-TLB IPI), and only then the reference
   dropped. A process is single-threaded today, so the IPI is a safety
   margin for CPUs that ran it before; it stays correct when processes get
   more threads.
3. Servers drop what they do not need: the compositor unmaps a window
   buffer with the last window shown from it (two windows may share one
   buffer: the kernel returns the same mapping); block servers map a client
   buffer for one request (`client_buffer::MappedBuffer`, unmapped on
   drop; they only need it for the IOMMU entry); arifs and net-core share a
   bounded cache (`client_buffer::ClientBuffers<8>`) that unmaps the oldest
   mapping to make room. The old caches kept mapping past 8 entries
   without recording them.
4. The `mmio_map` deduplication by physical range stays and becomes exact:
   a mapped buffer cannot be freed, so the same range is the same buffer.

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
  entries, the block servers by one request.
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

## Drivers that die (H2)

`release_address_space` now runs in this order:

1. Take the dead process's capabilities out of its table.
2. `pci::stop_claims_of`: every function whose claim (the configuration
   object `pci::claim` hands out) is held only by that table clears bus
   mastering (command register bit 2, PCI Local Bus 6.2.2) with one
   configuration write. From then on the device issues no memory
   requests. Logged: `pci: BB:DD.F driver (process N) gone: bus mastering
   cleared before its memory is freed`.
3. Unmap and free the address space, dropping the foreign references
   (DMA buffers whose last holder was a mapping are freed here).
4. Withdraw the IOMMU domain; drop the capabilities (the driver's own DMA
   buffers, its IRQ lines, which mask MSI/MSI-X).
5. `pci::reset_released`: a function stopped in step 2, still unclaimed,
   that advertises FLR (PCI Express Device Capabilities bit 28, or the
   Advanced Features capability for conventional functions) is reset:
   the header dwords FLR clears (BARs, cache line, interrupt line) are
   saved and the FLR started. Nothing waits here (`reap()` runs with
   interrupts disabled); configuration space is not touched again until
   the next claim.
6. The next `pci::claim` of that function (the supervisor restarting the
   driver) first waits until 100 ms have passed since the FLR started
   (PCIe Base 6.6.2), sleeping without locks, restores the saved header
   and then claims as before.

Stop, then free: no frame of the dead driver goes back to the PMM before
its device has stopped mastering the bus. Transactions already pending at
the moment bus mastering is cleared are not waited for (bounded reap);
the FLR is started only after the buffers are freed, so a function that
needs it to stop completely is still stopped before anyone else can
claim it.

A claim held by someone other than the dead process (for example, passed
on to another process) is left alone: the device is theirs.

## Consequences

- ABI 5: `mmio_unmap`; `mmio_map` documents that it deduplicates and that
  a mapping keeps a DMA buffer alive.
- `test=dma_lifetime` (`memory.dma_lifetime`): a mapping keeps the frames
  after the last handle, they return on reap and on `mmio_unmap`, and
  mapping beyond the quota is refused. `test=pci_reap`
  (`drivers.pci_reap`): bus mastering cleared on reap for the virtio-blk
  and for an `igb` that advertises FLR (scenario option `flr_device`),
  FLR started for the latter, both claimed again with the same header.
- Not validated on hardware yet: FLR on the reference laptop's functions
  (Realtek, Intel xHCI/EHCI) runs only when a driver dies and is
  restarted; the first such restart on the laptop should be watched (the
  log lines above, then the driver's own start).
