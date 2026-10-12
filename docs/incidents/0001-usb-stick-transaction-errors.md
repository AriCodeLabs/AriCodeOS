# Incident 0001: USB stick bulk transfers fail with transaction errors

- **Opened:** 2026-10-12. **Status:** open; driver fixes merged (`develop` 9bd36e8), root cause awaiting the laptop dump.
- **Hardware:** ASUS laptop, Intel 7 series chipset, xHCI 8086:1e31 (MSI,
  16 scratchpads, Intel port routing), no IOMMU, 4 CPUs, 3981 MiB. Boot
  stick "USB Disk 3.0", 30433280 sectors, rejects `SYNCHRONIZE CACHE`.
- **Software:** images built from `develop` 653e12f (2026-10-11, network
  updates) through acc8eaf (2026-10-12). The image from fc846ae
  (2026-10-10/11) ran a whole day on the same stick and port without a
  single transfer error, including writes (`write`, `diag NAME`).

## What was seen

Photos transcribed. `usb-input` is the xHCI driver serving the stick as
the block service; `fsd` and the `updater` are its clients.

1. Boot of 653e12f: right after `sshd` wrote `random_seed` (its first
   write) and read `authorized_keys`:
   `usb-input: READ(10) lba 133122 transport failure`,
   `usb-input: READ(10) lba 133446 transport failure`, then
   `sshd: no authorized_keys in AriFS: not listening`. Later `update check`
   read the whole active slot (2 MiB) without errors. Then `update`
   (a new block client) failed on its first command:
   `READ(10) lba 1 transport failure`, and every later `update` too.
2. Boot of cc4ffdd (driver now prints the completion code and performed a
   Bulk-Only Mass Storage Reset plus `CLEAR_FEATURE(HALT)` on both bulk
   endpoints, then retried): `transfer failed (completion code 4), reset
   recovery` repeated for every command; nothing recovered until power off.
   Completion code 4 = USB Transaction Error (xHCI 1.x, 6.4.5).
3. Boot of 8590845 and acc8eaf (retries with pauses 50 ms to 2 s, reset on
   the third failure): the system came up, `sshd` listened, the network
   update downloaded and installed versions 4 and 5 (2.3 MiB written to
   `UPDATE`, then copied to the slot) and both booted. Afterwards reads
   failed again intermittently: `update check` could not read LBA 1
   ("no UPDATE partition"), the updater's `last.log` was created but its
   content write failed (0 bytes), while `ls` through `fsd` worked seconds
   later. `sshd` answered the SSH banner only after about 20 s, the time
   its `authorized_keys` reread spent in the driver's retries.

## Facts established

- The failure is a USB Transaction Error reported by the controller, not a
  SCSI rejection (no sense data) and not a timeout of our 5 s data phase.
- It appears after writes (small or large) and sometimes on the first
  command of a new block client; megabytes of reads in between succeed.
- The Bulk-Only reset recovery made the device stop answering entirely.
- QEMU (virtio and emulated xHCI mass storage) never shows it.
- The previous day's image on the same hardware never showed it.

## Hypotheses (to confirm or refute; see the three study reports)

- H1 The stick needs time after a write and answers with no handshake
  while programming flash; our driver issues the next command at once.
- H2 The transfer ring or endpoint state handling (link TRB, cycle bit,
  Reset Endpoint / Set TR Dequeue after an error, events left over after a
  timeout) turns one error into a wedged endpoint.
- H3 A change between fc846ae and HEAD altered timing, interrupts or DMA
  buffer lifetime (random stirring from interrupt handlers, per-request
  buffer mapping, a client exiting with a request in flight, early writes
  by `sshd`).
- H4 The device's power or link (USB 3.0 stick on a USB 2.0 port) is
  marginal and the previous day was luck.

## Study reports (2026-10-12)

Three independent read-only reviews, each in its own file:

- [Study A: xHCI transfer path](0001-study-A-xhci-transfer-path.md)
- [Study B: Bulk-Only Transport and SCSI layer](0001-study-B-bulk-only-transport.md)
- [Study C: repository changes fc846ae..HEAD](0001-study-C-repository-changes.md)

They converge:

1. The transfer path (TRBs, rings, cycle bits, event ring) is correct;
   the trigger of the first code 4 is at the device or physical layer,
   right after writes (study C: the new workload writes and then reads at
   machine speed; the previous day was human-paced).
2. The driver turns one error into a disaster: after a transaction error
   on the IN endpoint it never un-halts it (only STALL is handled, and
   only on some stages), so later transfers time out at 5 s (xHCI
   4.10.2.1, 4.8.3); it sends a new CBW while the previous command is
   outstanding (BOT 5.3, 6.7); the recovery resets endpoints that are not
   halted, ignores every completion code, desynchronises the data toggle
   with `CLEAR_FEATURE(HALT)` (USB 2.0 9.4.5, xHCI 4.6.8) and never
   recovers EP0; timed-out TDs are never retired (no Stop Endpoint) and
   error events are matched without checking the TRB pointer; Port Status
   Change events are discarded, so a device that resets itself on the
   Bulk-Only reset (what the persistent code 4 looks like) is never
   re-enumerated.
3. The retry commits added a second defect: the server's retry budget
   exceeds the client's 5 s reply timeout, so clients time out and reuse
   their buffer while the server still works on it (the empty `last.log`,
   "no UPDATE partition" while `ls` works, the 20 s SSH banner).
4. Latent: the Link TRB never carries the Chain bit for a TD that spans
   the ring wrap (benign today).
5. Dismissed as causes: the random stirring from interrupt handlers, the
   RTC read, the idempotent mapping, the per-request buffer mapping.

## Plan (per the failure protocol)

1. Same-day A/B on the stick: the fc846ae image with a scripted
   write-then-read, then HEAD with the same sequence, to separate "the
   workload exposes the stick" from "hardware degraded".
2. Instrumentation in the `strict` dump and the fault events: stage,
   direction, DCI, completion code, event TRB pointer versus the TD,
   residual, Endpoint Context state and dequeue pointer, PORTSC decoded,
   Port Status Change events, completion codes of every recovery command,
   both tags on a CSW mismatch, port speed and descriptor data at
   enumeration, USBSTS.
3. Fixes mandated by the specifications, each with a host test of the
   state machine: recover the halted endpoint at every stage for every
   halting code; Reset Endpoint only when Halted and check every command
   completion; Stop Endpoint before reusing a ring after a timeout and
   before releasing a client buffer; match events by TRB pointer; recover
   EP0; read the CSW or Reset Recovery before any new CBW; wait after the
   Bulk-Only reset; port reset and re-enumeration as the last resort;
   Chain bit on the Link TRB; server budget below the client's wait and a
   fault event when a reply cannot be delivered.
4. Only then decide, with the laptop dump, whether a post-write quiet
   period is a documented workaround or unnecessary.

## Instrumentation landed (2026-10-12, first step)

- `strict` boot flag: the first transport failure dumps the driver state
  (completion code, direction, endpoint, LBA, count, ring positions,
  attempt number) and the driver exits; init does not restart it.
- Every transport failure, retried or not, is a fault event in the kernel
  ring (`diag`).

## Specification-mandated handling and instrumentation (2026-10-12, second step)

Branch `fix/usb-endpoint-recovery`, revised after
[audit 1](0001-audit-1-endpoint-recovery.md). What the driver
(`services/usb-input`), `libs/xhci` and `libs/usb-storage` do now, with the
section each step comes from written next to the code:

1. **Instrumentation.** Every failed `bulk()` (timeouts included, code 0)
   prints one line with the stage (CBW/DATA/CSW), direction, DCI,
   completion code and its name, elapsed ms, residual and event TRB
   pointer against the TD's first and last TRB, then the Endpoint Context
   state and TR Dequeue Pointer of DCI 1 and both bulk DCIs (read from the
   Output Device Context, kept per device), PORTSC of the storage port
   decoded (CCS, PED, OCA, PR, PLS, PP, speed, CSC, PEC, WRC, OCC, PRC,
   PLC, CEC) and USBSTS with HSE/HCE. The `strict` halt dump adds the same
   block after the xHCI endpoint recovery and before any BOT reset. Every
   Port Status Change event prints its port and PORTSC before the change
   bits are cleared. Storage enumeration prints VID:PID, bcdUSB, bMaxPower
   and the port speed (PSIV). Every Reset Endpoint, Set TR Dequeue, Stop
   Endpoint, Configure Endpoint (drop + add) and Disable Slot prints its
   completion code; every control transfer used in recovery prints its
   result; a CSW tag mismatch prints both tags with the status and residue.
2. **Endpoint state machine** (`aricode_xhci::endpoint_recovery`, host
   tested). On any non-success completion on a bulk endpoint, at any
   stage, the endpoint that reported it is recovered according to its
   context state (xHCI 4.8.3): Halted takes Reset Endpoint (4.6.8), Set TR
   Dequeue to the software enqueue pointer (4.6.10, only once the reset
   succeeded) and `CLEAR_FEATURE(ENDPOINT_HALT)` for every halting code,
   not only STALL: the reset restarts the host's data toggle at DATA0 and
   only that request restarts the device's (USB 2.0 9.4.5), otherwise
   mismatched packets are ACKed and discarded (8.6.4). EP0 is exempt
   (8.5.3.4). Running with no event takes Stop Endpoint (4.6.9) and Set TR
   Dequeue; Stopped takes Set TR Dequeue; Disabled or Error ends in
   re-enumeration. Reset Endpoint is never sent to an endpoint that is not
   Halted. Soft Retry (4.6.8.1: Reset Endpoint TSP = 1, doorbell, the TD
   re-executes) is attempted once for a USB Transaction Error on
   controllers of version 1.1 or later, where TSP exists; on 1.0 (the
   laptop's Panther Point) or if the command fails, the TSP = 0 path
   follows. EP0 (DCI 1) is recovered the same way after any failed
   control transfer, and `control()` accepts error events that point at
   the Setup or Data Stage TRB. When no command puts the endpoint back,
   the slot is disabled at once (4.6.4), so the controller owns no TD on a
   client buffer when the buffer is released.
3. **Timeouts and budget** (revised after audits 2 and 3). Stage timeouts
   are specification-sane: CBW 1000 ms, DATA 5000 ms, CSW 3000 ms (BOT 1.0
   specifies none; Linux works under the 30 s SCSI command timeout;
   `develop`'s 5 s DATA timeout never fired in this incident; cheap sticks
   stall 100 ms to 3 s on writes). A timed-out TD is retired with Stop
   Endpoint (the Stopped Transfer Event, code 26/27/28, is consumed) and
   Set TR Dequeue before the ring is used again and before `serve_block`
   releases the client buffer. Transfer Events are matched by TRB pointer
   within the current TD; others are printed as stray and ignored. The
   server budget per request is `REQUEST_BUDGET_MS` = 12000 ms with
   `RECOVERY_RESERVE_MS` = 3000 ms inside it (`aricode_xhci::Budget`, host
   tested): the transfer window is 9000 ms = CBW + DATA + CSW exactly;
   recovery commands may run until the deadline; no transfer, command or
   control transfer starts with less than `MIN_START_MS` (5 ms) left, it is
   refused without touching a ring and logged (`usb cmd refused`; a command
   enqueued but not completed is `usb cmd pending`). `control()` reserves
   `EP0_RECOVERY_MS` inside the deadline it is given, so its EP0 recovery
   never overruns a step. The single retry after a recovery needs a window
   of `RETRY_MIN_MS` = CBW + 1 s + CSW = 5000 ms. If a TD could not be
   retired, Disable Slot is issued at once (xHCI 4.6.4); until its
   completion is seen (any code; a non-Success one is recorded) the client
   buffer handle is held by the controller state (`Controller::held_buffer`,
   kept across re-enumerations) and a refused Disable Slot is retried by
   the idle loop first thing (`slot_to_disable`); no re-enumeration starts
   while a Disable Slot is pending. Everything slower than the endpoint
   recovery runs from the idle loop (`maintain_storage`), one step per
   iteration: the class reset, a GET_STATUS poll plus halt clearing (each
   sub-step gets the lesser of the step deadline and its own timeout) within
   `MAINTENANCE_STEP_MS` = 3000 ms, and a whole re-enumeration within
   `REENUMERATE_STEP_MS` = 6000 ms (aborted and counted as a failed attempt
   when the deadline passes; its SCSI commands get a budget built from the
   step with a window of at least CBW + 1 s + CSW, `Budget::within`, host
   tested, so INQUIRY / TEST UNIT READY / READ CAPACITY always have room to
   run). Chain of waits, resolved on the client side: the block client
   waits `aricode_sys::block::REPLY_WAIT_MS` = 35 s > budget (12 s) +
   longest maintenance step (6 s) + one queued request (12 s) = 30 s; the
   file client waits `fs::REPLY_WAIT_MS` = 45 s > 35 s plus the file
   service's own work. A slow disk is latency for the caller, never a
   client giving up while the server holds its buffer. A reply that cannot
   be delivered records `usb reply undelivered`. Still open: carrying the
   client's deadline in the block protocol.
4. **Bulk-Only Transport** (`aricode_usb_storage::after_transfer_failure`,
   `check_csw`, host tested). `scsi()` sends no CBW unless the transport is
   Ready (or the device is being enumerated). After a data or CSW failure
   and the endpoint recovery the CSW is read once (BOT 6.7.2/6.7.3); a
   valid CSW closes the command and the request may be retried once,
   otherwise a Reset Recovery (BOT 5.3.4) is requested. A CBW failure goes
   straight to Reset Recovery (BOT 6.6.1: the device may hold the
   command). Reset Recovery, from the idle loop: Bulk-Only Mass Storage
   Reset, GET_STATUS on EP0 every 200 ms for up to 6 s until the device
   answers, then both halts cleared: on the host a Halted endpoint takes
   Reset Endpoint + Set TR Dequeue and a Running or Stopped one takes Stop
   Endpoint + Configure Endpoint with Drop + Add of its DCI on an emptied
   ring (xHCI 4.6.8 note; the ring is emptied only once the controller
   stopped following it); on the device `CLEAR_FEATURE(ENDPOINT_HALT)` on
   both pipes. Every result is checked and recorded; a failure escalates.
   A state only becomes Ready if it still is the one the step started
   from: a port change seen meanwhile wins.
5. **Port events.** CSC/PRC/WRC on the storage port marks the storage
   unusable (requests answer `Io` at once, fault `usb dev lost`) and, with
   CCS still set, re-enumerates it after the 100 ms debounce (Disable
   Slot, Enable Slot, Address Device, descriptors, SET_CONFIGURATION,
   Configure Endpoint, INQUIRY / TEST UNIT READY / READ CAPACITY); it is
   served again only with the same capacity. During the enumeration the
   transport is `Enumerating`: a failed transfer only recovers its endpoint
   and the enumeration fails, counted by the single outer loop. Port reset
   plus re-enumeration is the last resort when a recovery command or the
   class reset fails; consecutive attempts are counted across recoveries
   (reset by a successful command or a new connection) and the storage is
   given up after three. Every enumeration failure after Enable Slot
   disables the slot again, exactly once. A storage device whose boot-time
   INQUIRY / TEST UNIT READY / READ CAPACITY fails is kept and scheduled
   for re-enumeration instead of being dropped.
6. **Link TRB Chain bit.** `ProducerRing::push` copies the Chain bit of a
   TRB enqueued just before the wrap onto the Link TRB (xHCI 4.11.5.1);
   unit test in `libs/xhci`.
7. **Retries and fault events.** The pause-and-retry schedule is gone. A
   command is retried once only when the transport is still Ready after
   the recovery and at least 0.5 s of budget remains; a unit attention is
   retried once (SPC-4 4.5.6). Every failed attempt is a fault event
   (`usb r lba N`, `usb w lba N`, `usb inj r/w lba N`: prefixes short
   enough for any READ(10) LBA, host tested in `aricode_usb_storage::fault`),
   as are every failed recovery command (`usb cmd fail`), failed
   CLEAR_FEATURE / class reset / first unanswered GET_STATUS and its
   timeout, the failed second CSW read (`usb csw2 fail`), REQUEST SENSE
   transport failures, an endpoint given up, each re-enumeration and its
   outcome (`usb reenum`, `usb reenum ok|fail`) and `usb given up`.
   `strict` halts on the first failure. `usb_fault=N|idle` keeps injecting
   a transaction error on the CBW; `usb_fault=N:csw|idle:csw` reads a CSW
   with no command outstanding, so the emulated stick STALLs its bulk IN
   pipe and the Halted path runs in QEMU (scenario `usb.fault_csw`);
   `usb_fault=N:reenum|idle:reenum` gives the endpoint up on that request,
   so Disable Slot, the port reset and the whole re-enumeration run in QEMU
   (scenario `usb.reenum`).

What the laptop dump should now show, per the decision table of study A:

- On the first failure: the stage and direction, the code, and the state
  of the failed DCI. `Halted` with code 4 confirms the amplifier (H-A);
  `Running` with PORTSC clean and the CBW stage points at the device.
- The completion code of each Reset Endpoint / Set TR Dequeue; no 19
  (Context State Error) is expected any more.
- `port N status change` lines with CSC/PRC around the class reset would
  confirm H-C; the re-enumeration lines follow.
- `GET_STATUS` answered N ms after the class reset: how long the stick
  needs after a Bulk-Only reset.
- Whether the single retry after the recovery succeeds: if it does, the
  trigger is a transient device-side error and no quiet period is needed;
  if the next command fails again right after a write, the post-write
  behaviour of the stick (H1 / R1) is confirmed.

## Open

- Root cause not established: the trigger of the first code 4 needs the
  laptop dump with the instrumentation above (the same-day A/B of the plan
  is still pending).
- Soft Retry cannot be exercised on the laptop (xHCI 1.0) nor in QEMU (no
  transaction errors); the TSP = 0 path is what runs there.
- Not exercised in QEMU: Stop Endpoint retiring a TD in flight, a
  DATA-stage failure on a client buffer, the undelivered-reply path, a
  device-initiated port change (the scenario runner's QMP client only
  injects keys; `device_del` / `device_add` would need xtask support).
  Re-enumeration itself is exercised by `usb.reenum` through the injected
  give-up.
- Audit 1 F10 (Reset Device instead of Disable Slot + fresh allocations)
  and F14 (halted HID interrupt endpoints) remain open.
- The DMA arena is linear: each re-enumeration leaves the old slot's
  contexts and rings allocated (about 6 KiB), which bounds re-enumerations
  to roughly ten per boot before "no DMA".
- A post-write quiet period is still not implemented: it is decided after
  the dump, as planned.

## Audits

Four audit rounds of the driver branch, each in its own file:
[audit 1](0001-audit-1-endpoint-recovery.md) (not mergeable: toggle
desynchronisation, re-entrant recovery, request budget, buffer release),
[audit 2](0001-audit-2-endpoint-recovery.md) (steps started with expired
deadlines, unreal step bound, unjustified timeouts), [audit 3](0001-audit-3-endpoint-recovery.md)
(re-enumeration could never run a SCSI command, chain arithmetic,
bookkeeping lost across storage replacement), [audit 4](0001-audit-4-endpoint-recovery.md)
(safe to merge). Merged into `develop` as 9bd36e8 on 2026-10-12.

## Next

Boot the laptop from the stick built with `strict` and run the
write-then-read sequence; the first transport failure halts the driver
with the full dump, which the decision table of study A reads. The A/B
with the fc846ae image (built, in a separate worktree) follows if the
dump does not settle the trigger.

## Laptop run with the merged driver and `strict` (2026-10-12, develop 9bd36e8)

Sequence driven over SSH at machine speed: `write p1.txt`, `cat p1.txt`,
`write p2.txt`, `cat p2.txt`, `write p3.txt`, `cat p3.txt`, `update
check`: no failure. Then `update` (download of the 2.3 MiB channel image
into `UPDATE`): the host server's write to the laptop stalled after
30 s, and the laptop's `diag`, fetched over SSH afterwards, shows:

```
[102.922 s] fault reported by process 7: usb w lba 188433 (code 0)
[103.277 s] process 7 'usb_input' exited with code 15
usb_input  failed  1  1  code 15  (optional)   <- strict: not restarted
```

- The failing command is a WRITE at LBA 188433, the second chunk of the
  `UPDATE` partition (which starts at 188416): early in a run of
  sequential 8 KiB writes.
- Completion code 0: no Transfer Event within the stage timeout (5 s data
  or 3 s CSW), not a transaction error (4). The device stopped answering
  entirely (NAK or silence) rather than failing handshakes.
- The system stayed up: network, sshd, fsd and the shell alive; the
  updater exited with 23 once the file client's wait expired.
- Observed cost of the design: each SSH connection rereads
  `authorized_keys` between connections and waits for the dead file
  service (45 s), so the SSH banner is delayed and short client timeouts
  fail; `diag` was obtained with a 170 s connect timeout. Follow-up:
  sshd should skip the reread while the file service is failing (keep
  the last keys, retry at most once a minute with a short wait).

The console shows the `strict` halt dump (stage, direction, endpoint
states, PORTSC, USBSTS) for this failure; it is transcribed below when
photographed.
