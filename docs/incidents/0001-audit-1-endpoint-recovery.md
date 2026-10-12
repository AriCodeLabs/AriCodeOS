# Incident 0001, audit 1: branch `fix/usb-endpoint-recovery` (first version)

Read-only audit of the implementation against the incident, the three
studies, xHCI 1.1, USB 2.0 and BOT 1.0. Host tests (13) and clippy pass.

**Verdict: not safe to merge as is.** The xHCI state machine, the Stop
Endpoint / Set TR Dequeue handling, the instrumentation and the Link TRB
fix are correct and a large improvement. Four findings block the merge.

## High

- **F1 Data toggle desynchronised on the code-4 path.** The task text said
  "CLEAR_FEATURE only after STALL"; that was wrong. Reset Endpoint with
  TSP = 0 resets the host toggle (xHCI 4.6.8); only `ClearFeature(ENDPOINT_HALT)`
  resets the device's (USB 2.0 9.4.5); mismatched packets are ACKed and
  discarded (8.6.4). After a halting code other than STALL the branch
  reads the CSW and, if valid, stays `Ready` without any Reset Recovery, so
  the next CBW or CSW is silently dropped half of the time: the very
  mechanism of study A H-B. Fix: `ResetHalted { clear_halt: true }` for
  every halting code on the TSP = 0 path; EP0 stays exempt (8.5.3.4).
- **F2 The recovery re-enters itself during re-enumeration**
  (`reenumerate_storage` → `enumerate_port` → `storage_init` → `scsi` →
  `transfer_failure` → `begin_reset_recovery` → `reenumerate_storage`):
  a half-initialised `Storage` (capacity 0) makes a successful nested
  enumeration look like a different medium, the working device is
  discarded on unwind, and `attempts` is reset to 0 on every failure path,
  so only the DMA arena bounds the loop. Fix: an `enumerating` state in
  which `transfer_failure` does only the endpoint recovery; `storage_init`
  failures propagate to the single outer loop, which counts attempts.
- **F3 The request budget does not bound what a client waits for.** The
  client waits 5 s from send; the server's 4 s starts at receive;
  `maintain_storage` holds the loop up to 2 s; recovery steps inside a
  request overrun the budget (control transfers, halt clearing, and
  re-enumeration with INQUIRY / TEST UNIT READY loops reachable from
  `block_io`). A client that gave up reuses its buffer while the server
  starts the stale request: a stale READ overwrites what the client now
  uses, a stale WRITE puts new data at the old LBA. Fix: never run
  re-enumeration, the TEST UNIT READY loop or halt clearing from a block
  request (answer `Io`, leave it to the idle loop); cap the maintenance
  slice to one poll per iteration; make `bulk`, `control` and `command`
  respect the deadline in the request path; lower the budget so budget +
  maintenance + one queued request < 5 s; follow-up: carry the client's
  deadline in the block protocol. Rewrite the SDK comment honestly.
- **F4 A client buffer can be released with its TD still owned by the
  controller** when Stop Endpoint or Set TR Dequeue fails after a DATA
  timeout; the Disable Slot that would stop the DMA comes later. Fix:
  Disable Slot before returning in that case, or hold the client handle
  until the slot is disabled.

## Medium

- **F5** Fault names are truncated at 24 bytes: `usb inj write lba ` leaves
  6 digits; LBAs above 999999 are recorded wrong. Shorter prefixes and a
  unit test that the longest prefix plus 10 digits fits.
- **F6** Printed but not recorded as fault events: the second CSW read
  failing, failed recovery commands, failed CLEAR_FEATURE / GET_STATUS /
  class reset, device lost, each re-enumeration and its outcome, "storage
  given up", REQUEST SENSE transport failures. `diag` must show them.
- **F7** `scsi()` does not check the transport is `Ready`; the unit
  attention retry can re-send a CBW after `sense()` left the transport in
  recovery (BOT 5.3). One central guard.
- **F8** `Storage.device` outlives the slot and can point at a HID device
  enumerated later; `enumerate_port` failures after Enable Slot leak the
  slot (MaxSlots 32 on Panther Point). `device: Option<usize>` cleared on
  Disable Slot; disable the slot on every enumeration error.
- **F9** `Ready` written unconditionally over a `Reenumerate` / `Gone`
  that a Port Status Change set during a recovery command.
- **F10** DMA arena leak per re-enumeration (5-6 KiB of 64 KiB): use Reset
  Device (xHCI 4.6.11) for a device still on the port and reset rings in
  place instead of Disable Slot + fresh allocations.
- **F11** `clear_bulk_halts` zeroes the ring before checking that Stop
  Endpoint succeeded.

## Low

- **F12** Set TR Dequeue issued to a still-Halted endpoint when Reset
  Endpoint failed (guaranteed Context State Error).
- **F13** `strict` halts after the whole recovery, class reset included;
  acceptable for the decision table, but consider halting after the xHCI
  endpoint recovery and before the BOT reset, the step that killed the
  stick in boot 2.
- **F14** Pre-existing: HID transfer events ignore the completion code; a
  halted interrupt endpoint kills the keyboard silently (own incident).
- **F15** Panther Point's spurious Success after a Short Packet will show
  as a "stray transfer event" line on hardware; the scenario's absent
  string is QEMU-only.
- **F16** Duplicated printing in `stop_endpoint`, a `too_many_arguments`
  allow, a dead-code suppressor, a formatter that could be shared.
- **F17** The `shell.write_persists` expectation (7 → 6 files) is
  legitimate (sshd writes no seed without keys) but unrelated; own commit.

## Conformance confirmed

Reset Endpoint only when Halted; TSP encoding, version gate and Soft
Retry; Set TR Dequeue preconditions and pointer; Stop Endpoint and its
events; Configure Endpoint Drop + Add for the toggle of a non-halted
endpoint; Reset Recovery order and both-side toggle reset in
`clear_bulk_halts`; CSW read after a data-phase failure (acceptable only
with F1); CBW failure → Reset Recovery; re-enumeration order; Link TRB
Chain bit; doorbell discipline; EP0 recovery. Not exercised in QEMU: Stop
Endpoint retiring a TD in flight, a DATA-stage failure on a client buffer,
the budget / undelivered-reply path, port change and re-enumeration (QMP
`device_del` / `device_add` could), Soft Retry.

## Required before merge

F1, F2, F3, F4; strongly recommended in the same change: F5, F6, F7, F8.
