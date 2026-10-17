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
  (`inst-history-read`, code 5, naming the unrecorded line).
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
one-file directory.

`cargo xtask check`, `cargo test -p aricode-update-index` (33) and
`cargo test -p xtask` (27) pass on the branch.

No QEMU-level regression was added: there is no file-service fault
injection today (the test-only command-line keys are `test`, `shell_fault`,
`usb_fault`, `restart_storm`, `service_hang`, `selftest`), and adding one
to arifs for a single scenario would be a new mechanism beyond this fix.
The existing scenarios that exercise the changed paths
(`update.state_rewrites`, `apps.*`, `update.installed_unknown`) have not
been run on this branch yet: the fix is pending that run and the
failure-protocol audit (step 6).

## Open

- The updater's `history` (`apps/updater/src/main.rs`) still reads the log
  with `.ok().flatten().unwrap_or_default()`: its `read_file` already
  separates failure from absence, but a failed read is then stored as an
  empty history. Same class, different program; not changed here.
- No physical disk failure was induced in this audit.
