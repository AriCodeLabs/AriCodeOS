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
  (`State`): a known header starts its section, a repeated header starts
  it again (the last copy is the most recent write), lines under unknown
  headers or before the first header are dropped, and any of this marks
  the file as repaired. `with_section` always writes one header per
  section in a fixed order; bodies cannot contain header lines.
- The updater and the installer record a fault event (`upd-state-repaired`,
  `inst-state-repaired`, once per run) when they read a damaged file; the
  next write stores it canonically, so the laptop's file repairs itself on
  its next `update`.
- Host tests: every sequence of four rewrites drawn from six bodies
  (empty ones included) stays canonical with no rejected line; the
  laptop's file is recovered (`plan` = `system 3`) and flagged.
- QEMU scenario `update.state_rewrites` (no installed apps, `update`
  twice, `upgrade`, `update`): no "invalid line(s)" and no repair.
