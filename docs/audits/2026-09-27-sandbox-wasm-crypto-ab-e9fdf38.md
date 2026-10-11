# Sandbox, WebAssembly, cryptography and A/B boot review

Date: 2026-09-27. Git base: `e9fdf38`.
Scope: changes after `28e745a`, including phases 28, 29 and 31, the fixes
from the previous audit, and the uncommitted Phase 30 work present when this
review started (workspace version 0.4.0).

The review was done on an isolated copy. The hashes of the flagged files are
in [snapshot.json](e9fdf38-review/snapshot.json): citing only `e9fdf38` does
not reproduce the A/B changes that were not yet committed.
No production code was modified and no fixes were applied.

During the review, Phase 30 was merged in `ade2d04` (implementation `ab4af73`).
The seven production files identified in `snapshot.json` keep exactly the
same hashes in that merge.

## Findings

| # | Priority | Issue | Status |
| --- | --- | --- | --- |
| 1 | P1 | A global descriptor allows access to files in another private directory | Pending |
| 2 | P1 | `keygen` creates the private seed readable by other users | Pending |
| 3 | P1 | A slot is confirmed before service startup completes | Pending |
| 4 | P2 | A pending slot is booted even if persisting the counter fails | Pending |
| 5 | P2 | Denials before `process_spawn_limited` still leak handles | Pending |
| 6 | P2 | Wasm checks accesses against the host reservation, not the module's current memory | Pending |
| 7 | P2 | Wasm keeps stale bytes when instantiating and growing memory | Pending |
| 8 | P2 | The runner ignores the `start` function of Wasm modules | Pending |

P1 requires priority attention; P2 is a functional defect that must be fixed.
This review is not a formal verification of the cryptographic primitives nor
an exhaustive audit of the whole operating system.

## Validation and evidence

- `cargo xtask check`: passes on the original copy, before adding the
  reproductions and adapting the test ports.
- Suite host tests: 184 passing.
- Consolidated QEMU result after reruns: **65 official scenarios passing**.
  The two new scenarios, `review.fs_isolation` and `review.broker_leak`, fail
  reproducibly and confirm findings 1 and 5. The consolidated and individual
  results are kept in `review-results.json`; the initial run is not presented
  as a complete error-free pass.
- The first QEMU run received SIGTERM while another suite was active in the
  workspace. It is not considered a complete run nor a functional failure of
  AriCodeOS.
- The second run uses ports 27007–27010 and different names for the runner
  and QEMU. The production paths of the findings are unchanged; only the test
  infrastructure is adapted and two reproduction scenarios are added.
- The affected scenarios were rerun after finishing the adaptation of the test
  HTTP URLs. The capacity expectation for `virtio.blk_rw` was taken from
  `ade2d04`: during the review it was corrected from 8192 to 98304 sectors,
  matching the A/B disk. These infrastructure and expectation failures are not
  counted among the pending findings.
- The four additional Wasm assertions fail with the results described in
  findings 6–8. They run `aricode-wasm` directly.
- The isolated file test obtains `SECRET` from another prefix and fails its
  isolation assertion. It uses the original extracted handler, AriFS and
  protocol; it simulates the disk and three DMA queries.
- The A/B reproductions run QEMU on their own disks: two read-only boots and
  one boot with a restricted network, respectively.
- The `keygen` test uses a fresh directory and `umask 022`; the generated seed
  is neither kept nor shown.

Local logs are in `build/audit/e9fdf38-worktree/`, and the instructions and
sources for the reproductions are in [e9fdf38-review/](e9fdf38-review/README.md).
Logs and disks are generated artifacts, not versioned files.

## 1. [P1] fsd descriptors are not bound to the private directory

Code: [services/arifs/src/main.rs](../../services/arifs/src/main.rs),
lines 172–195; single table created in `main`, line 224 of the reviewed copy.

`Open`, `Create` and `List` apply the endpoint prefix, but `Read`, `Write`
and `Close` look up the same `Fds` table for every endpoint. The descriptor
number does not include the owning directory, and nothing checks that the
file belongs to the prefix sending the request.

An application with its own private endpoint can probe descriptors 0–15 and
read, modify or close files opened from another directory. This also works
after the owner exits without closing its descriptor, because fsd keeps that
entry. Obtaining the victim's endpoint is not required.

Host reproduction: create `victim/secret.txt`, write `SECRET`, keep
descriptor 0 and send `Read(fd=0)` with prefix `attacker`. Result:
`Status::Ok` and the six bytes `SECRET`, instead of the expected rejection.

The QEMU reproduction uses two applications with `caps = fs` manifests and
different directories. The first writes `VICTIM_SECRET` and exits without
`Close`; the second tries to read descriptors 0–15 without opening that file.
Observed result: `REVIEW_CROSS_NAMESPACE_READ: VICTIM_SECRET` and exit 1
from the attacker. The scenario fails the isolation expectations.

Fix: associate each descriptor with its namespace, or keep one table per
namespace, and validate ownership in every operation, including `Close`.
The regression must attempt cross-namespace read, write and close; checking
only `List` or `Open` by name does not cover this access.

## 2. [P1] The private seed is written with overly broad permissions

Code: [tools/xtask/src/keys.rs](../../tools/xtask/src/keys.rs), lines 32–35
and 48–52.

`keygen` uses `write_hex`, which ends in `fs::write`, for both the public key
and the private seed. It sets no private permissions on the latter. With
`umask 022`, the seed is created with mode `0644`; the new directory is also
accessible under that configuration if its ancestors allow it.

Reproduction: run `xtask keygen` in a fresh temporary directory under
`umask 022`. Result: `ed25519.seed = 0644`, `ed25519.pub = 0644`, exit 0.
This is not about the development key deliberately published in `keys/dev`,
but about the new private keys the command generates.

Impact: other local users who can traverse the directory can copy the seed
and sign as its owner.

Fix: create the seed with `0600` permissions at open time, with exclusive
creation. Do not rely on the umask or on a `chmod` after writing it.
Regression: check that no group or other access bits exist under a permissive
umask and that an existing key is not overwritten.

## 3. [P1] Init confirms the slot before knowing whether boot completes

Code: [services/init/src/main.rs](../../services/init/src/main.rs),
lines 604–608.

`confirm_boot_slot` is called after checking blocks and files, but before
starting and checking network, input, GUI and broker. Multiple later paths
still terminate init with an error. By then the confirmation has already
turned the pending slot into the active one and removed the retry mechanism.

QEMU reproduction: set up A active and B pending with three attempts, and
boot with a restricted user network. The following is observed, in this order:

```text
AriInit: boot confirmed in slot B
net_client: ip 10.0.2.15 router 0.0.0.0 dns 0.0.0.0
AriInit: net_client exited with 4
init terminated with code 18
```

The ARIBOOT sector is left with `active = B`, `pending = None`, attempts 0.
Therefore later boots can no longer exhaust that slot's attempts and fall
back to A automatically because of this initialization failure. The probe
causes an invalid network configuration; the same path affects a new image
whose network service fails after passing the storage checks.

Fix: confirm only after all health requirements of the requested boot have
been met, right before handing control to the shell or declaring the system
ready. Keep the pending state on error exits. Add a regression that checks
the sector after a late init failure.

## 4. [P2] A failure to save attempts does not prevent booting the pending slot

Code: [boot/src/slots.rs](../../boot/src/slots.rs), lines 164–184.

When `disk.write` fails to persist `decision.state`, AriBoot only prints a
warning and returns the chosen image. If it was a pending slot, it boots even
though the attempt decrement was not saved.

QEMU reproduction: attach the disk with `readonly=on`, A active and B pending
with three attempts. Both consecutive boots show:

```text
slots: state active A, pending B (3 attempts)
slots: could not write boot state
slots: booting from slot B
```

Both boots use `test=random` to finish without confirmation from init.
This proves the counter does not advance; it does not simulate a failure of
B's kernel. If B fails to boot, this path can repeat the same slot
indefinitely instead of reaching rollback.

Fix: do not boot a pending slot if its attempt could not be saved. Fall back
to the verified active slot or to recovery, without making that choice depend
on the write that just failed working again. A regression must cover both
write and flush failures of the state.

## 5. [P2] Broker leaks persist on returns before the spawn

Code: [services/init/src/main.rs](../../services/init/src/main.rs),
lines 366–395, especially the `WINDOW` return without a compositor.

The previous fix closes `granted` if `process_spawn_limited` fails, but there
are returns after granting some handles and before reaching that call. For
example, with `caps = system, window`, `SYSTEM` is duplicated and then
`Denied` is returned if the boot does not include the GUI. That duplicate is
not released. The error paths when preparing other services follow the same
pattern.

QEMU reproduction: a `review_window` manifest requesting `system, window`,
boot `qemu_exit shell`, `repeat 300 run review_window` and then
`run hello_app 7`. No ELF is needed for the first name: the denial happens
before trying to load it.

Observed result: after the denials, `the broker could not launch it` appears
and `hello_app` no longer runs. The scenario fails the expectation of its
successful output.

Impact: denials exhaust init's handle table until other launches are blocked.
The existing regression `repeat 300 run no-existe` does not cover this path:
its errors do reach the cleanup added around the spawn.

Fix: use common cleanup or a guard for all handles prepared during `launch`,
including returns due to missing services and duplication failures. Disarm
that cleanup only once ownership has been transferred. Add the case of a
denial after a partial grant.

## 6. [P2] Wasm accesses beyond the current size of its linear memory

Code: [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs), lines 907–919;
also affects data segments, bulk operations and memory passed to the host.

`memory_pages` records the current size, but loads and stores receive the
full slice provided by the host. In the runner that buffer has four pages,
even if the module declares only one. A declared maximum of one page does
not limit these accesses: it only applies when executing `memory.grow`.

Direct reproduction: a module with a minimum and maximum of one page, a
four-page host buffer and `i32.load8_u` at address 65536. With `0x5a` at that
position, it returns `Ok(90)` instead of a trap. No access outside the Rust
slice or outside the process was observed: the module's memory limit is
bypassed.

Fix: check accesses and initialization against `memory_pages * PAGE`,
reserving the total buffer capacity for deciding whether it can grow. Also
pass only the active memory to imports. The specification requires a trap
when an access exceeds the current memory size:
[WebAssembly Core 1.0](https://webassembly.github.io/spec/versions/core/WebAssembly-1.0.pdf).

## 7. [P2] Initial and grown memory are not zeroed

Code: [libs/wasm/src/lib.rs](../../libs/wasm/src/lib.rs), lines 496–505
and 934–936.

`Instance::new` copies data segments without clearing the rest of memory.
`memory.grow` only changes the page counter. If the host buffer contains data
from a previous use, the module receives it as fresh memory.

Two direct tests with a buffer filled with `0xaa` return `Ok(170)`: one reads
byte 0 after instantiation and the other reads the first byte of the newly
added page. Both expect zero. In the current runner, new DMA buffers are
zero-initialized; the defect is in the library's reusable contract and shows
up when using preexisting memory or reusing it across instances.

Fix: clear the active memory before applying the segments and zero each
region added on growth. Keep both regressions with non-empty buffers. Zero
initialization is part of
[WebAssembly memory allocation](https://webassembly.github.io/spec/core/exec/modules.html#alloc-mem).

## 8. [P2] The start function is parsed but not executed

Code: [apps/wasm-runner/src/main.rs](../../apps/wasm-runner/src/main.rs),
lines 151–160; `Module::parse` keeps the index in `module.start`.

After creating the `Instance`, the runner calls `main` directly without
invoking `module.start`. The library does not run it during instantiation
either. A module that depends on that initialization is accepted and runs
with incorrect state; a trap in its start function is also ignored.

Direct reproduction of the same flow: a module has a global initialized to 0,
a `start` function that writes 42, and an export that returns the global.
The result is `Ok(0)` instead of `Ok(42)`.

Fix: run `start` exactly once after initializing memory/tables and resolving
imports, propagating traps before running exports; or explicitly reject these
modules until it is supported. The start function must be invoked during
[WebAssembly instantiation](https://webassembly.github.io/spec/core/exec/modules.html#instantiation).

## Follow-up

Both fixes from the [previous audit](2026-09-27-sdk-28e745a.md) are present
in this copy: enumeration that skips gaps, and cleanup after spawn failures.
Finding 5 identifies a path before the spawn that those fixes did not cover.

All eight findings remain pending fix and regression in `fix/*` branches,
per [WORKFLOW.md](../WORKFLOW.md). This review creates no commits, merges no
branches and does not change the workspace's production files.

## Resolution (branch `fix/audit-e9fdf38`, 2026-09-27)

| # | Status | Fix | Regression |
|---|--------|-----|------------|
| 1 | Fixed | fsd stores in each descriptor the endpoint that opened it; `Read`, `Write` and `Close` require it to match the caller (`BadFd` otherwise). Two launches of the same application share directory and descriptors by design (same identity). | `sandbox.manifest`: `fs_victim` leaves a descriptor open with `VICTIM_SECRET`; `fs_probe`, from another directory, requires `BadFd` on read, write and close for all 16 descriptors and that its own directory still works. |
| 2 | Fixed | `keygen` writes the seed with `OpenOptions` (`create_new` + `mode(0o600)`) from creation; the public key remains readable. | `keys::tests::keygen_sign_verify_round_trip` checks that the seed has no group or other bits. |
| 3 | Fixed | `confirm_boot_slot` runs at the end of the sequence, after network, TCP, HTTP, input and GUI; any earlier error return leaves the state pending. | `update.no_confirm_on_late_failure`: `init_fail_late` (tests only, via cmdline) makes init fail right before confirming; the re-read state is still `pending B (2 attempts)` and there is no confirmation. |
| 4 | Fixed | If the attempt for a pending slot cannot be persisted, AriBoot does not boot it: `aricode-ab::unpersisted` falls back to the active slot while keeping the pending one. | `update.readonly_no_pending_boot` (`disk_readonly = true`, new scenario field): with B pending and a read-only disk, A boots. |
| 5 | Fixed | `Granted` (guard with `Drop`) closes all duplicates prepared in `launch` unless `take` has handed them to the kernel; covers denials due to missing services and duplication failures. | `sandbox.manifest`: `repeat 300 run win_probe` (`system, window` manifest without GUI, denied after granting SYSTEM) followed by `run hello_app 10`, which must work. |
| 6 | Fixed | Loads, stores, `memory.copy`/`fill`, data segments and the memory seen by imports are bounded to `memory_pages * PAGE`; the rest of the host buffer is only used for growth. | `memory_is_bounded_by_current_pages_not_host_buffer`. |
| 7 | Fixed | `Instance::new` zeroes the active memory before the data segments, and `memory.grow` clears the new pages. | `memory_starts_and_grows_zeroed` (buffers filled with `0xAA`). |
| 8 | Fixed | `Instance::start` runs the `start` function (propagating traps); `wasm_runner` calls it before `main`. | `start_function_runs_before_exports` (global set by `start`; `start` with a trap). |
