# Incident 0045: the updater failed open on damaged state

- Opened: 2026-10-27. Status: fixed on `fix/updater-state` (ADR 0060);
  the independent audit of the fix is pending.
- Found by: the failure-pattern audit of 2026-10-25, section "Info": the
  updater treats a boot state with a bad CRC as fresh, the stable floor
  fails open when its section is damaged, and `serve-update` echoes the
  raw request target into its log and `Location`. Not seen on a machine.
- Environment: `updater` (`update`, `upgrade`, `update apply`, `update
  rollback`) on a machine with the A/B layout; `cargo xtask serve-update`
  on the build host.

## Evidence

Host tests run on `develop` at `5ef2f6e`, before the fix (kept out of the
tree; the regressions below replace them):

1. **Boot state.** A sector holding `BootState::fresh(Slot::B)` with one
   bit of its CRC flipped, read as `apps/updater` read it
   (`BootState::decode(..).unwrap_or(BootState::fresh(Slot::A))`): the
   install target `state.active.other()` is `B`, the slot that runs.
   `assertion left != right failed: install target is the running slot
   B; left: B, right: B`.
2. **Stable floor.** `[floor]` holding `stable 3x`, the machine running
   stable 1, composed as the updater's `stable_floor` did
   (`parse_floor(..).unwrap_or(None)`, then `raise_floor`): the floor
   becomes `Some(1)` and is saved, and `decide_system` for stable 2
   returns `Install`. `assertion left != right failed: stable 2 below the
   lost floor 3; left: Install, right: Install`.
3. **serve-update.** One connection to `serve_update_connection` sending
   `GET /AriCodeLabs/AriCodeOS/releases/latest/download/index\r\nSet-Cookie:%20owned=1 HTTP/1.1`:
   the response carried `Location: /AriCodeLabs/AriCodeOS/releases/download/local/index\r\nSet-Cookie:%20owned=1\r\n`,
   a header line of the client's choice, and the log printed
   `[xtask] 127.0.0.1:51184 GET /AriCodeLabs/AriCodeOS/releases/latest/download/index`
   and `Set-Cookie:%20owned=1: 9434 bytes sent` as two lines.

## Mechanism

1. `Disk::open_with` decoded sector 0 of `ARIBOOT` and, when it did not
   decode, used `BootState::fresh(Slot::A)`: A active, nothing pending.
   The record is damaged when a write is torn or the medium fails; AriBoot
   rewrites it on every boot from a slot, so the updater meets damage
   written after the boot. On a machine running slot B, `fresh(A)` makes
   B the "inactive" slot: `update apply` and `upgrade` would clear B's
   header, write the new image over the running system and schedule it
   with A, older, as the fallback; the downgrade rule compared the offer
   with A's image instead of what runs.
2. `stable_floor` reported a damaged `[floor]` (`upd-floor-damaged`),
   then treated it as no floor and raised it to the running stable
   release, which it saved. A machine that ran stable 3 and was rolled
   back to 1 lost its floor for good: stable 2, a downgrade below what it
   ran, went in as an upgrade, on that run and every later one.
3. `serve_update_connection` (and the scenario server `http_serve`) took
   the target as whatever followed the first space of the request, up to
   the next space, CR and LF included. `mirror_route` put the rest of the
   path in a `Location`, and the log line printed it raw.

## Fix

ADR 0060 records the contract.

1. **A damaged boot state is never read as fresh.** `Disk::open_with`
   refuses to work from it: `repair_boot_state` rewrites it from what
   AriBoot booted, read from the keys only AriBoot writes on the command
   line (`boot_slot`, `boot_pending`, ADR 0045), with
   `aricode_ab::recovered`: a slot booted confirmed is active with
   nothing pending; a slot booted pending is kept pending with no tries
   left and the other slot active (the next boot returns to the confirmed
   system, as when a trial runs out); from the ESP it writes AriBoot's own
   reading, A active. The record is read back. The run then installs
   nothing and says so (`upd-state-damaged`, code 60): `the boot state in
   ARIBOOT was damaged: rewritten from what AriBoot booted (slot B) as
   active B, none pending; nothing installed: run the command again`.
   Without a readable command line nothing is written. AriBoot keeps its
   reading of a damaged record (A, the slot a new image starts from,
   rewritten once A verifies); `recovered` never makes the next boot less
   conservative than that reading, and keeps the running slot where that
   reading would have switched to A.
2. **A damaged stable floor fails closed.** `floor_for_run` keeps it
   damaged instead of replacing it, and `decide_system` refuses every
   stable release while it is (`Refusal::FloorDamaged`; `upd-floor-refused`,
   code 58): the newest stable release this machine ran is unknown, so
   any stable release could be below it. Development builds are not
   affected (the floor says nothing about them). The way out is the
   owner's `--allow-downgrade`, the consent the floor asks for anyway: it
   installs and rewrites the floor as the running stable release, said
   and recorded (`upd-floor-rewritten`). Nothing else rewrites a damaged
   floor, so damage never lowers it silently.
3. **serve-update keeps request bytes out of headers and logs.**
   `request_target` accepts only `METHOD SP target SP HTTP/x.y CRLF` with
   a target of visible ASCII (RFC 9112, 3.2); anything else is answered
   `400 Bad Request` and logged escaped (`log_escaped`: control bytes as
   `\xNN`). `mirror_route` redirects only to plain file names (letters,
   digits, `-._`, the names `file_response` serves). Both servers share
   the parsing.

## Regressions

- `aricode_ab`: `a_damaged_state_is_rebuilt_from_the_booted_slot` (fails
  with `recovered` returning `fresh(A)`, as before).
- `aricode_update_index`: `a_damaged_floor_refuses_every_stable_release`
  (fails with the old mapping of a damaged floor: `left: (At(1), Some(1))`,
  `right: (Damaged, None)`).
- xtask: `serve_update_keeps_request_bytes_out_of_headers` (the request
  of the evidence, a bare CR, an escaped CR LF, and a sound request whose
  redirect is unchanged), `request_targets_and_log_text_are_checked`.
- QEMU: `update.state_damaged` (disk `a-corrupt-state-damaged`: slot A
  does not verify and the state's CRC is damaged, so AriBoot boots the
  ESP and leaves the record; the first `update apply` rewrites it and
  installs nothing, the second installs into B, which boots next) and
  `update.apply_floor_damaged` (`stable_floor_damaged`: `update apply`
  refuses stable 2 with the floor damaged and leaves `stable 3x`;
  `--allow-downgrade` rewrites it as `stable 1` and installs). The slot
  cases of the boot state cannot be staged at boot, since AriBoot
  rewrites a damaged record whenever a slot boots; they are the host
  test's.

## Open

- A missing `[floor]` section is still "no floor" (`Unset`): that is how
  machines from before the floor start, and a section that is absent
  cannot be told from one that was never written. Whoever can delete it
  can also write a valid low floor, so this is not damage the updater can
  detect.
- AriBoot's reading of a damaged record (boot A) can switch a machine
  running B to A if the record is damaged and the machine reboots before
  any updater run; a redundant copy of the record would be needed to do
  better.
