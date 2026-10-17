# Review of supervision, persistence, packages and WASM

Date: 2026-09-29. Reviewed commit: `6ee19fb`.
Range: `4836023..6ee19fb`; clean tree at the start.

Scope: USB/HID fixes, version 0.6.0 and stages 1–4: supervisor,
IPC correlation, console bridge, AriFS v1, cross-boot updates,
asynchronous broker, per-file permissions, packages and installer, WASM validator.

**Seven open findings: two P1 and five P2.** There are host and QEMU
reproductions. No fixes have been applied to production code.

## Validation

The official suite runs in a copy from `git archive 6ee19fb`, at
`/var/tmp/aricode-review-6ee19fb-uhBqbF`, on disk, not in the `/tmp` tmpfs.
Because another suite was running in the repository, test ports
17007–17010 were replaced with 31007–31010 in constants, URLs and expectations
in that copy. No algorithms or scenarios were changed to obtain results.

| Check | Result |
| --- | --- |
| `cargo xtask check` | Pass |
| Official host suite | 231 passed, 0 failed |
| `cargo xtask test --all` | 100 scenarios: 96 passed, 4 failed on count expectations (finding 7); exit 1 |
| Additional host probes | 7 failing regression assertions: 3 persistence, 3 WASM validation and 1 loop execution |
| WASM reference with Node | Rejects the three invalid modules; the valid loop returns 0 |
| QEMU: journal, abandoned replace and WASM import | All three defects reproduced; the system keeps running and shuts down with code 33 |
| QEMU: service not responding at startup | Two processes remain alive after being declared failed; later PCI claims fail |

The additional probes are not part of the 231 official tests. Their failures
are deliberate assertions of the expected behaviour, not build errors.
Evidence and reproduction: [6ee19fb-review](6ee19fb-review/README.md).

## 1. [P1] Chunked commit allows reading leftovers from another application's journal

Code: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
lines 278–287 and 309–325;
[libs/arifs/src/lib.rs](../../libs/arifs/src/lib.rs), `replace_chunk` and
`commit_header`, lines 479–539. Introduced by `9ce36fc`.

`Staging` only stores the file and the owner. It does not record which bytes the
transaction received. `ReplaceCommit` accepts a length larger than the chunks sent,
and computes the CRC over whatever is in the global journal, including leftovers from
a previous replace. Capping the length at 32 KiB does not check where the bytes came from.

In QEMU, `reviewa` and `reviewb` are distinct applications, with private
directories and only `caps = fs`. A replaces its file with a synthetic string.
B sends a one-byte chunk to its own file and commits 22 bytes. B reads:

```text
review: A wrote private fixture
review: B read from its own file: XYNTHETIC_PRIVATE_DATA
```

B received neither a shared file nor the root endpoint. File permissions
are enforced on open, but the data crosses through the shared journal.

Proposed fix: define a transaction with an expected length and coverage
of every byte, or require consecutive chunks starting at zero; reject an incomplete
commit. Initialise a new transaction without reusing another owner's data.
Regression: two namespaces, a prior replace and a commit with gaps or excessive
length; no byte from the previous transaction may appear.

## 2. [P1] Missing barriers before publishing metadata that depends on other writes

Code: [libs/arifs/src/lib.rs](../../libs/arifs/src/lib.rs), lines 400–403
and 451–458. The new persistence and its contract were added in `61d141a`.

The `BlockDevice` contract allows sector reordering until `flush`.
`create_sized` writes the superblock and the entry without an intermediate barrier; the
comment assumes a durable ordering that the interface does not guarantee. If a power cut
keeps the entry but loses the `data_next` advance, the next mount
accepts both pieces of metadata with valid CRCs and allocates the same space again.

A probe with a write cache, atomic sectors and a cut before `flush`
produces `first_lba=70, second_lba=70`. Two different files end up overlapping.
The same model applied to `write` keeps a three-byte size without keeping
its data: after reopening, `[0, 0, 0]` is read, although `NEW` was written.

An operation that is not yet durable may be lost; it must not allow
recovery to mount overlapping files. The existing power-cut tests
in write order do not cover this reordering case allowed by the contract.

Proposed fix: persist the allocation before publishing the entry, and the
data before publishing a size increase, or include those operations in a
recoverable transaction. Also check range consistency and allocation at
mount. Regression: a disk model with a cache and selective sector persistence,
not just a counter that interrupts writes in their original order.

## 3. [P2] A client that exits mid-replace leaves the global journal occupied

Code: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
lines 267–285 and 309–324. Introduced by `9ce36fc`.

The `srv.staging` reservation is only released by an explicit descriptor close,
a commit or certain errors. It has no timeout and no mechanism tied to the
client's death. Namespace endpoints are kept in fsd and shared
across launches, so process exit does not trigger that cleanup.

QEMU reproduction: A sends the first chunk and exits without closing; B attempts a
normal replace through `FsClient::replace`:

```text
review: exiting after chunk without commit/close
user: process 22 'reviewa' exited with code 0
review: BUSY after other app exited
```

Atomic replaces by other applications are affected, including the
installer. This does not claim that all system reads or writes block.

Proposed fix: transaction identity and cancellation/recovery when the
owner is lost, plus a bounded timeout for an abandoned transaction. Reject
commits of expired transactions. Regression: kill the client between two
chunks and check that another client can complete a replace without restarting fsd.

## 4. [P2] The supervisor loses the live process when its startup probe times out

Code: [services/init/src/main.rs](../../services/init/src/main.rs),
lines 530–542. Introduced by `4607e5d`.

When the probe fails, `launch` calls `announce_death(StartTimeout)`.
This function takes and closes the process handle, but neither terminates it nor waits for
it to exit. `HANDLE_CLOSE` only drops the handle: the process may keep running
and keep its devices. The supervisor schedules another generation and loses
the reference it used to watch the previous one.

QEMU reproduction with a fixture: replace the `virtio_input` binary in the initrd
with the existing `cpu_hog`, which does not respond and does not exit. Neither init nor
the kernel is modified. After both probes time out, `pci: already claimed` appears on retry;
`services --detail` shows the services stopped while `ps` shows PIDs 14
and 15 of `virtio_input` still alive.

Proposed fix: distinguish "not responding" from "exited". Keep the handle
until exit and cleanup are confirmed; if termination from the supervisor is added,
wait for it to finish before claiming PCI. If it cannot be terminated, mark the
service as failed-but-alive and do not launch another instance as if it had been released.
Regression: slow or permanently blocked startup with PCI ownership retained.

## 5. [P2] The runner resolves WASM imports without checking their signatures

Code: [apps/wasm-runner/src/main.rs](../../apps/wasm-runner/src/main.rs),
lines 319–345 and 142–143. New file imports in `6ee19fb`.

Resolution checks module, name and capability, but does not compare
`imp.type_index` with the host function's signature. The validator only knows
the signature declared by the module; it cannot check the Rust API on its own.
The host accesses `args[0..3]` assuming four arguments.

A valid 66-byte module imports `aricode.file_write` as `() -> i32` and
calls it from `main(i64) -> i32`. It passes `Module::parse`, has `files` granted and
in QEMU ends with `panic in user program`, code 255. The real signature
requires four `i32`. It is an isolated runner failure, not a kernel panic.

Proposed fix: attach parameters and results to each `Api`, compare them
at link time and reject the mismatch before running `start`. Also check
the signature of `main`; keep defensive length checks in
the host. Regression: imports with fewer arguments, wrong types and
incompatible results; they must produce a controlled link error.

## 6. [P2] The new validation lets structurally invalid modules through

Code: [libs/wasm/src/validate.rs](../../libs/wasm/src/validate.rs),
lines 490–527, especially the export check at 510–514.
Introduced as full validation in `6ee19fb`.

Bodies and certain indices are validated, but these three minimal modules,
kept in the evidence, are accepted:

- `wrong-global.wasm`: global declared `i32` initialised with `i64.const`.
- `missing-memory.wasm`: export of memory 99 with no memory defined.
- `inverted-limits.wasm`: memory with minimum 2 and maximum 1.

The three probes that expect rejection fail; Node's `WebAssembly.validate` returns
`false` for all three. The parser discards part of the information needed to
revalidate initialisers, and the validator only checks function exports.
It is not enough that later execution may fail: the new API claims to reject
the module before instantiating it.

Proposed fix: keep and validate constant expression types,
limits and all index spaces; also complete the structural checks
and those for the advertised profile. Regression: positive/negative corpus
cross-checked against another engine. The [module validation specification](https://webassembly.github.io/spec/core/valid/modules.html)
defines these obligations; the three probes use basic features, not
extensions outside the profile.

## 7. [P2] The new image leaves expectations in the mandatory suite out of date

Code: [tools/xtask/src/image.rs](../../tools/xtask/src/image.rs), lines
650–653; [filesystem_read_write.toml](../../tests/qemu/filesystem_read_write.toml),
line 12, and [filesystem_replace_recovery.toml](../../tests/qemu/filesystem_replace_recovery.toml),
line 18; [shell_write_persists.toml](../../tests/qemu/shell_write_persists.toml),
line 28; [usb_image_boot.toml](../../tests/qemu/usb_image_boot.toml),
line 19. Regression from `6ee19fb`.

The image adds `wasm_runner/demo.manifest` next to the module, going from three to
four initial files. Four scenarios still use
the old counts: three at mount, or five after creating files across
boots, instead of four and six. `filesystem.read_write`,
`filesystem.replace_recovery`, `shell.write_persists` and `usb.image_boot` fail.
They reach their operations and finish, but the expectations break
`cargo xtask test --all`.

Proposed fix: update the scenarios together with the fixture, or check
the required files without coupling persistence tests to the number of WASM
files. Regression: the affected scenarios must pass with the image actually built.

## Fixes from the previous review

`fb60352` includes the four fixes from `4836023`: PCI filter by xHCI
interface, probe before publishing the entry, corrected FS/LS intervals, capacity for
21 HID events and an initial eight-byte descriptor before adjusting EP0.
The HID/interval host tests and the QEMU scenarios `usb.hid`,
`usb.hid_boot` and `usb.hid_boot_with_ehci` pass in this suite.
The `usb.image_boot` failure is the count expectation from finding 7,
not an xHCI enumeration regression.
The physical FS case with a 64-byte EP0 remains unverified on hardware.

## Additional observation: WASM loops with parameters

The probe `valid_loop_parameters_preserved_on_branch` validates a module whose loop
takes an `i32`, decrements a counter and uses `br_if` to repeat. Node accepts it
and returns 0; AriCodeOS also validates it, but executing it returns
`Trap("empty stack")`. `Instance::branch` keeps zero values when branching to a loop
instead of its parameters, in [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs),
lines 657–665. That logic was already in `4836023`; it is recorded outside the seven
new findings, although it affects the profile now documented. Temporarily rejecting
that form or implementing its label parameters would avoid advertising it as supported.

## Resolution (branch `fix/audit-6ee19fb`, 2026-09-30)

| # | Status | Fix | Regression |
|---|--------|------------|-----------|
| 1 | Fixed | The library tracks the transaction: a chunk at 0 starts it, subsequent chunks must be contiguous and `replace_commit` only accepts the exact length written by that transaction (`FsError::BadTransaction`, `Invalid` over IPC); fsd discards the transaction on any error, close or change of owner. | Host: `chunked_commit_only_covers_its_own_bytes` (22-byte commit after a 1-byte chunk over another owner's leftovers, gaps, chunk not starting at 0, commit of another file). QEMU: `fs_victim` replaces with `XYNTHETIC_PRIVATE_DATA` and `fs_probe` attempts the same incomplete commit: `Invalid`, no leftovers (`sandbox.manifest`). |
| 2 | Fixed | `create` does a `flush` between the superblock and the entry; `write` flushes the data before publishing a larger size; `open` rejects (`Corrupt`) entries outside the allocated area, with a size larger than their capacity or overlapping, even if their CRC is valid. | Host: `reordered_metadata_never_overlaps_files` with a disk that keeps only a subset of the pending sectors (table only or superblock only): the next file never reuses the space, a published size always has its data, and an overlapping table with a correct CRC is not mounted. |
| 3 | Fixed | The journal reservation in fsd carries the time of the last chunk (fsd receives `SYSTEM` from init); after 2 s without activity, another client takes it over and the abandoned transaction is discarded. It is also discarded when the descriptor is closed or on an error. | QEMU: `fs_victim` leaves an uncommitted chunk and exits; `fs_probe` gets `Busy` and, once it expires, completes its replace without restarting fsd (`sandbox.manifest`). |
| 4 | Fixed | New syscall `process_kill` (`MANAGE` on the process): marks termination and wakes the thread, which terminates on leaving the current syscall, on waking from a wait (`wait_until_deadline` observes it) or on the next tick if it was in ring 3. When the probe times out, the supervisor kills the live process and waits for it to be reaped before announcing the death; if it does not terminate within 2 s, the service is left failed without relaunching. | QEMU: `services.start_timeout` (`service_hang`): a service that never responds is terminated "by the supervisor" in each generation and, once the budget is exhausted, none of its processes remain alive. |
| 5 | Fixed | The runner attaches a signature to each host function and compares it with the type declared by the import before linking (`incompatible signature`, code 14); it also requires `main(i64) -> i32` (code 15). Accesses to `args` are still by index over an already-checked signature. | QEMU: `wasm.bad_import` with the audit module (`file_write` as `() -> i32`): rejected at link time, no panic. |
| 6 | Fixed | The parser keeps the type of constant expressions and the table limits; the validator requires each global's initialiser to have its type, table, memory and global exports to point to something that exists, and memory and table limits to be consistent (maximum ≥ minimum, ≤ 65536 pages); element and data offsets must be `i32.const`. | Host: `audit_fixtures_structural_validation_and_loop_params` with the three audit modules (`wrong-global`, `missing-memory`, `inverted-limits`), rejected with their reason. |
| 7 | Fixed in `4c0de50` | File counts in the four scenarios updated for the image with `demo.manifest`. | The four scenarios have passed in the full suite since then. |
| Obs. | Fixed | Labels store their parameters and a `br` to a `loop` keeps as many values as the block has parameters (previously none). | Host: the audit's `loop-params` module returns 0, as in the reference. |

## Limits and resolution

Status of the seven findings: **open**. This turn only adds the report and
evidence. No branches were changed, no commits created and the tests
that another process was running in the repository were not altered.

The review covers the stated range, in greater depth on persistence,
supervision, packages and WASM. It does not certify the absence of other defects, physical
hardware, the full suite forced to four CPUs or resilience to every storage
failure. The update `kill_on` scenarios cut power on seeing a
marker before the writes: on their own they do not determine every intermediate
point of the journal or sector reordering.
