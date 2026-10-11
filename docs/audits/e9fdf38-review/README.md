# Reproductions for the sandbox, Wasm, cryptography and A/B review

These tests express the expected behavior and fail on the reviewed copy.
They are not fixes and are not part of the normal suite.
`snapshot.json` identifies the reviewed files and their base commit; the A/B
boot work was uncommitted in Git when the copy was taken.

## Host

From the repository root:

```bash
cargo test --manifest-path docs/audits/e9fdf38-review/Cargo.toml --bin wasm_probe -- --nocapture
cargo run --manifest-path docs/audits/e9fdf38-review/Cargo.toml --bin fs_probe
```

`wasm_probe` runs `aricode-wasm` directly: it checks memory bounds, zeroing
on instantiation and growth, and the `start` function.

`fs_probe` keeps the `Fds`, `qualified`, `ClientBuffers` and `handle` logic
extracted from the reviewed service. It uses the original AriFS and protocol,
an in-memory disk and three simulated DMA queries. It shows that a different
prefix can use the victim's descriptor; it does not simulate IPC or the kernel.

## QEMU: isolation and broker leaks

Use a separate copy of the repository to avoid adding test programs to a
working image:

1. Copy `qemu/review_victim.rs` and `qemu/review_attacker.rs` to
   `user/testprogs/src/bin/`.
2. Create `apps/review/` and copy the three `.manifest` files there.
3. Copy the two `.toml` files to `tests/qemu/`.
4. Run `cargo xtask test --all --area review`.

No packages need to be created: xtask discovers the testprogs binaries and
the manifests. The victim exits leaving its private file open; the attacker
tries to read descriptors 0–15 from another directory. The second scenario
repeatedly requests `system, window` without starting the GUI and then tries
to run `hello_app`.

## QEMU: A/B boot

The scripts `qemu/readonly_boot.py` and `qemu/early_confirm.py` are run from
the root of the test copy, after building the `boot.kernel_ready` and
`crypto.random` scenarios. They need the Phase 30 artifacts in `build/esp`
and `build/scenarios`, and OVMF at the indicated paths.

They create their own disks in `build/review-readonly` and
`build/review-early-confirm`. They copy A's signed image to B and set up B
with three attempts. The first boots twice read-only, without running init.
The second allows writes and introduces a restricted network to observe an
init failure after confirmation. They print the relevant serial output and
the disk state; they contain no automatic pass/fail assertions.

## Private key permissions

In a fresh temporary directory, run `xtask keygen` under `umask 022` and
inspect the permissions of `ed25519.seed` with `stat`. The review got `0644`;
there is no need to show the contents or keep the seed.

## Isolation of the original run

The first QEMU suite was terminated with SIGTERM while another suite was
active in the workspace. The second used a separate copy, ports 27007–27010
instead of 17007–17010, and different names for the runner and QEMU
executables. Only the test port constants, the test HTTP client URLs, the
QEMU command and the corresponding expectations were adjusted. The files
of the findings remained identical to `snapshot.json`.
The `virtio.blk_rw` rerun uses the expectation corrected in `ade2d04`
(98304 sectors for the A/B disk).
