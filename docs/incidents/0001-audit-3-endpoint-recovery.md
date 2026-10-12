# Incident 0001, audit 3: branch `fix/usb-endpoint-recovery` (third version)

Host tests (15) and clippy pass. **Verdict: not yet safe to merge.**

## Verified closed

N1 (deadline discipline: `command_on`, `control()` and `bulk()` refuse to
start when the deadline is within 5 ms; every command and control goes
through a bounded step; `Refused` is distinct from `Failed` and handled
per BOT 5.3), F4 in the request path (held buffer and pending Disable Slot
released only on completion; no double release), N4, N5, the two F2
cosmetics, the state machine (every state has an exit; nothing strands),
conformance unchanged.

## Mandatory

- **M1 (high, functional). Re-enumeration from the idle loop can never run
  INQUIRY / TEST UNIT READY / READ CAPACITY.** `command_budget` builds a
  budget whose transfer window is `deadline - reserve`; with the 3 s
  maintenance step and the 3 s reserve the window is already in the past
  when `storage_init` runs, so `bulk()` refuses the CBW: "INQUIRY failed"
  three times, `usb given up`, `Gone`. Every path that ends in
  `Reenumerate` (device reset, class reset refused, GET_STATUS timeout,
  give-up, boot-time init failure) ends in a lost disk. Invisible to the
  scenarios, which never re-enumerate. Fix: an enumeration step long
  enough for reserve + CBW + a DATA slice + CSW, a host test that a budget
  built from a short enclosing deadline still yields a window, and a QEMU
  scenario that exercises re-enumeration.
- **M2 (medium). The chain 8 + 3 + 8 = 19 < 20 is not an upper bound**:
  the EP0 recovery and the enumeration cleanup take fresh deadlines inside
  a step (up to 4.5 s), so 20.5 s > the block client's 20 s. Fix: reserve
  the EP0 recovery inside the deadline `control()` is given, bound the
  cleanup by the step, and redo the sum with the real step lengths.
- **M3 (medium). `pending_disable`, `held_buffer` and `retire_failed`
  live in `Storage` and are lost across `self.storage.take()`** during
  re-enumeration: the late Disable Slot completion no longer matches, and
  the held client handle is dropped inside the old `Storage`, leaking the
  frames silently. A Disable Slot completing with a code other than
  Success also leaves the buffer held forever. Fix: keep that bookkeeping
  in `Controller`, release on any Disable Slot completion (recording a
  non-Success code), and wait for a pending Disable Slot before
  enumerating.

## Recommended

- **R1** The transfer window (5 s) is shorter than CBW + DATA + CSW
  (9 s), so the DATA stage never gets its nominal 5 s and the CSW after a
  long WRITE gets 1 s: either say so or size the budget to the sum.
- **R2** `RETRY_MIN_MS` = 1 s is too small: a retry with 1 s of window
  sends the CBW and has DATA refused, manufacturing a Reset Recovery.
  Require at least CBW + a DATA slice + CSW.
- **R3** `enumerate_port` resets the port before checking the deadline.
- **R4** Distinct fault names for pending and refused commands so `diag`
  stays readable.
