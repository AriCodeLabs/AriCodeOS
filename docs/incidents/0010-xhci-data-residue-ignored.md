# Incident 0010: the xHCI driver ignores the data residue of a passed command

- Opened: 2026-10-18, found by the independent audit of `fix/usb-short-csw`
  (incident 0007), the same family: an acknowledgement the device did not
  fully give. Status: fixed in `fix/usb-short-csw`, pending QEMU run and
  audit.
- Pre-existing: since the storage class joined usbd (ADR 0023, 0.10.0).
- Environment: code reading against BOT 1.0 and xHCI 1.2; no device has
  been seen doing it.

## Mechanism

A Bulk-Only command that passes (`bCSWStatus` 0) may still have moved
fewer bytes than the host asked for: the device says so in
`dCSWDataResidue` (BOT 5.2, 6.7.2 and 6.7.3), and the host's own data
transfer may end on a short packet (xHCI Transfer Event with a non-zero
TRB Transfer Length, 6.4.2.1). A READ that moved fewer bytes leaves the
rest of the client's buffer as it was; a WRITE that moved fewer bytes did
not put every sector on the medium.

`services/usb-input/src/main.rs`, `Controller::scsi`, discarded both: the
data-stage `bulk` result was matched with `if let Err(...)` (its `Ok`
count dropped) and the CSW arm `Ok(Csw { status: Passed, .. }) => Ok(())`
never looked at `residue`. `block_io` then answered the client's READ or
WRITE as done. The EHCI driver (ADR 0031 point 7) has had the right
behaviour from the start: `residue.max(data_len - moved)` and, if it is
not zero, "bytes not transferred", a fault event, a strict halt and the
request answering `Io`.

Until the second fix of incident 0007 the xHCI `bulk` also returned the
event's residual instead of the bytes moved, so this count was not
available to begin with.

Multi-TRB data TDs: a data stage that crosses a 64 KiB boundary is split
into several TRBs (`bulk_trbs`); the Transfer Event's residual counts the
bytes of the TRB that generated it only. `aricode_xhci::td_bytes_moved`
sums the lengths of the TRBs before the first Success or Short Packet
event and adds that TRB's length minus its residual (as Linux's
`sum_trb_lengths`); a short packet in the middle of the TD ends it, so the
later TRBs moved nothing and the last TRB's IOC event, which reports its
whole length as not transferred, does not override the first event.

## Fix

- `aricode_usb_storage::bytes_not_moved(residue, data_len, moved)` (host-
  tested) is the shared decision: the device's residue or the host's
  shortfall, whichever is more; both drivers use it in the Passed arm of
  `scsi`, which now returns the bytes not moved (0 when complete) in the
  xHCI driver as it did in the EHCI driver.
- `block_io` (xHCI) mirrors the EHCI driver: `Ok(0)` is the only success;
  any other count prints `READ(10)|WRITE(10) lba N: K bytes not
  transferred`, records `usb short data` with the count (the name is now
  shared, `fault::SHORT_DATA`; the EHCI driver's `ehci short data` goes),
  halts with the state dump under `strict` (completion code 1, Success:
  the transfer completed, short), and answers `Io`. The transport stays
  Ready: the CSW was valid, so no Reset Recovery is due (BOT 6.7).
  Enumeration commands and `SYNCHRONIZE CACHE` ignore the count as before
  in both drivers (their replies are parsed from the driver's own buffer,
  with their own length checks).
- Regression: `usb_fault=WHEN:short-data` (both drivers) reports the data
  stage of a complete command one sector short at the transport boundary.
  Scenarios `tests/qemu/usb_short_data.toml` (xHCI, failed before the fix
  with the write acknowledged) and `tests/qemu/usb_ehci_short_data.toml`
  (EHCI, keeps the existing rule): `Io` with the count, fault in `diag`,
  no Reset Recovery, the next write succeeds. A device-side residue
  (`dCSWDataResidue`) is not injected: the emulated stick never reports
  one for a complete transfer, and the decision is covered by the host
  test of `bytes_not_moved`.

## Open

- QEMU results for this branch are recorded in incident 0007 once run; the
  audit of the fix is pending.
- No strict-mode scenario for the short data stage in either driver.
- No real device has been seen returning a residue; the ADR 0023 note
  records the rule.
