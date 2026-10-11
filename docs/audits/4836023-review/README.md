# Evidence from the `4836023` review

The reproductions were run in temporary copies of the commit, without
modifying the kernel or the services. The expected failures of these tests
are independent of the official suite.

## Keyboard

Copy `keyboard-report.rs` to `libs/input/tests/review.rs` in a copy of the
commit and run:

```sh
cargo test -p aricode-input --test review -- --nocapture
```

`keyboard.log` keeps the failed assertion: two new presses instead
of six; repeating the report does not deliver the missing ones.

## Intervals

Copy `interval.rs` to `libs/xhci/tests/review.rs` and run:

```sh
cargo test -p aricode-xhci --test review -- --nocapture
```

`interval.log` keeps the two failed assertions. The expected values
come from section 6.2.3.6 of the xHCI specification linked in the report.

## Additional EHCI controller

In a copy without the previous host tests, apply
`legacy-controller.patch`, copy `audit_usb_legacy.toml` to `tests/qemu` and
run:

```sh
cargo xtask test --all --area audit_usb_legacy
```

The patch only adds `-device usb-ehci,id=review_ehci` when the scenario
enables `usb_hid`; it keeps the xHCI and its devices. `legacy-serial.log`
captures the guest output. `legacy-tests.log` keeps the 90-second
timeout and the absence of the boot-complete markers.

In the audit run, 17007–17010 were changed to 30007–30010 in
`sdk/proto/src/lib.rs`, `user/testprogs/src/bin/http_client.rs`,
`tests/qemu/shell_commands.toml` and `tests/qemu/network_stack.toml`.
An alias for the QEMU executable was also used. These are isolation adjustments;
they are not part of the defect or of the reproduction patch. Do not run
suites concurrently that share ports.

## Official suite

`check.log` contains the passing result of `cargo xtask check`. The main
report records the final result of the official tests; its isolated
copy uses ports 29007–29010 and does not contain these additional tests.

Final result: 212 host tests and 81 QEMU scenarios passing.
`tests-first.log` contains 43 scenarios before `/tmp` ran out of space;
`tests-remaining.log` contains the remaining 38 and ends with zero failures.
The host tests are repeated in the second batch; the count of 212 does not
double them. `prior-regressions.log` keeps the markers from the previous
fixes. The temporary images and builds were deleted after
saving this evidence.
