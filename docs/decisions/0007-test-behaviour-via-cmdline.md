# ADR 0007: Test behaviour via the command line, not features

- **Date:** 2026-09-26
- **Status:** Accepted

## Context

In M0 the kernel terminated QEMU through a Cargo feature (`qemu-exit`),
which forced building different binaries for testing and for normal use and
scattered `#[cfg(feature)]` across the code.

## Decision

AriBoot reads `\aricodeos\cmdline.txt` from the ESP and hands it to the kernel in
`BootInfo` (protocol v2). Keys recognised in the `aricode-cmdline` crate:
`qemu_exit` (terminate QEMU on completion or failure) and `test=<name>` (provoked
faults). A single binary of each component serves every scenario in
`tests/qemu/*.toml`.

## Consequences

- No test features or scattered `cfg`; the runner only changes one file.
- Scenarios are declared as data and validated with a unit test.
- The cmdline becomes available as a general boot configuration mechanism
  for later phases (log level, safe mode...).
- Test behaviour is off on a real machine unless its key is present. Since
  ADR 0032 that includes init's test clients (`selftest`), which xtask adds
  to every scenario by default.
