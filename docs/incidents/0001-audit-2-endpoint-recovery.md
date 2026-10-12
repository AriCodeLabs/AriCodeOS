# Incident 0001, audit 2: branch `fix/usb-endpoint-recovery` (second version)

Host tests (14) and clippy pass. **Verdict: not yet safe to merge.**

## Closed

F1, F2, F5, F6, F7, F8, F9, F11, F12, F13, F16: closed as the
specifications require (CLEAR_FEATURE after every TSP = 0 Reset Endpoint on
bulk endpoints with EP0 exempt; `Transport::Enumerating` with a carried
attempt counter; fault names that fit any LBA; fault events for every
recovery anomaly; central Ready/Enumerating guard in `scsi()`; optional
device index with slot cleanup; conditional transitions; ring emptied only
after a successful stop; no Set TR Dequeue after a failed Reset Endpoint;
`strict` halts before any class reset). F3 and F4 only partly, because of
N1.

Two cosmetic leftovers of F2: a second Disable Slot for a slot already
disabled during enumeration (a false `usb cmd fail`), and "back in
service" printed when the Enumerating → Ready transition lost to a port
change.

## New findings

- **N1 (high, mandatory). Steps started with an expired deadline are
  enqueued and never waited for.** `command_on`, `bulk()` and `control()`
  push the TRB, ring the doorbell and return at once when the deadline has
  passed; command timeouts are `min(deadline, now + 500)`, so an expired
  request deadline stays expired. With `DATA_TIMEOUT_MS == REQUEST_BUDGET_MS`
  every DATA timeout returns exactly at the deadline; the following Stop
  Endpoint and Disable Slot are queued, reported as "no completion event"
  (false `usb cmd fail`, `usb ep lost`), and `serve_block` releases the
  client buffer while the DATA TD is still owned by the controller: the
  window F4 was meant to close. A control timeout recovers EP0 with the
  control's own expired deadline, so a stale SETUP is re-executed later.
  Fix: a recovery reserve inside the budget (transfers end at
  `deadline - reserve`; recovery commands may use the reserve); never
  start a transfer or command when the deadline is within a few ms (refuse
  without touching a ring); EP0 recovery with its own deadline; and if the
  Disable Slot after a failed retire does not complete, keep the client
  handle until it does.
- **N2 (medium-high, mandatory). The maintenance-step bound is not real.**
  The GET_STATUS + halt-clearing step can take 2.2 s (2.2 + 1.5 + 1.5 > 5);
  a re-enumeration step can take 3-4 s realistically and more than 10 s in
  the worst case. Fix: a hard per-step deadline threaded through
  re-enumeration (abort and count the attempt), capped halt clearing, or
  enumeration split into idle-loop steps; then state the real bound.
- **N3 (high for the diagnosis, mandatory to resolve). The shortened
  DATA (1500 ms) and CSW (500 ms) timeouts have no specification basis
  and will manufacture failures.** BOT specifies no timeout; Linux uses the
  30 s SCSI command timeout; the incident's evidence says the previous 5 s
  DATA timeout never fired; cheap sticks stall 100 ms to 3 s on writes and
  deliver the WRITE CSW only after accepting the data. A manufactured
  timeout stops a half-finished command, fails the CSW re-read, and
  triggers the Bulk-Only reset that killed the stick in boot 2, while
  `diag` fills with code-0 faults the decision table would misread.
  Resolution: on the client side. The block client's reply wait is an SDK
  constant, not a property of the device: give block (and the file
  service's clients above it) a longer wait, keep the server budget above
  5 s with DATA ≥ 5 s and CSW ≥ 2 s, and cite the reasoning next to the
  constants.
- **N4 (low).** A storage device whose boot-time INQUIRY / TEST UNIT READY
  / READ CAPACITY fails is dropped with no re-enumeration scheduled.
- **N5 (low).** `clear_bulk_halts` hands one 2 s deadline to up to eight
  steps; a slow first step starves the rest into N1 behaviour.

## Scenarios

`usb.fault_retry`, `usb.fault_csw` and `strict.usb_halt` match the code
and QEMU. Not exercised: a timeout retiring a TD in flight (the N1 path),
a DATA-stage failure on a client buffer, port change and re-enumeration. A
host unit test of the deadline arithmetic is recommended.

## Panther Point

No command is issued in an invalid endpoint state; Disable Slot with TDs
pending is legal (xHCI 4.6.4). The one hazard is N1 (queued commands not
waited for, buffer released early).

## Required before merge

N1, N2, N3; recommended: the two F2 cosmetics, N4, N5.
