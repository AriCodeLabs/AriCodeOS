# ADR 0061: An interrupt line masks only its own claim's source, and MSI-X is written only where the function decodes it

- **Date:** 2026-10-27
- **Status:** Accepted (incident 0050; second audit of incident 0047;
  revised after the audit of the branch). Amends ADR 0022 (point 2) and
  ADR 0051 (point 3).

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
or another device, decodes (defect 2). The audit of the first fix found
the same with another BAR moved: memory decoding then stays off (a moved
BAR is not offered), the table does not answer at all, and MSI-X was still
programmed (into nothing) and enabled.

## Decision

1. **A line touches its function's interrupts only while its claim is
   the latest.** Under `VECTORS` the kernel keeps, per PCI function, a
   number that every claim changes (`register_msix`, `register_msi`, or
   `claimed_without_interrupt` for a claim with no line); each MSI-X or
   MSI line records the number of its claim. When a line is dropped it
   masks its MSI-X entry (or disables MSI) only if its number is still
   the function's; otherwise it leaves the source to the newer claim and
   says so, after releasing the lock (`irq: vector 0x30 released; its
   function was claimed again since, its interrupts left to the newer
   claim`). The decision and the mask run under `VECTORS`, as every
   claim's programming does, so no entry is torn. I/O APIC lines are
   unchanged: no claim hands them out.
2. **MSI-X is written only where the function decodes it.** A claim uses
   MSI-X only if the capability register and the BAR it names both read
   as at boot (the same check that decides whether a BAR is offered) and
   memory decoding is on after the claim's command write. Otherwise the
   function gets MSI if it has it, else no interrupt, and the kernel says
   why (`pci: 00:03.0 MSI-X table's BAR 1 moved since boot: MSI-X not
   used`, `pci: 00:04.0 memory decoding stays off (a BAR was not
   offered): MSI-X not used`).
3. **One of MSI and MSI-X at a time.** A claim that does not use MSI-X
   turns it off, with or without MSI: a function left with the old
   claim's MSI-X enabled would go on sending the old message, whose
   vector belongs to another line once the old one is gone. A claim that
   uses MSI-X turns MSI off first: an earlier MSI claim's line that lives
   on no longer turns it off (point 1).

## Alternatives

- **Reading the device in the drop** (the first version of this ADR):
  mask only if the entry or capability still holds the line's vector.
  Exact while the device answers, but it made the drop do non-posted
  reads with `VECTORS` held and interrupts off: a function in the 100 ms
  after an FLR may stall such a read until the completion timeout while
  other CPUs spin on `VECTORS` in the interrupt handler, and after BARs
  moved the read may reach another device (read side effects) or nothing
  (all ones, logged as a newer claim when there was none). The claim
  numbers decide the same thing without touching the device.
- **Revoking the old line on reclaim** (the object's revocation flag):
  the claim would have to find every older line of the function and the
  drop would have to see the flag; the claim numbers under `VECTORS` are
  that, without walking objects whose last reference may be going away
  at that moment.
- **Reading entry 0 back after programming it** and falling back to MSI
  when it does not hold the message: with point 2 the kernel writes only
  where the function decodes, and a read-back would not detect another
  device decoding the same address either. Not added.

## Consequences

- An old claim's line can no longer silence or tear a newer claim's
  interrupts; a dropped line of the latest claim masks its source as
  before. The drop never reads the device.
- A drop of the latest claim's line still writes its MSI-X entry at the
  boot-time table address. If its own driver moved that BAR, the write
  goes where the BAR was: nowhere (posted, dropped), or into another
  function whose driver moved a BAR there, which the kernel cannot see
  without reading. The same write was made before this ADR.
- A driver that moves a BAR of its function, or leaves memory decoding
  off with a BAR moved, loses MSI-X for the next claims until the
  boot-time BARs are back (an FLR restores them).
- The holder of an old line can still wait on it; after a reclaim it
  never fires, since the device sends another vector or nothing.

## Tests

- `pci.stale_line` (`test=pci_stale_line`), on an xHCI with MSI and
  MSI-X (`usb_hid_msi_and_msix`):
  - `pci_irq_keeper` leaves its line in a queued message and dies; the
    function is claimed again and the message dropped: the new claim's
    MSI-X entry (virtio-blk) and MSI (the xHCI with its BAR parked by the
    test) stay delivered, and the newest line, dropped, masks its source;
  - an MSI line of the parked xHCI outlives its claim; with the BAR back
    the next claim takes MSI-X and MSI is off while the old line lives;
  - `pci_bar_mover` moves, with decoding off, the virtio-net's MSI-X BAR
    and the virtio keyboard's other BAR, and dies: the next claim hands
    out no MSI-X line and leaves MSI-X off.
  Each failed before its fix (incident 0050).
