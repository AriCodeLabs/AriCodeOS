# ADR 0061: An interrupt line masks only its own message, and MSI-X needs its BAR where it was

- **Date:** 2026-10-27
- **Status:** Accepted (incident 0050; second audit of incident 0047).
  Amends ADR 0022 (point 2) and ADR 0051 (point 3).

## Context

`pci_claim` hands a driver an interrupt line with `TRANSFER`. ADR 0022
said that dropping a line masks its source: the MSI-X entry is written
masked, or the MSI enable bit cleared. Nothing tied the line to its
claim, so a line can outlive it: passed to another process, left in a
message nobody received yet, or kept by a driver that closed only its
configuration handle. Init then claims the function again and programs
the same entry 0 with a new vector; when the old line goes, its drop
masked the new driver's interrupts, and its write took no lock while the
new claim's write held `VECTORS`, so the two could interleave into a
torn entry (incident 0050, defect 1). Normal restarts never showed it:
`reap` drops every handle of the dead driver before init claims again.

ADR 0051 made a claim check that the MSI-X capability register (BIR and
offset) reads as at boot, and used the table address found then. The BAR
that holds the table was not checked again: a driver can move it through
its writable configuration page, and on the next claim of a function
without FLR (an FLR restores the boot-time BARs) the kernel programmed
entry 0 where the BAR had been, an uncached write to an address nothing,
or another device, decodes (defect 2).

## Decision

1. **A line masks only what it programmed.** When a line is dropped, its
   MSI-X entry or MSI capability is read: it is masked (or MSI disabled)
   only if it still holds this line's message, its vector towards the
   boot LAPIC. A vector is never given to two live lines
   (`allocate_vector` skips the ones in use), so a newer claim's entry
   always holds another one. Otherwise the drop leaves the source alone
   and says so (`irq: vector 0x30 released; its device was programmed
   again since (a newer claim), left as it is`). The read, the check and
   the mask run under `VECTORS`, the lock `register_msix` and
   `register_msi` program under, so no entry is torn. I/O APIC lines are
   unchanged: no claim hands them out.
2. **MSI-X needs the table's BAR where it was at boot.** A claim uses
   MSI-X only if the capability register and the BAR it names both read
   as at boot (the same check that decides whether a BAR is offered).
   Otherwise the function gets MSI if it has it, else no interrupt, and
   the kernel says why (`pci: 00:03.0 MSI-X table's BAR 1 moved since
   boot: MSI-X not used`).
3. **A claim that does not use MSI-X turns it off,** with or without
   MSI: a function left with the old claim's MSI-X enabled would go on
   sending the old message, whose vector belongs to another line once the
   old one is gone.

## Alternatives

- **Revoking the old line when the function is claimed again.** The
  device record would keep a weak reference to the line, and the claim
  would revoke it and tell its drop not to mask. It needs that bookkeeping
  and an ordering between the revocation (under the PCI locks) and the
  drop (under `VECTORS`), and covers only the reclaim: what the hardware
  holds is the fact that decides whether the entry is still the line's,
  and reading it covers every way the line can outlive its claim. The
  holder of an old line can still wait on it; after a reclaim it never
  fires, since the device sends another vector.
- **Re-reading the BAR in the line's drop too.** A drop after its driver
  moved the BAR reads the entry at the old address: there nothing decodes
  (reads do not return the line's message, so nothing is written), or a
  device whose driver arranged for it to echo the exact message, which
  then gets the mask itself. Not worth a second check.

## Consequences

- An old claim's line can no longer silence or tear a newer claim's
  interrupts; a dropped line that still owns its source masks it as
  before.
- A driver that moves its MSI-X BAR loses MSI-X for the next claims of
  the function until the boot-time BAR is back (an FLR restores it).

## Tests

- `pci.stale_line` (`test=pci_stale_line`): `pci_irq_keeper` leaves its
  line in a queued message and dies; the function is claimed again and
  the message dropped: the new claim's MSI-X entry (virtio-blk) and MSI
  (an xHCI with MSI only) stay delivered, and the newest line, dropped,
  masks its source. `pci_bar_mover` moves the virtio-net's MSI-X BAR and
  dies: the next claim hands out no MSI-X line and leaves MSI-X off.
  Both failed on develop (incident 0050).
