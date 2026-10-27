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
```

Before uploading, the same files can be tried locally through the mirror
of the public path (`serve-update`, docs/hardware.md, "The public source
and its local mirror").

## Test levels per branch

The tests live in the repository ([tests/README.md](../tests/README.md)) and have three levels
so that each change runs the fastest check that still gives coverage. The
full suite is for `main`; small changes do not wait for it:

```text
each commit on feature/* or fix/*     cargo xtask check && cargo xtask test --level unit      (minutes)
  same branch, touching running code  cargo xtask test --all --area <areas touched>           (minutes)
before merging into develop           cargo xtask check && cargo xtask test   (unit + smoke)  (~5 min)
before merging into main (release)    cargo xtask check && cargo xtask test --all
                                      and cargo xtask test --all --cpus 4                     (~12 min both)
                                      and cargo xtask perf && cargo xtask perf --cpus 4       (~2 min, alone)
```

The QEMU scenarios run several at a time (`--jobs N`, by default from the
CPUs and memory; ADR 0040), which brought the two full passes from about
an hour to the time above on the reference host (16 CPUs). Run the full
suite on a branch only when it changes the kernel's scheduling/SMP, memory
or the test runner itself, or when a scenario of another area could
plausibly break, and say why. Never run the same suite twice for the same
commit, and never rerun a red suite into green: explain the failure first
(failure protocol, ADR 0028).

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
