# Incident 0030: a PCI device chose where the kernel wrote

- Opened: 2026-10-25. Status: fixed on `fix/pci-device-bounds`.
- Found by: the failure-pattern audit of 2026-10-25 (audit A, H1: the
  Brotli pattern, an outside value used as an address without a bound).
  Not seen on a machine: it needs a device that lies.
- Environment: every claimed PCI function; any machine where a device
  can be malicious (external PCIe or Thunderbolt, reflashed firmware) or
  broken.

## Evidence

From the code of `develop` at `df15084` (`kernel/src/pci.rs`,
`claim_locked`):

- `table_phys = bar_phys + table_offset`, with `table_offset` up to
  `0xFFFF_FFF8` read from the function at every claim, went straight to
  `hw::register_msix`, which maps that physical page writable in the
  kernel and writes the 16-byte entry; dropping the interrupt line
  writes there again.
- Each memory BAR became an `Mmio` object (`phys` = reported address,
  `len` = probed size) after checking only `size != 0 && address != 0`.

The reproduction needs a lying device, which QEMU does not provide; the
test key `pci_fault` makes the kernel read the xHCI controller as one,
and the scenarios check what is done with it.

## Mechanism

The kernel treated configuration space as trusted input, unlike every
other device-supplied value its drivers bound (USB descriptor lengths,
transfer residues, ring indexes). The IOMMU confines what a device can
reach by DMA, not what the CPU does on its behalf: both paths were CPU
writes.

## Fix

ADR 0051: BAR sizes measured once at boot with decoding off; each BAR
checked against RAM, the kernel's MMIO and other functions' BARs, by
whole pages; the MSI-X table required to fit a checked BAR; a claim
offers only what was checked, where it was checked. ECAM enumeration
stays within the segment's buses (A-L4).
