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

Before the release branch is merged, raise `aricode_x509::TIME_FLOOR`
(`libs/x509/src/lib.rs`) to the release day: no machine checks
certificates at an earlier moment, so a certificate that expired before
the release is refused even when the real-time clock is wrong (incident
0036). Check the week's certificate advisories too (webpki-roots,
Mozilla's CA list): a key distrusted there goes into
`aricode_x509::roots::DISTRUSTED` (`libs/x509/src/roots.rs`) with a comment
naming the certificate and the advisory (ADR 0054). Refresh the public
suffixes wildcards may not stand on (`libs/x509/psl/update.sh`, ADR 0062)
and commit the extract when the list changed.

A release also feeds the public update source (ADR 0048): from the tagged
commit, publish the release's system version with the release key and
attach the three files to the GitHub release, beside the USB image (the
owner approves each upload):

```bash
cargo xtask publish-update --version N --no-legacy-manifest \
    --signing-key ~/.config/aricodeos/signing-key
gh release upload vX.Y.Z build/update/index build/update/index.sig build/update/aricodeos-N.img
cargo xtask usb-image --public-source --signing-key ~/.config/aricodeos/signing-key
cargo xtask notices
gh release upload vX.Y.Z build/THIRD-PARTY-NOTICES.txt
```

The USB image carries `THIRD-PARTY-NOTICES.txt` on its EFI partition, and
the release attaches it too: the licences of the crates the images
include ask for their notices with binary distributions (NOTICE).

Before uploading, the same files can be tried locally through the mirror
of the public path (`serve-update`, docs/hardware.md, "The public source
and its local mirror").

## Lines of work

The owner's rule (2026-11-06): several lines advance at once, and a change does
not wait for what it cannot affect. Each line is a long-lived branch off `develop`,
with its own worktree:

```text
stream/desktop   -> the desktop, the toolkit and its apps (ADR 0078)
stream/platform  -> kernel, drivers and the path to the hardware (ADR 0080)
stream/demos     -> what shows the system working: games, simulation
```

`feature/*` and `fix/*` branch from their line and merge back into it. A line
merges `develop` before each feature, and merges into `develop` when a step works,
on the reference laptop if it can be shown there. A change to what several lines
share (`sdk/`, the protocols) goes to `develop` first, so the others take it soon.

## Test levels per branch

The tests live in the repository ([tests/README.md](../tests/README.md)). A test is
there to find what a change can break, so each moment runs only what it can find:

```text
each commit on feature/* or fix/*     cargo xtask check && cargo xtask test --level unit      (minutes)
  same branch, touching running code  cargo xtask test --all --area <areas touched>           (a minute or two)
merging a line into develop           cargo xtask check && cargo xtask test   (unit + smoke)  (~2 min)
composing a version (before main)     cargo xtask check && cargo xtask test --level release
                                      and cargo xtask test --level release --cpus 4           (~12 min both)
                                      and cargo xtask perf && cargo xtask perf --cpus 4       (~2 min, alone)
```

- **Slow scenarios wait for the version.** A scenario of 15 s or more has `level =
  "release"`: `--all` and `--area` skip it. Working on what one covers, run it by
  name (`--only`). A new scenario over 15 s starts at that level.
- **A test that a harder one already covers goes.** A scenario earns its place by
  what it alone can fail on: when one extends another, keep the larger.
- **The full run is not for branches,** whatever they touch: not for kernel
  changes either. The CI container runs it on `develop` after each merge, and a
  failure there is an incident, fixed forward.

Never rerun a red suite into green: explain the failure first (failure protocol,
ADR 0028).

The full suite on `develop` runs continuously in the CI container
(`tools/docker/ci/`, README.md "Docker"); a failure there becomes an
incident. One suite runs at a time on a machine, the CI container's
included (stop it first), and test containers get `--cpus 10 --memory 12g
--memory-swap 12g` on the reference host so it keeps room and stays cool
(the runner's default `--jobs` follows those limits). Ports alone would
allow more: only three test ports are fixed (compiled into the guest's test
programs), one set per network namespace, and a Docker container is its
own namespace.

The full suite can run on any machine with Docker, without installing the
toolchain: see "Docker" in [README.md](../README.md). The result counts the
same as a host run of the same command.

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

## Failures

Anything that goes wrong follows `docs/FAILURE-PROTOCOL.md` (ADR 0028):
record it in `docs/incidents/`, instrument with `strict`, study it from
several angles before changing code, fix with a test, audit, close.
Workarounds are bounded, counted and visible in `diag`.
