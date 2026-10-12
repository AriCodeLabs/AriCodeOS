# Incident 0001, audit 4: branch `fix/usb-endpoint-recovery` (fourth version)

Host tests (15) and clippy pass. **Verdict: safe to merge; nothing
mandatory remains.**

## Closed and re-verified

- **M1** `Budget::within` keeps a transfer window of at least CBW + 1 s +
  CSW (5 s) by shrinking the reserve, or refuses the command up front;
  `storage_init` refuses each SCSI command with a distinct message when no
  window fits. In the 6 s re-enumeration step INQUIRY gets about 5 s; the
  new scenario would fail at "INQUIRY failed" otherwise.
- **M2** Chain recomputed by the auditor with every deadline: request
  path 12.1 s worst case (no fresh deadline inside it; EP0 recovery is
  reserved inside the control deadline); maintenance steps: slot fallback
  0.5 s, class reset 2 s, GET_STATUS + halt clearing 3 s, re-enumeration
  6.1 s. Queued behind one request and one step: 12.1 + 6.1 + 12.1 ≈
  30.3 s < 35 s (block client) < 45 s (file client). Assumes at most two
  concurrent block clients.
- **M3** Bookkeeping on `Controller` survives `storage.take()`; a Disable
  Slot completing with any code releases the held handle (non-Success
  recorded); re-enumeration waits while a Disable Slot is pending or
  queued, without counting an attempt; no double release; the Gone arm is
  subsumed by the tracked path.
- **R1-R4** window = 1 + 5 + 3 s exactly; retry needs CBW + 1 s + CSW;
  the port reset is guarded by the deadline; distinct `usb cmd pending` /
  `usb cmd refused` events.
- **Scenario `usb.reenum`** exercises give-up → Disable Slot → request
  answered `Io` → idle-loop re-enumeration (port reset, Enable Slot,
  Address Device, descriptors, SET_CONFIGURATION, Configure Endpoint,
  INQUIRY / TEST UNIT READY / READ CAPACITY) → "back in service" → the
  next write succeeds; its absent strings catch an M1-type regression.
  Only the synchronous Disable Slot path is reachable in QEMU.
- No regressions found: every transfer, command and control goes through
  a refuse check; every transport state has an exit; nothing unlogged; no
  command issued in an invalid xHCI state.

## Recommendations (not blocking)

- R-a: say that the chain assumes at most two concurrent block clients.
- R-b: the "waits for the pending Disable Slot" line prints every idle
  iteration; print once or push `not_before` forward.
- R-c: set `retire_failed` only when a Disable Slot was actually issued.
- R-d: a non-Success Disable Slot records `usb cmd fail` twice.
- R-e: "no response" printed after a refusal already printed.

Open, unchanged: F10, F14, F15, F17 (separate commit).
