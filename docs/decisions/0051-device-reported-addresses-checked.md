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

1. **Sizes are measured once, at boot**, with the function's memory and
   I/O decoding turned off during the sizing probe (as the specification
   requires), and kept. A claim no longer probes.

2. **Each memory BAR is checked at boot** (`aricode_pci::check_bar`):
   - a power-of-two size of at least 16 bytes, the address aligned to it,
     the range within the address space;
   - by whole 4 KiB pages (what a mapping covers), no overlap with RAM or
     firmware data (the firmware map's free, boot, runtime and ACPI
     regions, kept by the physical allocator), the local and I/O APICs,
     the MSI window (`0xFEE00000..0xFF000000`), the DMA remapping units,
     ECAM, or a BAR of another function.
   A BAR that fails is not offered to any driver; the kernel says why
   (`pci: 00:04.0 BAR 0 at 0x100000 (0x4000 bytes) not offered:
   Overlaps("RAM")`).

3. **The MSI-X table must lie within a checked BAR**
   (`aricode_pci::msix_table_fits`), checked at boot together with the
   register it was read from. A claim uses the table address found then;
   if the register reads differently at the claim, or the table did not
   fit, the function gets MSI if it has it, else no interrupt, and the
   kernel says so.

4. **A claim offers a BAR only where it was at boot.** The BAR registers
   are read again; a BAR that moved since is not offered.

5. **Enumeration stays within the segment's buses**: a bridge naming a
   bus below the segment's first is skipped, as one above its last
   already was (A-L4: the ECAM offset wrapped).

## Consequences

- A device cannot direct the kernel's MSI-X write, nor give its driver a
  mapping of RAM or of the kernel's MMIO. A function whose BARs are
  refused simply has no driver.
- A real machine whose firmware places a BAR so that it shares a page
  with another device's BAR loses that BAR. The reference laptop and QEMU
  boot with every BAR offered.
- The framebuffer is a BAR of the display device and stays allowed: the
  kernel already writes to it.

## Tests

- Host: `aricode-pci` `device_reported_bars_are_checked`,
  `msix_table_bounds`.
- `pci_fault=msix|bar` (tests only) makes the kernel read the xHCI
  controller as lying: `pci.lying_msix` (table past its BAR: MSI instead,
  the keyboard works; the runner's `usb_hid_msi_and_msix` gives it both)
  and `pci.lying_bar` (BAR 0 in RAM: refused, the driver gets nothing,
  the system boots).
- The full suite with 1 and 4 CPUs: every driver's BARs are offered as
  before.
