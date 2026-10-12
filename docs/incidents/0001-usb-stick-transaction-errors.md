# Incident 0001: USB stick bulk transfers fail with transaction errors

- **Opened:** 2026-10-12. **Status:** open, under investigation.
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

## Open

- Root cause not established. The retries with pauses in acc8eaf are a
  workaround, bounded and logged, not a fix.
