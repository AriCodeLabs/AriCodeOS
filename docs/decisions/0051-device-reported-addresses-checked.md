# ADR 0051: Addresses a PCI device reports are checked before the CPU uses them

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25, A-H1 and A-L4; incident 0030)

## Context

A PCI function describes itself through its configuration space: where
each memory BAR lies and how large it is, and where in which BAR its
MSI-X table sits. Until now the kernel believed it:

- the MSI-X entry was written at `BAR + Table Offset`, with the offset
  (up to 4 GiB) never compared with the BAR's size, and the register read
  again at every claim;
- every memory BAR became an `Mmio` object for the driver with the
  address and the size the device reported, checked only for being
  non-zero.

So a malicious or broken device (external PCIe, Thunderbolt, reflashed
firmware) chose where the kernel wrote 16 bytes, and what physical memory
its driver could map: RAM included, the kernel's own. The IOMMU (ADR 0038)
does not stop either: the CPU does the writing. It is the same failure as
this week's Brotli disclosure: a length or offset from outside used to
address memory without a bound.

## Decision

1. **Sizes are measured once, at boot.** Sizing writes ones into a BAR,
   so the function's memory decoding is off meanwhile (only that bit:
   I/O BARs are never sized), with interrupts off on that CPU, and only
   for functions that have a memory BAR; host bridges are never touched
   (turning their decoding off can be fatal). The size is the lowest
   address bit the device lets software set, as Linux computes it. A
   claim no longer probes.

2. **Each memory BAR is checked at boot** (`aricode_pci::check_bar`):
   - a power-of-two size of at least 16 bytes, the address aligned to it,
     the range within the address space;
   - by whole 4 KiB pages (what a mapping covers), no overlap with what
     the firmware map called RAM, firmware data or reserved memory (kept
     by the physical allocator in a sorted, coalesced `RangeSet` that only
     ever errs towards covering more), the chipset's fixed range
     `0xFEC00000..4 GiB` (I/O APICs, HPET, TPM, the MSI window, boot
     flash), a relocated local or I/O APIC, the DMA remapping units (their
     whole register set), ECAM, or a kept BAR of another function;
   - the framebuffer only from the display's own BAR, the one that holds
     all of it.
   A BAR the firmware left unassigned is said as such. A BAR that fails
   is not offered to any driver, and the kernel says why
   (`pci: 00:04.0 BAR 0 at 0x100000 (0x4000 bytes) not offered:
   Overlaps("RAM")`). A refused BAR never counts against another
   device's, so a device lying about its size only loses its own; a
   device whose well-formed BAR overlaps another's does take that one
   with it, and only fixing the hardware helps there.

3. **The MSI-X table must lie within a checked BAR**
   (`aricode_pci::msix_table_fits`), checked at boot together with the
   register it was read from. A claim uses the table address found then;
   if the register reads differently at the claim, or the table did not
   fit, the function gets MSI if it has it (MSI-X is turned off first, so
   it cannot take precedence), else no interrupt, and the kernel says so.

4. **A claim offers a BAR only where it was at boot.** The BAR registers
   are read again; a BAR that moved since is not offered. Memory decoding
   is turned on only if every assigned BAR was offered; otherwise it is
   left as the firmware set it (a refused BAR may overlap another
   device's). The claim itself still succeeds: the driver gets what was
   offered (DMA, the interrupt) and decides.

5. **Enumeration stays within the segment's buses**: a bridge naming a
   bus below the segment's first is skipped, as one above its last
   already was (A-L4: the ECAM offset wrapped). A 64-bit BAR in a
   header's last slot is ignored: its "high half" would be another
   register.

## Consequences

- A device cannot direct the kernel's MSI-X write, nor give its driver a
  mapping of RAM, firmware memory, the chipset's fixed MMIO or the
  kernel's own.
- A real machine whose firmware places a BAR so that it shares a page
  with another device's BAR loses that BAR; one whose firmware marks part
  of the PCI window reserved would lose the BARs there. Each such refusal
  is in the boot log. Verified: QEMU's machines (the full suite with 1 and
  4 CPUs); the reference laptop is checked with a development build
  before this reaches `main`.
- The framebuffer is a BAR of the display device and stays allowed to
  it: the kernel already writes to it.

## Tests

- Host: `aricode-pci` `device_reported_bars_are_checked`,
  `msix_table_bounds`, `a_64_bit_bar_in_the_last_slot_is_ignored`;
  `aricode-pmm` `sorted_and_merged_in_any_order`,
  `full_set_fills_the_smallest_gap`.
- `pci_fault=msix|bar` (tests only) makes the kernel read the xHCI
  controller as lying: `pci.lying_msix` (table past its BAR: MSI instead,
  the keyboard works; the runner's `usb_hid_msi_and_msix` gives it both:
  QEMU's xHCI as the runner starts it otherwise offers MSI-X only)
  and `pci.lying_bar` (BAR 0 in RAM: refused, the driver gets nothing,
  the system boots).
- The full suite with 1 and 4 CPUs: every driver's BARs are offered as
  before.

## Note (2026-10-27, ADR 0061)

Point 3 also requires the BAR that holds the MSI-X table to read as at
boot, and memory decoding to be on after the claim's command write (point
4 leaves it off when a moved BAR is not offered): a driver can move a BAR
through its configuration page, and the kernel programmed the table where
the function no longer decoded it (incident 0050). A claim that does not
use MSI-X now turns it off whether or not the function has MSI.
