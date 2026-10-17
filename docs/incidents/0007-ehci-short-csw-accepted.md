# Incident 0007: the USB storage drivers accept a short command-status wrapper

- Opened: 2026-10-18. Status: fixed in `fix/usb-short-csw`, pending QEMU
  run and audit.
- Reviewed commit: `525b550`; introduced with the EHCI driver in 0.11.0
  (the xHCI driver has had the same omission since ADR 0023).
- Environment: extracted production `Ehci::scsi`, simulated bulk transport.
- Evidence: [release audit, finding 1](../audits/2026-10-18-release-0.11.0-525b550.md#1-p1-ehci-acknowledges-a-write-without-receiving-a-complete-csw),
  [probe output](../audits/525b550-review/probes.log).

## What was observed

On a WRITE(10), the mock completed the data phase and transferred only the
CSW signature and current tag (8 bytes). The existing residue/status bytes
were zero. `scsi` returned `Ok(0)` and performed no Reset Recovery.

## Mechanism

A Command Status Wrapper is exactly 13 bytes (USB Mass Storage Bulk-Only
Transport 1.0, 5.2); a CSW of any other length is not valid and requires a
Reset Recovery (6.3.1, 5.3.4). Both drivers read the CSW with a bulk IN
transfer whose result carries the bytes moved, ignored the count, read the
whole 13-byte persistent CSW buffer and handed it to
`aricode_usb_storage::check_csw`, which only validates signature, tag and
status byte:

- EHCI, `services/usb-ehci/src/main.rs`, `Ehci::scsi` (CSW stage) and the
  recovery CSW read in `transfer_failure` (BOT 6.7.2).
- xHCI, `services/usb-input/src/main.rs`, `Controller::scsi` (CSW stage)
  and the recovery CSW read in `transfer_failure`. Found while fixing the
  EHCI driver: the same `Ok(_)` on the moved count, the same full-buffer
  read.

A device that delivers only the signature and the tag rewrites the first 8
bytes of the buffer; the residue and status are those of the previous
command. After a passed command they say "residue 0, Passed", so a WRITE
that received no status is acknowledged to fsd as written. The recovery
path could likewise close a failed command on stale bytes.

## Fix

- `libs/usb-storage`: `check_csw_transfer(moved, raw, tag)` rejects any
  count but 13 with the new `CswError::Length(moved)` before a field is
  read; `CswError::event_code()` gives the fault event its code (the bytes
  moved). Host tests cover 0, 8, 12, 13 and 14 bytes over a buffer holding
  a previous Passed and a previous Failed status, and keep the tag
  mismatch, phase error and signature checks at 13 bytes.
- Both drivers read every CSW through one `read_csw` function: it fills
  the buffer with 0xFF (not a valid status byte) before the transfer, so no
  previous status can survive even if a future caller forgot the count,
  returns the bytes moved with the buffer, and the callers check them with
  `check_csw_transfer`, in the normal and the recovery paths. A wrong
  length is reported (`CSW of N bytes, not 13 (BOT 5.2)`), recorded
  (`ehci csw invalid` / `usb csw invalid`, code N; the record now covers
  the recovery path too) and followed by the existing Reset Recovery; with
  `strict` the request halts the driver with its state dump like the other
  invalid-CSW cases, before any recovery.
- Regression: `usb_fault=WHEN:short-csw` makes the CSW read of a complete
  command look, at the transport boundary, as if the device moved 8 bytes
  over the previous command's zero residue and Passed status. Scenarios
  `tests/qemu/usb_short_csw.toml` (xHCI) and
  `tests/qemu/usb_ehci_short_csw.toml` (EHCI): the request answers Io, the
  fault is in `diag`, the Reset Recovery runs and the next write succeeds.

## Audit of the fix (2026-10-18, commit `59bc075`)

The independent audit of the first fix found a P0, confirmed by a QEMU
run: `init.no_selftest_usb` failed, `usb-input: USB storage on port` never
appeared. In the xHCI driver `Controller::bulk` returned `Ok(residual)`,
the Transfer Event's TRB Transfer Length (xHCI 6.4.2.1: bytes **not**
transferred), and the new `read_csw` handed that to `check_csw_transfer`
as the count moved. Every complete 13-byte CSW became `Length(0)`, a
Reset Recovery ran on the first INQUIRY and the storage never came up.
The EHCI `bulk` sums the bytes moved over its retired qTDs, so that driver
was right. The library test could not see it: the drivers are not
host-tested and the residual-to-moved translation lived in the driver.

Second fix (same branch):

- `aricode_xhci::td_bytes_moved(lens, event_trb, residual)` (host-tested)
  turns the Success or Short Packet event that ends a TD into bytes moved.
  Only the last TRB carries IOC and ISP, so the event is the last TRB's
  and the count is the TD's length minus its residual: exact when the
  controller reports the TD's whole shortfall there (Linux's reading,
  `process_bulk_intr_td`: requested minus remaining) and the earlier TRBs'
  sum when it reports only the last TRB's own length (QEMU on a mid-TD
  short packet). An event on an earlier TRB, were a host to emit one,
  counts the TRBs before it plus its own bytes (Linux's `sum_trb_lengths`
  case). Incident 0010 has the detail. `Controller::bulk` now returns
  `Ok(bytes moved)` like the EHCI driver, `Trb::transfer_length` reads the
  TRB field.
- `CswError::Length(0)` had the event code 0, indistinguishable from
  `Invalid` in `diag`: a wrong length is now its own event,
  `usb csw length` with the bytes moved as the code, and the rest is
  `usb csw invalid`; the names (`fault::CSW_LENGTH`, `fault::CSW_INVALID`)
  and the console lines (`CswError::describe`, host-tested) are shared by
  both drivers, which previously printed slightly different texts. The
  EHCI driver drops its own `ehci csw invalid` name for the shared one, as
  it already uses the shared `usb r|w lba` and `usb sense` names.

The audit also found a pre-existing fault of the same family in the xHCI
driver, the data-stage residual and `dCSWDataResidue` ignored: incident
0010.

## QEMU run (2026-10-18, branch at `494954c`)

`cargo xtask test --level full --area usb,init`: 55 scenarios, 0 failed
(`init.no_selftest_usb` 5.3 s again brings the USB stick up; the whole
`usb` and `init` areas pass, `strict.ehci_halt`, `strict.usb_halt` and
`strict.usb_hid_halt` included). A first run in the same worktree had 28
scenarios fail at 0.0 s for an unrelated reason, the QMP socket path of a
deep git worktree exceeding the 107 bytes of a UNIX socket address; xtask
now falls back to a short path (`qmp_socket_path`).

Evidence from the new scenarios' serial logs:

```text
usb.short_csw (xHCI, 22.7 s)
usb-input: injected fault: the CSW read moved 8 bytes; the previous command's status stays in the buffer
usb-input: CSW of 8 bytes, not 13 (BOT 5.2): invalid, no status for tag 45
usb-input: reset recovery (BOT 5.3.4) pending: the class reset is sent when idle
write: write error
usb-input: reset recovery (BOT 5.3.4): Bulk-Only Mass Storage Reset
usb-input: reset recovery done: transport ready
write: 11 bytes to probe.txt
fault reported by process 12: usb csw length (code 8)
fault reported by process 12: usb inj w lba 34 (code 0)

usb.ehci_short_csw (EHCI, 22.4 s)
usb-ehci: injected fault: the CSW read moved 8 bytes; the previous command's status stays in the buffer
usb-ehci: CSW of 8 bytes, not 13 (BOT 5.2): invalid, no status for tag 46
usb-ehci: Reset Recovery (BOT 5.3.4): mass storage reset, both bulk halts cleared
usb-ehci: Reset Recovery done; transport ready
write: write error
write: 11 bytes to probe.txt
fault reported by process 12: usb csw length (code 8)
fault reported by process 12: ehci reset recovery (code 0)
```

## Open

- The audit of the second fix (failure protocol step 6) is pending.
- No strict-mode scenario for the short CSW exists; strict behaviour is
  the shared `ScsiFail::Csw` path exercised by the code, not by a test.
- Physical device reproduction and actual data-loss measurement remain
  unperformed.
