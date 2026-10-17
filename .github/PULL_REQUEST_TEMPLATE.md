<!-- Target branch: develop. One topic per pull request. See CONTRIBUTING.md. -->

## What and why

<!-- What the change does and the reason for it. Link the issue, ADR or incident. -->

## How it was tested

<!-- New host tests and QEMU scenarios; what you ran and the result. -->

## Checklist

- [ ] Branched from `develop` (`feature/*` or `fix/*`), one topic
- [ ] Tests added or updated in this branch (host tests, `tests/qemu/*.toml`)
- [ ] `cargo xtask check` is green
- [ ] `cargo xtask test --all` is green
- [ ] Contract change (ABI, protocol, format, security rule) has an ADR in `docs/decisions/`
- [ ] No dead code left behind; new files carry the SPDX header
- [ ] `README.es.md` updated if `README.md` changed
- [ ] Commits are signed off (`git commit -s`)
