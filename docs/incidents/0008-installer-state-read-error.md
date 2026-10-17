# Incident 0008: installer loses state after a directory-list error

- Opened: 2026-10-18. Status: fixed in `fix/installer-state-read-error`,
  pending QEMU run and audit.
- Reviewed commit: `525b550`; concerns the new shared updater state.
- Environment: extracted installer helpers with one failed list request.
- Evidence: [release audit, finding 2](../audits/2026-10-18-release-0.11.0-525b550.md#2-p2-a-transient-directory-list-error-can-erase-updater-state),
  [probe output](../audits/525b550-review/probes.log).

## Observation

An existing `updater/state` contained installed, plan and staged sections.
Its first directory lookup failed; the following open/replace succeeded.
Writing the installed section returned success and removed both other
sections without raising `inst-state-unreadable`.

## Mechanism (confirmed by the audit probe)

`apps/installer/src/main.rs` found a file's size by walking the directory
with `while let Ok(Some(..)) = fs.list(i, ..)` (`file_size`). The loop
ended on the first `Err` exactly as it ended past the last entry, so the
helper answered `None` both for an absent file and for a listing the file
service never answered (`ReplyLost`, `Timeout`, or any error status).
`read_state` took `None` for "no state file yet" and returned an empty
text; `write_state_section` then stored `with_section("", ...)` with one
atomic `replace`: the plan and the staged list were gone, the write
reported success, and the `inst-state-unreadable` fault (final re-audit,
L-a) was never raised because it only guarded the read *after* a
successful lookup.

The same helper served every other read in the installer, with the same
conflation:

- `installed` (installed-version detection, used by `install_one`): a
  failed lookup read as `Installed::Missing`, so a package was installed
  over a copy whose version nobody knew (a downgrade with the owner's
  check bypassed; ADR 0029, decision 5);
- `installed_apps` (`LIST`) and `install_pending` walked the directory
  with the same `while let Ok(Some(..))`: a listing that failed part-way
  produced a shorter list, written to `[installed]` as "not installed" or
  reported as "nothing to install";
- `history` read the log with `unwrap_or_default()`: a failed read
  replaced the whole history by the one new line;
- the staged-package and pending-package reads reported a failed request
  as "missing" or "cannot be read": a safe outcome (nothing installed),
  but the cause was hidden.

## Fix

Branch `fix/installer-state-read-error`.

- New module `libs/update-index/src/files.rs`: a `Files` trait (open,
  read, close, list) whose `open` returns `Ok(None)` only when the service
  explicitly answered *not found*, and two decisions on top of it:
  `read_optional` / `read_text` (`Ok(None)` absent, `Ok(Some)` read,
  `Err(ReadError::Failed(e))` request failed, `Err(ReadError::TooLarge)`
  over the limit) and `list_names` (`Err` on a listing that fails
  part-way, never a shorter list). The installer implements `Files` over
  its `FsClient` (`Main`), mapping only `FsError::Status(Status::NotFound)`
  to absence; the directory listing is no longer used to find a file.
- `read_state`: empty text only on `Ok(None)`; any `Err` raises
  `inst-state-unreadable` (code 12, now with the error in the message) and
  `write_state_section` refuses the read-modify-write, so the other
  sections are never replaced by what a failed read did not return.
- `installed` gains `Installed::Unknown(FsError)`; `install_one` refuses
  the package with the new fault `inst-installed-unknown` (code 14) instead
  of treating the copy as missing; `installed_apps` raises the same fault
  and does not write `[installed]` at all.
- `installed_apps` and `install_pending` list through `list_names`; a
  failed listing is the new fault `inst-dir-list` (code 15) and the run
  stops without writing anything.
- `history`: a log that cannot be read is not rewritten
  (`inst-history-read`, code 5, naming the unrecorded line). The append
  itself is `aricode_update_index::history::appended` (read through
  `Files`, then `append`), shared by the three writers of the history.
- Second commit, same class in the other two writers of
  `updater/history.log`: the updater's `history()` read the log with
  `.ok().flatten().unwrap_or_default()` (its `read_file` already separated
  failure from absence, but the caller threw the distinction away), and the
  shell's `update add/remove` did the same with `read_small`. Both now go
  through `history::appended`: a failed read is `upd-history-read` (updater,
  code 48) or `sh-history-read` (shell, code 2) and the file is not
  rewritten. The updater's `read_file`/`read_text` are now thin wrappers
  over `files::read_optional` (a `Private` newtype implements `Files`), so
  `upd-state-unreadable` and `upd-indexes-unreadable` name the error; the
  shell's `read_small` wraps the same function.
- A pending or staged package whose read fails is `inst-pkg-read` (code
  16), distinct from missing, oversized or tampered (the
  `apps.staged_tampered` message is unchanged).
- ADR 0029, decision 7 and Consequences, document the new codes.

## Tests

Host (`cargo test -p aricode-update-index`, `files::tests`, all with a
mock `Files` whose requests fail on demand): absent file is `Ok(None)` and
nothing else is; a present file is read whole across chunks; a failed open
is `Err(Failed)`, not absence; a failed read is `Err(Failed)` and closes
the descriptor; an oversized file is `Err(TooLarge)` (exactly the limit
passes); the state read-modify-write keeps the plan and staged sections
when the read succeeds, starts empty when the file is absent and gives
nothing to write from when the read fails; a listing filters in directory
order; a listing that fails after the first entry is an error, not a
one-file directory. `history::tests`: `appended` starts with the new line
when the log is absent, appends when it is present, refuses a failed read,
trims an overgrown log and refuses one far over the limit.

`cargo xtask check`, `cargo test -p aricode-update-index` (40: 25
existing, 8 `files`, 2 `history`, 1 `installed_path`, 4 `run`) and
`cargo test -p xtask` (27) pass on the branch.

No QEMU-level regression was added: there is no file-service fault
injection today (the test-only command-line keys are `test`, `shell_fault`,
`usb_fault`, `restart_storm`, `service_hang`, `selftest`), and adding one
to arifs for a single scenario would be a new mechanism beyond this fix.
The existing scenarios that exercise the changed paths
(`update.state_rewrites`, `apps.*`, `update.installed_unknown`) have not
been run on this branch yet: the fix is pending that run and the
failure-protocol audit (step 6).

## Audit of the branch (2026-10-18, third commit)

An independent audit of the two commits found the mechanism closed and
asked for these corrections, made in the third commit:

- M1. In a `STAGED` run, `install_one` answered `Ok(false)` ("skipped") on
  `Installed::Unknown`; the run then emptied `[staged]` and exited 0, so a
  transient read failure consumed the staged download with a success
  exit. It now returns `Err(code::INSTALLED_UNKNOWN)`: the run exits
  nonzero, and `install_staged` keeps `[staged]` after any *failure*
  (`inst-pkg-read`, `inst-installed-unknown`, `inst-open`, `inst-write`),
  saying so; a *refusal* (tampered, unverified, downgrade, same version)
  still consumes it.
- M2. The updater's `installed_apps(fs).unwrap_or_default()` in `report`
  and `upgrade` read an unreadable `[installed]` as "no apps installed":
  `report` printed every offered app as not installed and saved an
  app-less plan. `installed_apps` now takes the state text; `report` reads
  the state once, and when that read failed it still reports the system,
  says the apps are not compared, lists nothing as "not installed" and
  saves no plan (the fault code is the exit code); `upgrade` reuses the
  state it already read.
- L1. `inst-state-unreadable` could fire three times in one `STAGED` run
  (the read, the `[staged]` clear, the `[installed]` write each re-read
  the file). Both programs now read the state once per run and pass the
  text to `write_state_section`.
- L2. `FsClient::open` refuses a name over 32 bytes client-side
  (`Status::Invalid`), which the new code would have reported as
  `inst-installed-unknown`, blaming the file service, for a package whose
  name makes `apps/NAME.aripkg` too long. `aricode_update_index::
  installed_path(name)` builds the path only for a valid app name (ADR
  0029, decision 2: at most 20 bytes, `[A-Za-z0-9_-]`; the longest gives
  exactly 32 bytes) and the installer refuses anything else as a package
  (`inst-pkg-name`, code 7) before touching the installed copy. Host test
  `installed_path_fits_arifs_or_is_refused`.
- L3 (accepted, no code change): the installer's wiring of these decisions
  to the real file client is checked by reading only; the decisions
  themselves are host-tested over a mock. A QEMU regression needs a
  file-service fault-injection mechanism (arifs, test-only command-line
  key), which does not exist yet. Known gap.
- L4 (pre-existing, not fixed here): arifs never reclaims the descriptors
  of a client that died; see incident 0011
  (`0011-arifs-descriptors-not-reclaimed.md`). With this fix a full table
  is refused visibly instead of being read as "absent".

## Second audit (2026-10-18, fourth commit)

- A1 (test gap). The end of a `STAGED` run is now
  `aricode_update_index::run::settle_staged(prior, outcomes)`, a pure
  function over one `Outcome` per entry (`Installed`, `Refused(code)`,
  `Failed(code)`): a failure anywhere keeps `[staged]` and the exit code is
  the first failure's; otherwise the list is consumed and the code is the
  refusal recorded before the entries (malformed lines), else the first
  refusal's nonzero code, else 0. The installer only produces the outcomes
  and applies the decision. Host tests (`run::tests`, over the `files`
  mock for the state text): all installed consumes with code 0 and keeps
  the plan; refusals consume and name the first fault; a failure keeps the
  list unchanged and exits nonzero; a failure wins over refusals in any
  order, the first of two failures names the code, an empty list is a
  no-op. The updater's `report` decision (unreadable state: no plan saved,
  nonzero) was not factored: it is one `Result` match around a write, and
  the messages are the only logic.
- A2. `write_state_section` updates the in-memory `state` only on an
  acknowledged write. If the `[staged]` clear's reply is lost after the
  replace landed, `[installed]` written from the old text would bring the
  list back (harmless: the next run refuses the same versions as already
  installed, but confusing). The installer now re-reads the state after a
  failed clear and writes nothing more if that read fails too; the
  invariant is stated at the call.
- A3. `report` with an unreadable state (or a failed plan write) no longer
  ends with "everything is up to date" or "N upgrade(s) available: run
  upgrade": it says "N upgrade(s) found but the plan was not saved
  (updater/state could not be read | write failed): run update again". Its
  exit code is the first failure of the run (`first()`), so a disk error
  is not overwritten by a later state fault; every failure is still its
  own fault event.
- A4. The kept-list message now says what is true: "staged list kept for
  inspection in updater/state (a failure, not a refusal); upgrade stages
  again".

## Open

- A file-service fault-injection mechanism for QEMU regressions (L3).
- Incident 0011 (arifs descriptors).
- No physical disk failure was induced in this audit.
