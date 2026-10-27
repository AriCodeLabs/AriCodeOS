# Incident 0045: the updater failed open on damaged state

- Opened: 2026-10-27. Status: fixed on `fix/updater-state` (ADR 0060),
  with the follow-ups of its audit (section "Audit (2026-10-27)").
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
   `aricode_ab::recovered`: a slot booted, pending or not, is active
   with nothing pending (the updater only runs after init checked and
   confirmed it; audit H1 and re-audit M1 below); from the ESP it writes
   AriBoot's own reading, A active. The record is read back. The run then installs
   nothing and says so (`upd-state-damaged`, code 60): `the boot state in
   ARIBOOT was damaged: rewritten from what AriBoot booted (slot B) as
   active B, none pending; nothing installed: run the command again`.
   Without a readable command line nothing is written. AriBoot keeps its
   reading of a damaged record (A, the slot a new image starts from,
   rewritten once A verifies).
2. **A damaged stable floor fails closed.** `floor_for_run` keeps it
   damaged instead of replacing it, and `decide_system` refuses every
   stable release while it is (`Refusal::FloorDamaged`; `upd-floor-refused`,
   code 58): the newest stable release this machine ran is unknown, so
   any stable release could be below it. Development builds are not
   affected (the floor says nothing about them). The way out is the
   owner's `--allow-downgrade`, the consent the floor asks for anyway: it
   installs, and once the image went into its slot the floor is rewritten
   as the running stable release, said and recorded
   (`upd-floor-rewritten`; audit L4 below). Nothing else rewrites a damaged
   floor, so damage never lowers it silently.
3. **serve-update keeps request bytes out of headers and logs.**
   `request_line` accepts only `METHOD SP target SP HTTP/x.y CRLF` with
   a target of visible ASCII (RFC 9112, 3.2); anything else is answered
   `400 Bad Request` and logged escaped (`log_escaped`: control bytes as
   `\xNN`). `mirror_route` redirects only to plain file names (letters,
   digits, `-._`, the names `file_response` serves). Both servers share
   the parsing.

## Regressions

- `aricode_ab`: `a_damaged_state_is_rebuilt_from_the_booted_slot` (fails
  with `recovered` returning `fresh(A)`, as before),
  `a_pending_boot_is_rebuilt_as_the_slot_init_checked` (audit H1,
  re-audit M1).
- `aricode_update_index`: `a_damaged_floor_refuses_every_stable_release`
  (fails with the old mapping of a damaged floor: `left: (At(1), Some(1))`,
  `right: (Damaged, None)`), `reading_a_damaged_floor_never_saves_it`
  (audit L4).
- xtask: `serve_update_keeps_request_bytes_out_of_headers` (the request
  of the evidence, a bare CR, an escaped CR LF, and a sound request whose
  redirect is unchanged), `request_targets_and_log_text_are_checked`.
- QEMU: `update.state_damaged` (disk `a-corrupt-state-damaged`: slot A
  does not verify and the state's CRC is damaged, so AriBoot boots the
  ESP and leaves the record; the first `update apply` rewrites it and
  installs nothing, the second installs into B, which boots next) and
  `update.apply_floor_damaged` (`stable_floor_damaged`: `update apply`
  refuses stable 2 with the floor damaged and leaves `stable 3x`;
  `--allow-downgrade` installs, then rewrites it as `stable 1`). The slot
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
- From an ESP boot, B becomes the install target although it may hold the
  only image that verifies (audit L3).

## Audit (2026-10-27)

An independent audit of the fix (`126429e`) found one high finding and
several low ones. All are followed up on this branch (after merging
`develop`, `8f2cef9`); the fix above describes the first version, and
where it differs the follow-up below holds (ADR 0060 is corrected).

- **H1: a pending boot was rebuilt wrongly.** `recovered` took
  `boot_slot=X boot_pending` as "the other slot active, X pending with no
  tries". But init confirms the pending slot at step 11, before the shell
  and sshd start, and `boot_pending` stays on the command line all boot:
  when any updater runs, X is almost always active already. (a) Boot A
  pending, init confirms A, the record is damaged: the repair wrote
  "active B, pending A, 0 tries", and the next run took A, the running
  confirmed slot, for the install target. (b) The same boot, then B
  installed and scheduled, and a second install's write tore the record:
  the repair made the next boot exhaust A's (zero) tries and boot B, new
  and never tried, as confirmed, where AriBoot's own reading boots A. The
  claim that the repair is never less conservative than AriBoot was
  false. Init does not make the confirmation a precondition: a failed
  confirmation (`confirm_boot_slot`: no block service, no `ARIBOOT`, a
  read or write error, an invalid state) is only logged and the shell and
  sshd start anyway, so the updater cannot tell a confirmed slot from one
  still on trial. First follow-up (`a070565`): from a boot pending
  confirmation `recovered` returned `None` and nothing was written, the
  run asking for a reboot. The re-audit (M1 below) showed that rationale
  wrong and the rule now rebuilds the slot as active. On `126429e` both
  scenarios, written against the old signature, failed with `install
  target is the running slot A` and `B booted as confirmed: Decision {
  slot: B, pending: false, ... }`.
- **L1: `upgrade` went on after a repair.** `Disk::probe` dropped the
  repair's code, and with a system line in the plan `Disk::open`'s 60 was
  kept but the apps were still staged (exit `60|0x100`). Now `probe`
  returns a damaged state as an error, and `upgrade` returns at once from
  either path, before staging anything. Not reachable from a scenario
  (`upgrade` needs a plan from an earlier run, which already repairs);
  checked by reading and by the shared `state_damaged` predicate.
- **L2: the development channel never reached the repair.** A damaged
  record made `loader_trusts_dev` false, so `update`/`upgrade --channel
  dev` stopped at "the loader does not trust the development channel".
  `loader_trusts_dev` now says "not known" for a damaged record, and the
  development check says so and repairs it (exit 60). The rebuilt record
  sets `loader_dev_channel` when the running image is a development build
  (`boot_channel=dev`: the loader verified it with that key); otherwise it
  stays off until the next boot, so after a repair from a stable image
  the channel opens again only after a reboot (stated in ADR 0060).
- **L3: from an ESP boot, B becomes the target** although B may hold the
  only image that verifies (pre-existing: AriBoot never tries B when A is
  active and rejected). Left open and stated in ADR 0060: `update
  rollback` tries B, an install overwrites it.
- **L4: the floor was rewritten before the image was checked.** `update
  apply --allow-downgrade` saved the rewritten floor while reading it,
  before UPDATE was loaded, so an invalid or development image lowered it
  too. `floor_for_run` no longer rewrites anything; `floor_rewrite` gives
  the value, and the updater saves it only after a stable image forced
  with `--allow-downgrade` went into its slot (`update apply` and
  `upgrade`). Test: `reading_a_damaged_floor_never_saves_it` (on
  `126429e`, `floor_for_run(damaged, stable 1, true)` returned
  `(At(1), Some(1))`, which the updater saved), and the `floor_rewrite`
  cases in `a_damaged_floor_refuses_every_stable_release`; the scenario
  `update.apply_floor_damaged` now expects the rewrite after `installed
  into slot B`.
- **I3:** `upd-floor-rewritten` (and `upd-floor-damaged`) are fault
  events with code 58, not exit codes; ADR 0060 says so.
- **I4:** `serve-update` logged `GET` for any method; `request_line`
  returns the method too and the log shows it.

### Re-audit (2026-10-27)

An independent re-audit of `a070565` found H1's hazards closed and L1,
L2, L4, I3 and I4 correct, and one medium and two low findings:

- **M1: the reason for leaving a pending boot to AriBoot was wrong.**
  The audit H1 follow-up assumed that a failed confirmation lets the
  shell start on an unchecked slot. In the code, init confirms at step 11
  after every health check; a trial that fails a check never gets there
  (a service fails and init returns, or `init_fail_late` returns 99) and
  then no shell starts; the updater is launched only from a shell
  (`services/shell`, `launch_run("updater", ..)`; the compositor launches
  no updater), and sshd's session shells start after step 11 too. So
  whenever the updater runs, the pending slot passed init's checks, and
  an unconfirmed one only means that the confirmation's own I/O failed.
  Leaving the record to AriBoot sent a machine running B back to A, an
  older system or the never-tried image a second install put there, with
  no fallback. Follow-up: `recovered` rebuilds a slot booted pending as
  active, exactly what init meant to write, and returns a plain
  `BootState` again; the premise is stated in its doc comment, in ADR
  0060 and here: anything that could run the updater before init's step
  11 would have to revisit the rule. Test:
  `a_pending_boot_is_rebuilt_as_the_slot_init_checked`, for A and B: after
  the repair the install target is never the running slot, and the next
  boot is the slot that passed init, with an install torn in the other
  slot and with a confirmation whose write failed.
- **L-a: after a repair from a stable image the development channel blamed
  the loader** ("does not trust the development channel", pointing at a
  new stick) until the next boot. AriBoot counts every boot it chooses
  and nothing else raises the count, so a record holding 0 boots was made
  or rebuilt and not written by it since (`BootState::written_by_loader`);
  its development flag says nothing. `loader_trust` now tells that case
  apart and the refusal says the trust is not known and that a boot from
  a slot restores it. The test errs only towards keeping the channel
  closed: AriBoot also writes without counting when it rejects a slot or
  cannot persist a decision, and the count wraps at `u32::MAX`; a machine
  that boots from the ESP never has the record written (third pass of the
  re-audit, L1 and L2).
- **L-b: the race in `dev_not_trusted`** (a record that became valid
  between the check and the repair) fell through to the same message; it
  now gives the same "not known, reboot" message.
