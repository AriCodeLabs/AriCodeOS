# SMP, IOMMU and real-hardware readiness review

Date: 2026-09-28. Reviewed commit: `3eb4965ab6eed03cd4261975a6b6f251d3541c7a`.
Diff compared: `ade2d04..3eb4965` (92 files). Includes the fixes from the
previous audit, Phase 32 (SMP), Phase 33 (VT-d) and Phase 34 preparation
(USB image, FAT32, PS/2 and diagnostic options).

Two confirmed findings: one P1 and one P2. No fixes have been applied to
production code. Cited line numbers refer to the reviewed commit.

## Validation

| Check | Result |
| --- | --- |
| `cargo xtask check` | Pass |
| `cargo xtask test --all` | Pass: 203 host tests and 77 QEMU scenarios, 0 failed |
| Concurrent reaping of a process, QEMU with 4 CPUs | Confirmed the process is removed while its mapper is still present |
| VT-d context reassignment, QEMU | Confirmed the second domain's context is invalidated when the first is released |

Tests ran on copies of `git archive 3eb4965`, with network ports different
from those of the working tree. The official suite uses its per-scenario CPU
configuration; this is not equivalent to repeating every case with four
CPUs. The two additional tests instrument only another temporary copy.
No physical laptop has been tested.

Evidence and instrumentation patches: [3eb4965-review](3eb4965-review/README.md).

## 1. [P1] Waiting for a process can remove its mapper before its pages are freed

Code: [kernel/src/task/scheduler.rs](../../kernel/src/task/scheduler.rs),
lines 389–404; [kernel/src/user/process.rs](../../kernel/src/user/process.rs),
lines 319–344 and 348–362.

With SMP, `reap` removes the thread from the policy and the table under `SCHED`,
but frees the address space after releasing that lock. In that window,
another CPU can run `finished`: it considers the process finished and reaped
because `scheduler::state(main_thread)` already returns `None`.
`wait_until` then removes its entry from `PROCESSES`.

When the first CPU reaches `release_address_space`, it no longer finds the
process and cannot recover its mapper. Dropping the mapper does not return its
frames to the PMM: that requires `free_user_half` and `into_root_frame`, exactly
the operations that were skipped. The user pages and their tables are lost.
Repeatedly creating and waiting for processes can exhaust RAM.
The local interrupt exclusion that protected the sequence on a single CPU
does not prevent this interleaving across different CPUs.

Reproduction: the `user.hello` scenario with four CPUs and a test barrier
on entry to `release_address_space`. The barrier lets the removal from
`wait_until` run and then lets reaping continue. Result:

```text
AUDIT wait: removed process with mapper=true
AUDIT reaper: process_present=false
```

The scenario finishes successfully with `Exited(42)`: its functional success
does not detect the lost pages. This test forces a permitted interleaving;
it does not measure how often it happens without instrumentation nor does it
aim to be a memory-exhaustion test.

Proposed fix: publish a reaping-completed state after freeing the address
space and the DMA domain, and make `wait_until` wait for that state before
removing the process. Keep the `PROCESSES -> SCHED` lock order; do not solve
it by taking `PROCESSES` inside `SCHED`.

Regression: a barrier test that keeps the process visible while reaping
is pending, followed by repeated create/wait on several CPUs and a check
that frames are recovered.

## 2. [P2] Releasing the old domain disconnects an already reassigned device

Code: [kernel/src/iommu.rs](../../kernel/src/iommu.rs), lines 482–490 and
534–547. Entry path: `mmio_map`, in
[kernel/src/user/syscalls.rs](../../kernel/src/user/syscalls.rs), lines 443–470.

`attach` writes the device's new context and adds `(unit, source)`
to the receiving domain, but keeps that same record in the previous
domain. When the previous process exits, `release` zeroes every context
in its list without checking who currently owns it.
The second process's device then loses its DMA translation.

The sequence is reachable through PCI MMIO capabilities: they are created
with `TRANSFER`, can be passed to another process, and every `mmio_map` calls
`attach` with the receiving PID. Claiming the device a second time with
`pci::claim` is not required.

Additional reproduction inside `iommu::selftest`: assign function
`00:02.0` to the domain of test PID 1001, reassign it to that of 1002 and release
the first. The real context entries built by the kernel in QEMU are queried:

```text
AUDIT context before release: present=true domain=3
AUDIT context after release: present=false domain=0
```

The assertion that the second domain's context is still present fails.
The test calls the IOMMU implementation directly with test identifiers;
it does not perform a full transfer between two drivers nor measure a
subsequent I/O operation. The loss of the context is confirmed.

Proposed fix: define and maintain a single owner per PCI function. If
reassignment is allowed, remove the previous association and validate
ownership before clearing a context in `release`. If it is not allowed,
reject the second `attach` before modifying the tables. The domain list
must also not mark the operation as complete before `context_entry` and
the context write have succeeded.

Regression: assign to A, transfer to B, terminate A and verify that B keeps
the context and can complete I/O; releasing B must finally remove the
context.

## Previous fixes and limits

The eight merged fixes from the
[previous audit](2026-09-27-sandbox-wasm-crypto-ab-e9fdf38.md) have been reviewed:
fsd descriptor ownership, key file permissions, late A/B confirmation,
selection on persistence failure, broker handle cleanup, Wasm memory limits
and zeroing, and `start` execution.
Those findings are not reopened in this review.

USB boot in QEMU validates the GPT/FAT32 image and the loader, but uses
a virtio-blk device. It does not demonstrate support for USB storage,
AHCI/NVMe, nor VT-d/PS/2 compatibility with any specific laptop. Those
limitations are also recorded in `docs/hardware.md`.

## Resolution (branch `fix/audit-3eb4965`, 2026-09-28)

| # | Status | Fix | Regression |
|---|--------|------------|-----------|
| 1 | Fixed | `Process.reaped`: `release_address_space` frees the address space and the DMA domain and only then marks the process reaped and wakes `exit_wait`; `wait_until` does not remove the process until it sees `reaped` (it no longer queries the thread state or yields in the gap). Lock order intact: `PROCESSES` is never taken inside `SCHED`. | `test=process_reap`: 180 `hello` processes created and waited on in batches of 6, with a PMM frame count (discounting heap growth) and an empty process table at the end. Scenarios `user.process_reap` (1 CPU) and `smp.process_reap` (`cpus = 4`, the interleaving from the finding). |
| 3 (found while fixing) | Fixed | The framebuffer console (32 KiB grid) was built by value on AriBoot's 64 KiB stack and overflowed it; with the USB image and `-smp 4` the initrd sat right below and got corrupted (`initrd: invalid image (Truncated)`). `Console::empty()` is `const` and lives in the `static`; `attach` hooks up the screen without moving the grid. In addition, `check_boot_stack` measures the boot stack before leaving it and requires 16 KiB of headroom (panic with diagnostics instead of corruption), and AriBoot and the kernel print the initrd's SHA-256. | `usb.image_boot` with `--cpus 4` (failed before), `empty_console_is_const_and_attaches_in_place`; the whole suite with `--cpus 4`. |
| 2 | Fixed | `State.owners`: a single owner per PCI function. `attach` prepares the context table and the domain before recording anything, writes the context, transfers ownership and removes the function from the previous domain's list; `release` only clears contexts whose owner is still that domain. | `iommu::selftest`: function `00:02.0` is assigned to domain 1001, reassigned to 1002 and 1001 is released: the real context entry (read from the tables with `context_domain`) still points to 1002's domain; releasing 1002 removes it. Scenario `iommu.isolation`. |
