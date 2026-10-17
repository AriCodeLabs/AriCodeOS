# Incident 0006: duplicated sections in updater/state

- **Date:** 2026-10-17
- **Environment:** reference laptop, develop 45a69ef (ADR 0029 update
  sources), no installed apps.
- **Status:** fixed in `fix/update-state-sections`.

## Observation

After the migration from `update_url`, two `update` runs and one
`upgrade` (all otherwise successful), `updater/state` contained:

```text
[installed]
[plan]
[plan]
[plan]
system 3
[staged]
```

and both `update` and `upgrade` printed "1 invalid line(s) in the
installed app list ignored" and "1 invalid line(s) in the plan ignored".

## Cause

`aricode_update_index::lists::section` found the end of a section by
searching for `"\n["` inside its body. An empty body starts directly with
the next header, which is not preceded by a newline inside the body, so the
whole rest of the file was taken as the body of the empty section.
`with_section` then wrote that "body" (with the next section's header in
it) back under the empty section's header and appended the real next
section again: one more `[plan]` per rewrite. On the laptop `[installed]`
is empty (no apps) and comes first, so every rewrite after the first
duplicated `[plan]`. The QEMU scenarios always had an installed app or an
empty last section, which is why they did not show it.

## Fix

- `lists` parses the state file line by line into its three sections
  (`State`); `with_section` always writes one header per section in a
  fixed order, and bodies cannot contain header lines.
- Damaged files are recovered without guessing. The order of copies in a
  damaged file does not tell which one was written last (an independent
  audit showed an out-of-order example where the later `[plan]` is the
  stale one), so a repeated `[plan]` or `[staged]` is emptied: both are
  recomputable by running `update` and `upgrade` again, and a stale copy
  could install something the owner was never shown. A repeated
  `[installed]` keeps its last copy (advisory: the installer rewrites it
  before every updater run) and is flagged like any repair. Lines under an
  unknown header or before the first header are dropped. Only this
  structural damage counts; CRLF line endings, blank lines and a missing
  final newline do not.
- The updater and the installer store the repaired file at once when they
  read a damaged one (atomic replace) and record `upd-state-repaired`
  (code 47) or `inst-state-repaired` (code 13), with a message saying
  whether the repair was saved. On the laptop the first run after the fix
  repairs the file (the plan is dropped) and the next `update` writes a
  fresh plan.
- Host tests: every sequence of four rewrites drawn from six bodies
  (empty ones included) stays canonical with no rejected line; the
  laptop's file, the out-of-order example, a repeated `[staged]`, a
  repeated `[installed]`, unknown headers and stray lines are recovered
  and flagged; CRLF, blank lines and a missing final newline are not
  flagged.
- QEMU scenario `update.state_rewrites` (no installed apps, `update`
  twice, `upgrade`, `update`): no "invalid line(s)" and no repair.
