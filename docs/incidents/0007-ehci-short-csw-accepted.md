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

## Open

- The two QEMU scenarios and the full USB suite have not been run yet on
  this branch (another process owned the QEMU ports); the audit of the fix
  (failure protocol step 6) is pending.
- No strict-mode scenario for the short CSW exists; strict behaviour is
  the shared `ScsiFail::Csw` path exercised by the code, not by a test.
- Physical device reproduction and actual data-loss measurement remain
  unperformed.
