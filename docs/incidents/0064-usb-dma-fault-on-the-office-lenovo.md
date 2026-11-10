# Incident 0064: a USB DMA fault on the office Lenovo

- Date: 2026-11-10
- Environment: the owner's office machine, a Lenovo V15 G2 ITL (type 82KB, Tiger Lake,
  xHCI 8086:a0ed at 00:14.0), booted from the dev build 66 stick; the report is `diag-lenovo.txt`
  (`~/aricode-scratch/laptop/diag-lenovo.txt`, not in the repository: it came from a
  machine that is not ours).
- Work branch: `feature/acpidump`, from `develop` at `7aad3e7`.

## What was seen

Six seconds after boot, while `usb_input` (pid 8) brought up the second xHCI's devices,
the IOMMU blocked a read by the controller:

    [6.372 s] DMA fault from device 00a0: read from page without r
    [9.531 s] fault reported by process 8: usb sense 5 20 00 (code 5)

`00a0` is 00:14.0, the xHCI. Sense 5/20/00 is ILLEGAL REQUEST, INVALID COMMAND OPERATION
CODE, from a mass storage device on that controller. The desktop came up and a USB mouse
worked, as on the first visit (2026-11-07); the touchpad did not, for a reason of its own
(an I2C HID device, which AriCodeOS has no driver for).

## What is not known

The event kept only the device and the first 24 bytes of the reason: not the address the
controller read, nor whether the page was one the driver had mapped and since unmapped, a
page never mapped, or a structure the controller reads before the driver expects it (a
scratchpad, a context, an event ring segment). The reference laptop's xHCI (8086:1e31) has
never faulted, and QEMU's does not either. Whether the sense error follows from the fault
or is a device's answer to a command it does not support is not known either.

No fix without the cause (ADR 0028).

## What changed

- A DMA fault event now carries the device, whether it was a read or a write, the IOMMU's
  reason code and the page (`sysinfo::dma_fault_code`), and a reason short enough for the
  event's 24 bytes; `diag` prints `DMA fault from 00:14.0, read at 0x...: no read
  permission`. The kernel's log line had the address already, but `diag` on a machine
  without a serial port is all a report carries.
- `acpidump NAME` saves the firmware's DSDT and SSDTs, for the touchpad study and to see
  how the firmware describes this xHCI (its ports and their `_UPC`, `_PLD`).

## Next

The owner runs, on the office machine, `diag diag2.txt` and `acpidump acpi.bin`. With the
page known, the xHCI driver's own mappings at that moment say what the controller read.
