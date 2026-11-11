# Incident 0064: a USB DMA fault on the office Lenovo

- Date: 2026-11-10
- Environment: the owner's office machine, a Lenovo V15 G2 ITL (type 82KB, Tiger Lake,
  xHCI 8086:a0ed at 00:14.0), booted from the dev build 66 stick; the report is `diag-lenovo.txt`
  (`~/aricode-scratch/laptop/diag-lenovo.txt`, not in the repository: it came from a
  machine that is not ours).
- Work branch: `feature/acpidump`, from `develop` at `7aad3e7`; then `fix/xhci-dma-fault`,
  from `develop` at `bd3c686`, after the second run.

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

## Second office run (2026-11-11, dev build 68, commit af612d8)

`diag2.txt` and the firmware's tables (`acpi.bin`, decompiled) stay outside the repository
(`~/aricode-scratch/lenovo/`). The lines that matter:

    [1.148 s] process 7 'usb_input' created
    [1.277 s] process 8 'usb_input' created
    [11.466 s] DMA fault from 00:14.0, read at 43b76000: no read permission
    [11.915 s] process 9 'ps2_input' created
    [14.650 s] process 10 'fsd' created
    [14.715 s] fault reported by process 8: usb sense 5 20 00 (code 5)

Two xHCIs: 00:0d.0 (8086:9a13, the Type-C subsystem, process 7) and 00:14.0 (8086:a0ed, the
PCH, process 8). 7991 MiB of RAM, 28 MiB in use at the report.

### What the code and the specifications settle

- **The page was never the driver's to lose.** usb-input has one DMA allocation, its arena,
  made before the controller is reset and never freed; every pointer it hands the
  controller (DCBAA and scratchpad array, rings and their link TRBs, ERST, contexts, the
  descriptor, report, CBW and CSW buffers) is inside it. The only other memory it maps is
  the client buffers of block requests, and fsd, the first client, started at 14.650 s,
  three seconds after the fault. Slot memory that a Disable Slot releases or a
  re-enumeration hands on stays in the arena and stays mapped. So at 11.466 s no page the
  driver had mapped and unmapped existed: the controller read an address it formed itself
  (a wrap at a boundary, a prefetch), one it got from somewhere other than this driver, or
  one the kernel failed to map.
- **The reason means "not mapped in this domain".** VT-d 4.0, 7.1.3, LGN.3 (6h): a read
  that found no read permission, which a page absent from the second-level tables gives
  too. The fault record holds only the page (bits 63:12). Our names for 7h and Dh were
  wrong ("entry not present", "error reading paging"): 7h is an error reading a paging
  entry (LSS.1) and Dh a request the context's translation type blocks (LCT.5); fixed, and
  Eh (LGN.4) added.
- **The controller kept working.** The storage on 00:14.0 answered a command at 14.715 s
  and the mouse worked. A DMA error a controller acts on sets HSE and halts it (xHCI 1.2,
  5.4.2), so the read was one whose result it did not need, or one it retried elsewhere.
- **The page is far from where the allocator was working.** The kernel's allocator hands
  out the lowest free frame (`BitmapAllocator::allocate`, `allocate_contiguous` from its
  hint) and only 28 MiB were in use; 0x43b76000 is at 1083 MiB. Unless this firmware's map
  leaves the first gigabyte to boot services that come back later, the arena was well
  below the page. That cannot be proven from the report: it holds neither the memory map
  nor the arena's address.
- **Boundaries: a real defect of ours, maybe not this one.** xHCI 1.2, chapter 6 before
  Table 6-1: no structure of up to 64 KiB may span a 64 KiB boundary and none of up to
  PAGESIZE a PAGESIZE boundary; the table gives the Device Context, each input context and
  the DCBAA the PAGESIZE boundary and the ring segments the 64 KiB one. The arena packed
  structures with 64-byte alignment only, so a Device Context or an Input Context could
  cross a page and a ring a 64 KiB boundary (every arena is longer than 64 KiB, so it
  always holds one). Linux allocates contexts from a pool with a PAGESIZE boundary and
  whole-page ring segments (`xhci-mem.c`, `xhci_mem_init`); its only prefetch quirk is
  Zhaoxin's (`XHCI_ZHAOXIN_TRB_FETCH`, two pages per segment). It is fixed as spec
  hardening. **A wrapped read cannot itself have produced this page**: a context crossing a
  page and read wrapped reads the start of its own page, inside the arena; every ring is
  256 or 512 bytes, so a ring crossing a 64 KiB boundary and read wrapped reads the first
  few hundred bytes of its own 64 KiB block, a page that is 64 KiB aligned (it would have
  been 0x43b70000), while 0x43b76000 is 0x6000 into its block; every other structure is at
  most 2 KiB. Only an indirect route remains: a context read wrapped hands the controller
  garbage (a TR Dequeue Pointer, say), which it then follows. That is a pointer we never
  gave, hypothesis 1 below.
- **No RMRR can explain a fault on its own.** The kernel maps a device's reserved regions
  into the domain its driver gets (`State::attach`), so a read inside an RMRR of 00:14.0
  would not fault. Memory the firmware's USB stack used before boot is no RMRR (VT-d 4.0,
  8.4 excludes regions that are DMA targets only before boot). Whether this DMAR has an
  RMRR at all is unknown: `acpidump` saves the DSDT and SSDTs only. The DSDT's XHCI device
  (`_SB.PC00.XHCI`, `_ADR` 0x00140000) has no memory OperationRegion; its ports say which
  are internal (`_UPC` connectable, `_PLD` not visible): HS05 (`CAMA`, a camera), HS06,
  HS07 and HS10 (Bluetooth, power resource `BTPR` in an SSDT). So besides the mouse and the
  stick, process 8 enumerated internal devices at boot, which is why its boot enumeration
  took until about 11.9 s (one port used its 10 s budget).
- **The sense was SYNCHRONIZE CACHE.** usb-input sends INQUIRY, TEST UNIT READY, REQUEST
  SENSE, READ CAPACITY(10), READ(10), WRITE(10) and SYNCHRONIZE CACHE(10). Sixty-five
  milliseconds after fsd started (AriFS mounting, which writes and flushes when it
  recovers or migrates), only the last is one a stick answers with ILLEGAL REQUEST,
  INVALID COMMAND OPERATION CODE; the driver then takes the flush as a no-op
  (`Sense::command_unsupported`). Unrelated to the fault, and not a failure. It is now
  recorded as `usb no sync cache` in both USB drivers, so a report names the command.

### Hypotheses, most likely first

1. **An address that was never ours.** Either memory the firmware used before boot (the
   controller or the firmware's SMM handler still holding a pointer from then), or garbage
   the controller read as a pointer, for instance from a context it read wrapped across a
   page (now impossible: the contexts are placed within a page). For: the page is far from
   what the allocator had handed out; the read did not stop the controller. The next run's
   `page` line says what the firmware's map called the page, and `below`/`above` how far
   it is from the arena.
2. **A page of the arena the kernel did not map** (tables or invalidation). Against: the
   IOMMU self-test and every translated QEMU scenario. The next run shows it as "in a
   region still mapped" (or, if the tables lost it, as a page inside the arena: the
   driver's `usb dma in LABEL`).

The boundary placement is not a hypothesis of its own: a wrapped read stays in pages the
domain maps (see above).

## What changed (branch `fix/xhci-dma-fault`)

- usb-input places every arena structure off the page and 64 KiB boundaries
  (`aricode_xhci::arena::place`), checks PAGESIZE offers 4 KiB pages, and logs each
  structure with a label; the arena grew to 128 KiB plus the scratchpads, which holds ten
  slots with four HID endpoints each and two storages placed that way at every phase
  against 64 KiB (host test).
- The kernel tags each domain region (own buffer, client buffer, RMRR, framebuffer) with
  its first mapping time, remembers the regions each domain unmapped (8 own, 16 others)
  and, for the first DMA faults of the boot (8 at most, two per device, one per page, so
  the second xHCI or a repeated page cannot use up the slots), keeps where the page lay:
  inside a live region, inside one unmapped before (and when), or between the nearest
  regions below and above; what the firmware's memory map called it (from an exact copy of
  the map, `aricode_pmm::KindMap`: no gap merged into a neighbour of another kind; MMIO
  named); whether the allocator had the frame; which RMRR holds it; which other domain
  maps it (`aricode_iommu::site`). New selectors `DMA_FAULT_SITES` (whole only for a
  `MANAGE` holder such as the shell; the device's driver gets its own regions without the
  other owner and the frame state; anyone else only the fault and the memory kind) and
  `IOMMU_RESERVED` (the RMRRs and their devices).
- `diag` prints an `iommu` line, the RMRRs and, per kept fault, `in`, `below`, `above`,
  `page` and `domain` lines.
- usb-input asks the kernel, at most once a second while its loop runs and at once after
  the boot enumeration, whether a fault came; for its own it prints the page against the
  arena (the structure on it, or how far before or past the arena) and records
  `usb dma in LABEL` / `usb dma past arena` / `usb dma before arena` with the offset or
  distance. A firmware that keeps the controller after the handoff is recorded
  (`usb bios kept xhci`).
- Scenario `usb.dma_fault_site` (`usb_fault=dma`): a descriptor read into the page past
  the arena; the kernel's site, the driver's naming and `diag`'s lines are checked, and
  the storage still works.

## Next office run

Boot a dev build with this branch, wait a minute, then `diag diag3.txt` and `acpidump
acpi3.bin`. The report must be read for: the `dma fault 1` block (relation, `below` and
`above` with their distances, the `page` line's memory kind and frame), the `rmrr` lines
(an RMRR for 00:14.0 or none), the `usb dma ...` event from process 8 with its code, and
`usb bios kept xhci` if present. Reading it:

- `page` says loader or boot services memory, runtime services or reserved, and the page is
  far from the own DMA buffer (`below`/`above`): firmware memory, hypothesis 1, and
  `usb bios kept xhci` says whether the firmware kept the controller.
- `page` says RAM free at boot and the frame is in use, far from the arena: a garbage or
  foreign pointer, hypothesis 1.
- "in a region still mapped", or a driver event `usb dma in LABEL`: hypothesis 2.
- No fault at all is weak evidence: the boot enumeration differs from run to run (the
  first run faulted at 6.4 s, the second at 11.5 s). It fits the indirect route through a
  wrapped context, which the placement now prevents, but proves nothing alone; another run
  or two without one are needed before saying so.
