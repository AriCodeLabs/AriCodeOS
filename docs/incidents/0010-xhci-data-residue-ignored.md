# Incident 0010: the xHCI driver ignores the data residue of a passed command

- Opened: 2026-10-18, found by the independent audit of `fix/usb-short-csw`
  (incident 0007), the same family: an acknowledgement the device did not
  fully give. Status: closed 2026-10-18, fixed in `fix/usb-short-csw`
  (merged into develop at 397430d; see Closure).
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
into several TRBs (`bulk_trbs`); only the last one carries IOC and ISP, so
the one Transfer Event is the last TRB's. `aricode_xhci::td_bytes_moved`
takes the TD's length minus that event's residual: exact when the
controller reports the whole TD's shortfall on the end TRB (Linux's
reading in `process_bulk_intr_td`: requested minus remaining), and the sum
of the earlier TRBs when it reports only the last TRB's own length (QEMU
on a mid-TD short packet; that TRB's own shortfall is then not reported,
so the count is never too high). An event on an earlier TRB, were a host
to emit one, counts the TRBs before it plus its own bytes (the case Linux
handles with `sum_trb_lengths`). "ISP on the last TRB only" and "the last
TRB's event closes the transfer" are one decision, noted next to
`Trb::normal_part` and `Controller::bulk`.

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

## Second audit (2026-10-18, commits `2c4130a..925cf48`): follow-up

- `td_bytes_moved` overstated the count for a mid-TD short packet under
  Linux's reading (the last TRB's own length minus a TD-wide residual
  clamped to 0 and the earlier TRBs were counted whole): the last TRB now
  uses the TD's length minus the residual (above); host test with both
  readings.
- **Hardware-relevant, to watch on the laptop:** ISP (Interrupt on Short
  Packet) was set on the last TRB of OUT TDs too, since the storage class
  joined usbd (`e042994f`). xHCI 1.0 does not allow it on OUT TRBs and
  Linux sets it for IN only; QEMU ignores it, a real controller may not.
  `Trb::normal_part` now sets ISP for IN TDs only (CBWs and WRITE data
  stages no longer carry it). Not exercised on the laptop yet.
- The short data stage has its own text in both drivers (`… bytes not
  transferred: short data stage (BOT 6.7.2), the request answers Io`; the
  xHCI driver no longer prints "transport failure" for it, and its strict
  header says `halting on a short data stage`) and is recorded as the
  request too, `usb [inj] r|w lba N` with the bytes short as the code, so
  `diag` shows which request and whether it was injected; the scenarios
  assert `usb inj w lba 34 (code 512)` (not yet run after this change).

## QEMU run (2026-10-18, branch at `494954c`)

`cargo xtask test --level full --area usb,init`: 55 scenarios, 0 failed
(see incident 0007 for the whole run). The new scenarios:

```text
usb.short_data (xHCI, 20.8 s)
usb-input: injected fault: the data stage is reported one sector (512 bytes) short
usb-input: WRITE(10) lba 34: 512 bytes not transferred
write: write error
write: 11 bytes to probe.txt
fault reported by process 12: usb short data (code 512)

usb.ehci_short_data (EHCI, 22.4 s)
usb-ehci: injected fault: the data stage is reported one sector (512 bytes) short
usb-ehci: WRITE(10) lba 34: 512 bytes not transferred
write: write error
write: 11 bytes to probe.txt
fault reported by process 12: usb short data (code 512)
```

No Reset Recovery line in either log: the transport stayed Ready and the
second write went through.

## Open

- The audit of the fix (failure protocol step 6) is pending.
- No strict-mode scenario for the short data stage in either driver.
- No real device has been seen returning a residue; the ADR 0023 note
  records the rule.

## Closure (2026-10-18)

Merged into `develop` at 397430d together with the other two fixes of the
0.11.0 audit. On the integrated tree: `cargo xtask check` green, full
QEMU suite 153 scenarios with 0 failed with 1 CPU and again with 4 CPUs,
including `usb.short_data`, `usb.ehci_short_data`, `strict.usb_halt` and `strict.ehci_halt`. Two rounds of independent audit of the fix (findings
recorded above) with every item resolved. Hardware verification on the
laptop is the next step and is tracked in the incident's open items.
