# ADR 0060: damaged update state fails closed

- **Date:** 2026-10-27
- **Status:** Accepted (failure-pattern audit 2026-10-25, section "Info";
  incident 0045). Amends the A/B boot state of Phase 30 and the stable
  floor of ADR 0045.

## Context

Two records decide what the updater may install. The boot state (sector 0
of `ARIBOOT`, `aricode_ab::BootState`) says which slot is active and which
is pending; the install target is the other slot. The stable floor
(`[floor]` of `updater/state`, ADR 0045, audit M1) is the newest stable
release the machine ran; a stable release below it is a downgrade.

Both readers took damage for absence. A boot state that did not decode
was read as `fresh(A)`: on a machine running slot B the updater would
install over B, the running system, with A as the fallback. A damaged
floor was read as no floor, raised to the running release and saved: a
machine rolled back below a release it ran lost that floor for good.
Incident 0045 reproduced both on the host.

AriBoot reads a damaged boot state as A active with nothing pending,
boots A when it verifies and rewrites the record to say so. It has to
boot something, and with no record nothing better is known.

## Decision

1. **A damaged boot state is not fresh.** Only a new image starts from
   `BootState::fresh(A)`; AriBoot keeps it as its reading of a damaged
   record, unchanged. The updater never installs, schedules or rolls back
   from a damaged record. It rewrites the record from what AriBoot booted,
   which only AriBoot writes on the command line (`boot_slot`,
   `boot_pending`, ADR 0045), with `aricode_ab::recovered`:
   - booted from a slot, confirmed: that slot active, nothing pending;
   - booted from a slot, pending: the other slot active (a pending slot
     is never the active one) and the booted slot pending with no tries
     left, so the next boot returns to the confirmed system, as when a
     trial runs out (whether init confirmed it before the damage is not
     known);
   - booted from the ESP: AriBoot's own reading, A active.

   It reads the record back, says and records what it wrote
   (`upd-state-damaged`, code 60) and ends the run without installing:
   the owner sees the repair, which can change what boots next, before
   running the command again. Without a readable command line it writes
   nothing.

   Both sides agree: the record each side derives without other evidence
   is AriBoot's `fresh(A)`; the updater, which has the loader's own word
   on what it booted, uses it, and what it writes is a valid record that
   AriBoot then follows. The next boot is never less conservative than
   AriBoot's reading of the damaged record, and keeps the running slot
   where that reading would have switched to A.
2. **A damaged stable floor refuses every stable release.** A floor that
   cannot be read is unknown, not absent: any stable release could be
   below it (`Refusal::FloorDamaged`, `upd-floor-refused`, code 58).
   Development builds are unaffected, as before. A damaged floor is never
   replaced by the updater on its own: the owner's `--allow-downgrade`,
   the consent a floor asks for anyway, installs and rewrites it as the
   running stable release, the newest one the machine is still known to
   have run (`upd-floor-rewritten`). With a development build or an
   unknown system running it stays damaged.

   Refusing only releases below the highest version still known (the
   running one) was considered and rejected: the floor exists for the
   machine that went back below a release it ran, and that is exactly
   the release such a rule would no longer know.
3. A missing `[floor]` section stays "no floor": machines from before the
   floor start so, and the running stable release seeds it. Whoever can
   delete the section can also write a valid low floor, so absence is not
   damage the updater can detect.

## Consequences

- After a damaged boot state, the first updater run that opens the disk
  (`update`, `upgrade`, `update apply`, `update rollback`) exits with 60
  and the next one proceeds.
- After a damaged floor, `upgrade` and `update apply` refuse stable
  releases, upgrades included, until the owner passes
  `--allow-downgrade`; `update` says so in its report.
- New exit code 60 (`upd-state-damaged`); 58 now also covers
  `upd-floor-refused` and `upd-floor-rewritten`.
- Scenarios `update.state_damaged`, `update.apply_floor_damaged`; host
  tests in `aricode-ab` and `aricode-update-index`.
- Open: AriBoot itself still boots A when the record is damaged and no
  updater run repaired it first; doing better needs a redundant copy of
  the record.
