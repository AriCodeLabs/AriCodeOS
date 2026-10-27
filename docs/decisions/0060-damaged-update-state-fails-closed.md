# ADR 0060: damaged update state fails closed

- **Date:** 2026-10-27
- **Status:** Accepted (failure-pattern audit 2026-10-25, section "Info";
  incident 0045 and its audit). Amends the A/B boot state of Phase 30 and the stable
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
   from a damaged record. It rebuilds the record from what AriBoot
   booted, which only AriBoot writes on the command line (`boot_slot`,
   `boot_pending`, `boot_channel`, ADR 0045), with
   `aricode_ab::recovered`:
   - booted from a slot, confirmed (no `boot_pending`): that slot active,
     nothing pending. The active slot only changes during a boot when
     init confirms a pending one, so it is still the slot that runs; a
     trial scheduled since is dropped (the install can be made again).
   - booted from a slot, pending: **not known, nothing written.** Init
     confirms the slot once the boot services work, before the shell and
     sshd start, but a confirmation that fails is only logged and the
     boot goes on, and `boot_pending` stays on the command line all boot.
     So the running slot is usually active already, and may still be on
     trial. Either guess can be worse than AriBoot's own reading (audit
     of incident 0045, H1): "still on trial" made the running slot the
     install target, and after a second install made the next boot take
     the new, never tried image as confirmed; "confirmed" would skip a
     trial that failed. The updater says so and asks for a reboot, after
     which AriBoot boots A and rewrites the record.
   - booted from the ESP: AriBoot's own reading, A active.

   `loader_dev_channel` is set in a rebuilt record only when the running
   image is a development build (the loader verified it, so it trusts
   the key); otherwise it stays off until AriBoot writes the record on
   the next boot.

   The updater reads the record back, says and records what it did
   (`upd-state-damaged`, code 60) and ends the run without installing or
   staging anything: the owner sees the repair before running the
   command again. Without a readable command line it writes nothing.

   Both sides agree: the record each side derives without other evidence
   is AriBoot's `fresh(A)`; the updater writes another one only where
   AriBoot's own word on this boot fixes it (a confirmed boot from a
   slot), and that record is valid and followed by AriBoot. It keeps the
   running, confirmed slot active where AriBoot's reading would switch to
   A, and drops at most a trial scheduled during this boot.
2. **A damaged stable floor refuses every stable release.** A floor that
   cannot be read is unknown, not absent: any stable release could be
   below it (`Refusal::FloorDamaged`; event `upd-floor-refused`, exit
   code 58). Development builds are unaffected, as before. A damaged
   floor is never replaced by the updater on its own: a stable image the
   owner installs with `--allow-downgrade`, the consent a floor asks for
   anyway, rewrites it as the running stable release, the newest one the
   machine is still known to have run, once that image verified and went
   into its slot (event `upd-floor-rewritten`; `floor_rewrite`). A
   refused or invalid image, or a development build, changes nothing.
   With a development build or an unknown system running it stays
   damaged.

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
  (`update`, `upgrade`, `update apply`, `update rollback`, and the
  development channel's check of the loader) exits with 60; `upgrade`
  stages no app in that run, and `update` still reports, with the system
  unknown. After a confirmed boot from a slot or from the ESP the next
  run proceeds; after a pending boot it takes a reboot first.
- After a repair from a stable image, the development channel stays
  closed until the next boot (the loader's trust is not known).
- After a damaged floor, `upgrade` and `update apply` refuse stable
  releases, upgrades included, until the owner passes
  `--allow-downgrade`; `update` says so in its report.
- New exit code 60 (`upd-state-damaged`). The floor's refusal exits with
  58 (`upd-floor-refused`); `upd-floor-damaged` and `upd-floor-rewritten`
  are fault events with code 58 and do not decide the exit code.
- Scenarios `update.state_damaged`, `update.apply_floor_damaged`; host
  tests in `aricode-ab` and `aricode-update-index`.
- Open: AriBoot itself still boots A when the record is damaged and no
  updater run repaired it first (always so after a pending boot); doing
  better needs a redundant copy of the record. From an ESP boot the
  rebuilt record makes B the install target although B may hold the
  only image that verifies (AriBoot never tries B when A is active and
  rejected); `update rollback` tries B, and an install overwrites it.
