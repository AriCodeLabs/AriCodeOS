# Evidence for the `3eb4965` review

These tests are audit instrumentation, not fixes. They were run on temporary
copies; they do not modify production code.

## Process reaping

- `reap-interleaving.patch`: barrier in `release_address_space` and
  observation of the removal in `wait_until`. Replaces yielding with busy
  waiting in that window so the parent's CPU does not reap the process.
- `audit_reap.toml`: adaptation of `user.hello` with four CPUs.
- `reap-serial.log`: observed result, `mapper=true` when the process is
  removed and `process_present=false` when reaping continues.

On a copy of `3eb4965`, apply the patch, copy the TOML to `tests/qemu`
and run `cargo xtask test --all --area audit_reap`. The scenario keeps
the functional expectations of `user.hello` and passes; the `AUDIT`
markers document the faulty interleaving.

## IOMMU reassignment

- `iommu-reassignment.patch`: adds a lifecycle check at the start of
  `iommu::selftest`, using two test domain identifiers and PCI function
  `00:02.0` from the project's QEMU configuration.
- `audit_iommu.toml`: standalone selection of that scenario.
- `iommu-serial.log`: context present in the second domain before
  freeing the first, absent afterwards; failed assertion.

On another clean copy of `3eb4965`, apply only this patch, copy the TOML
to `tests/qemu` and run `cargo xtask test --all --area audit_iommu`.
An assertion failure is expected on the reviewed commit.

Both reproductions used ports 28007–28010 instead of 17007–17010 and an
alias for the QEMU executable. The isolated official suite used
27007–27010. These are adjustments to avoid interference with other runs;
they are not part of the kernel patches. Do not run two suites on the
same ports simultaneously.

## Official suite

`check.log`: formatting and Clippy pass. `tests.log`: 203 host tests
and 77 QEMU scenarios pass, on the isolated copy without kernel
instrumentation. The failures of the additional reproductions do not
belong to this official suite.
