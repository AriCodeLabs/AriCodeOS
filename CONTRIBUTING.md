# Contributing to AriCodeOS

Thank you for considering a contribution. AriCodeOS is a microkernel
operating system written from scratch in Rust; it grows through careful,
tested, documented changes. This page says how to make one that can be
accepted. The detailed rules live in the documents it links to.

## Before you start

- Read the [README](README.md) to build and run the system, and
  [docs/WORKFLOW.md](docs/WORKFLOW.md) for the branch flow and the rules
  every change follows.
- Look at the master document [AriCodeOS.md](AriCodeOS.md) and the ADRs in
  [docs/decisions/](docs/decisions/) before proposing a design change:
  many choices are already decided and recorded with their reasons.
- Open an issue first for anything bigger than a small fix, so the idea can
  be discussed before you spend time on it.

## Licensing of contributions

AriCodeOS is licensed under the Apache License 2.0 ([LICENSE](LICENSE)).
By submitting a contribution you agree that it is licensed under the same
terms, as section 5 of the License states, and that you have the right to
submit it. Every new source file starts with the project header:

```rust
// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
```

Add a `Signed-off-by: Your Name <you@example.com>` line to your commits
(`git commit -s`) to certify the [Developer Certificate of
Origin](https://developercertificate.org/). Accepted contributors are
listed in [AUTHORS.md](AUTHORS.md). Keep the attribution notices
([NOTICE](NOTICE)) intact; derived works credit AriCodeOS as described
there.

## How a change is made

1. Branch from `develop`: `feature/<topic>` for new work, `fix/<topic>`
   for a defect. One topic per branch.
2. Write the code **and its tests** in the same branch: host unit tests in
   the crate (`libs/*` are `no_std` with host tests) and, for anything a
   running system shows, a QEMU scenario in `tests/qemu/` (see
   [tests/README.md](tests/README.md)).
3. Keep the code factored: reusable logic goes to `libs/*`; the bare-metal
   crates only integrate. No dead code: when you replace something, delete
   the old version in the same commit.
4. If the change alters a contract (ABI, protocol, file format, security
   rule), add an ADR in `docs/decisions/` in the same branch.
5. Run, and make green, before opening the pull request:

   ```bash
   cargo xtask check          # rustfmt --check and clippy -D warnings
   cargo xtask test --all     # host tests and every QEMU scenario
   ```

   Without the toolchain installed, both run in Docker (see "Docker" in
   the README).
6. Open the pull request against `develop`. Say what the change does, why,
   and how it was tested; link the issue, ADR or incident it relates to.

## Failures are not hidden

Anything that goes wrong, in your change or found while making it, follows
[docs/FAILURE-PROTOCOL.md](docs/FAILURE-PROTOCOL.md): it is recorded in
`docs/incidents/`, studied before it is changed, fixed with a regression
test, and audited. Workarounds are bounded, counted and visible in `diag`.
A bug report with the serial log, the command line and the `diag` output
is the most useful contribution a user can make.

## Security

Services are closed by default, no key is committed, refusals are logged
and secrets come from real entropy. A change that opens a port, adds a
default credential or weakens a check is not accepted without an ADR that
justifies it. To report a vulnerability, follow [SECURITY.md](SECURITY.md)
instead of opening a public issue.

## Language and style

English is the canonical language of code, messages, documentation and
tests; `README.es.md` is the maintained Spanish translation of the README,
updated in the same branch as `README.md`. Commit messages are short,
imperative and scoped (`kernel: install own GDT and TSS`). AI coding
assistants may be used; the contributor stays responsible for the result
and reviews it as their own work ([AI-NOTICE.md](AI-NOTICE.md)).

## Code of conduct

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
