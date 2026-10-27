# ADR 0022: MSI in addition to MSI-X for user drivers

- **Date:** 2026-10-10
- **Status:** Accepted

## Context

`pci_claim` handed drivers an IRQ only if the function had MSI-X (entry 0
programmed by the kernel). In QEMU every device we use (virtio, qemu-xhci)
has MSI-X, so it was never missing. On the first physical boot (Phase 34)
the laptop's xHCI, an Intel 7 series (`8086:1e31`), only offers MSI: the
kernel claimed it without an IRQ, init did not set bit 15 of the mask, and
`usb-input` exited with "xHCI initialization failed" (code 10) with no
further explanation. The same will happen to AHCI, older NVMe and almost
any 2012 controller.

## Decision

1. **MSI as the second option.** `pci_claim` programs MSI-X if the
   function has it; otherwise, the MSI capability with a single message
   (address targeting the boot LAPIC, data = vector, per-vector mask
   unmasked if present). The driver does not distinguish the source: it
   receives the same IRQ handle and uses `irq_wait`. INTx is still
   unsupported.
2. **`IrqSource::Msi { cfg_virt, cap }`** in `hw`: masking means clearing
   the enable bit in the capability's control register (ECAM, uncached).
   Vector allocation and line creation are shared with MSI-X.
3. **The claim trace states the source**: `claimed (MSI-X entry 0)`,
   `claimed (MSI)` or `claimed (no MSI or MSI-X: no IRQ)`.
4. **`usb-input` explains the capabilities failure**: "no BAR 0 or no IRQ
   among the capabilities" versus "could not map BAR 0".
5. **Scenario `shell.usb_keyboard_msi`**: `usb_hid_msi = true` starts
   QEMU's xHCI with `msi=on,msix=off`; it requires `claimed (MSI)` and that
   the USB keyboard reaches the shell.

## Consequences

- A single vector per function: multi-message MSI (MME > 0) is not used;
  today's drivers expect one IRQ.
- Functions with neither MSI nor MSI-X are claimed without an IRQ (as
  before) and the driver sees it in the mask; INTx via the I/O APIC is left
  for when a device needs it.

## Note (2026-10-27, ADR 0061)

Masking on drop (point 2) applies only while the source still carries the
line's own vector: a line can outlive its claim, and a newer claim of the
function programs another vector, which the old line leaves alone
(incident 0050).
