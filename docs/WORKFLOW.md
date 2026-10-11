# Git workflow (git flow)

`main` never receives direct commits. All work enters through branches and
is integrated in `develop`; `main` only moves forward with tagged versions.

## Branches

```text
main        -> always bootable; only merges from develop (release) or hotfix/*; every merge carries a tag vX.Y.Z
develop     -> integration branch; must always pass `cargo xtask test`
feature/*   -> new functionality, branched from develop, merged back into develop
fix/*       -> fix for a defect in develop, branched from develop, merged back into develop
hotfix/*    -> urgent fix on a published version, branched from main, merged into main AND develop
release/*   -> (optional) stabilisation before a tag; branched from develop, merged into main and develop
experiment/*-> prototypes that may break; never merged as they are
```

Lower-case names with hyphens and, where it applies, the roadmap phase:

```text
feature/f04-gdt-idt
feature/f05-physical-memory
fix/loader-identity-map-limit
hotfix/serial-init-order
```

## Life cycle of a feature

```bash
git checkout develop && git pull --ff-only
git checkout -b feature/f04-gdt-idt
# ... small commits with descriptive messages ...
cargo xtask check && cargo xtask test        # mandatory before integrating
git checkout develop
git merge --no-ff feature/f04-gdt-idt        # --no-ff: the branch stays visible in the history
git branch -d feature/f04-gdt-idt
```

## Publishing a version

```bash
git checkout develop && cargo xtask check && cargo xtask test
git checkout main
git merge --no-ff develop
git tag -a v0.0.2 -m "AriCodeOS 0.0.2: memory and logging"
git checkout develop
```

## Test levels per branch

The tests live in the repository ([tests/README.md](../tests/README.md)) and have three levels
so that each phase runs the fastest check that still gives coverage:

```text
on a feature/* or fix/*    cargo xtask test --level unit           (seconds, on every commit)
                           cargo xtask test --area <areas of the phase>   (only its QEMU scenarios)
before merging into develop cargo xtask check && cargo xtask test   (unit + smoke)
before merging into main   cargo xtask check && cargo xtask test --all   (unit + full)
```

Every roadmap phase adds its tests in the same branch as its code: new
`#[cfg(test)]` in the crates and new `tests/qemu/*.toml` with its area.

## Rules

1. No direct commits on `main` or `develop`: always from a branch.
2. `--no-ff` on every integration merge.
3. Before merging: `cargo xtask check` and the test level required by the
   target branch (table above) must be green.
4. One topic per branch. If an unrelated bug shows up during a feature, open
   a `fix/*`.
5. Every finished phase (Definition of Done, section 98 of the master
   document) is integrated into `develop` and, if it closes a version from
   section 64, tagged on `main`.
6. Architecture decisions: an ADR in `docs/decisions/` within the same branch.
7. Factored code: reusable logic goes into `libs/*` (no_std, with host
   tests); the bare-metal crates only integrate.
8. No dead or obsolete code: when a fix replaces something, the old code is
   deleted in the same commit. `clippy -D warnings` enforces it.
9. Test behaviour is requested on the command line (`qemu_exit`, `test=...`),
   never through compile-time features (ADR 0007).
10. Audits are kept in `docs/audits/` with their resolution; every fixed
    finding carries a regression (unit test or QEMU scenario) in the same
    `fix/*` branch.

## Languages

English is the canonical language of the project: code, comments, runtime
and console messages, ADRs, documentation and tests. The only maintained
translation is `README.es.md`, the Spanish version of `README.md`; whoever
changes `README.md` updates `README.es.md` in the same branch. The master
document (`AriCodeOS.md`) and the audit reports in `docs/audits/` were
translated to English as well (2026-10-11). Test data (sample files such as
`hola.txt` and their contents) is data, not prose, and keeps its text.

## Commit messages

First line in the imperative, short, with the scope when it helps:

```text
kernel: install own GDT and TSS
ariboot: limit the identity map to non-reserved memory
docs: ADR 0007 on the IDT format
```

Commit messages are written in English. (The older history is in Spanish.)
