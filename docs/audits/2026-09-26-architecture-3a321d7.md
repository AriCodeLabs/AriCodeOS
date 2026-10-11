# Architecture documentation review

Reviewed commit: `3a321d7` (0.0.5, phases 11 and 12).
Document: [architecture.md](../architecture.md).
Diff compared: `9d8c7e5..3a321d7`, plus a full read of the current document
to detect contradictions between sections.

Documentation review complete. The claims below were checked against the code
of the same commit. This is not an exhaustive audit of phases 8–12 nor a
validation of their execution. Cited line numbers refer to that commit.

## Findings

### 1. [P2] The published lock order contradicts the actual nesting

Document: line 179, `heap -> pmm -> vmm -> console`.

In [vmm.rs](../../kernel/src/memory/vmm.rs), lines 166–168, `with` holds
`VMM.lock()` during the callback. If the mapper needs a table, it calls
`PmmFrames::alloc_frame` (lines 33–34), which takes the PMM. The nesting is
therefore VMM -> PMM. During heap growth, the frame allocated beforehand through
the PMM is obtained before entering the VMM: the sequence of calls does not
imply holding both locks in that order.

Following the published rule and calling the VMM with the PMM held can cause
reentry into the PMM when a table is needed. No current deadlock has been
reproduced: the finding is that the documented rule induces incorrect usage.
The same stale comment appears in `kernel/src/sync.rs:5–6`.

Proposed fix: document the locks that are actually nested, distinguish the
growth lock from the allocator lock, and forbid entering the VMM while
holding the PMM. Also review the comment in `sync.rs`.

### 2. [P2] Claims from old phases are kept as current state

Document: lines 41, 106–108, 189–190 and 295–297.

The document claims that `SingleCpu` exists, that there is no APIC, that external
interrupts are disabled, that the console has no lock and that
`time::sleep_ms` waits with `hlt` without a scheduler. This contradicts the
synchronization, interrupt and scheduler sections of the document itself.

Evidence from the current code:

- [main.rs](../../kernel/src/main.rs), lines 109–111: initializes the APIC and enables interrupts.
- [console.rs](../../kernel/src/console.rs), lines 15 and 44: `IrqLock` and `try_lock`.
- [sync.rs](../../kernel/src/sync.rs), line 33: `IrqLock` alias; `SingleCpu` is no longer defined.
- [time.rs](../../kernel/src/time.rs), lines 27–28: delegates sleeping to the scheduler.
- [exceptions.rs](../../kernel/src/arch/x86_64/exceptions.rs), lines 12–20:
  #BP continues; other user exceptions terminate the process;
  kernel exceptions are fatal; external vectors are dispatched to the APIC.

Proposed fix: update the old sections to the 0.0.5 state and remove
already-implemented functionality from the debt list. If history is to be
kept, mark it explicitly as the behaviour of a past version.

### 3. [P2] The TSS selector is no longer 0x18

Document: line 99.

[aricode-x86-cpu](../../libs/x86-cpu/src/lib.rs), lines 159–161, defines the
user selectors and `TSS = 0x28`. [gdt.rs](../../kernel/src/arch/x86_64/gdt.rs)
inserts user data and code before the TSS descriptor and checks the layout.
GDT entry index 3, at offset 0x18, now holds user data, not the TSS.

Proposed fix: publish the full current layout, including the user selectors
with RPL 3, and keep a single reference for the CPU and user-mode sections.

### 4. [P2] The paging contract describes a retired API

Document: lines 60–62.

It describes a nonexistent `PagePool` and claims that the only `unsafe`
operation for the caller is taking ownership of its region. The current
implementation in [aricode-x86-paging](../../libs/x86-paging/src/lib.rs) uses
`Mapper<A: FrameAlloc, M: PhysMap>` (line 126), an `unsafe PhysMap` contract
(line 47) and an `unsafe share_kernel_half` operation (line 346) that allows
sharing tables created by another mapper.

Proposed fix: replace the historical description with the contracts of the
frame providers and physical translation, and explain the validity and
lifetime required of shared tables. Do not attribute to the mapper exclusive
ownership of every table it walks.

### 5. [P3] The crate list identifies BootInfo as v2

Document: line 28.

[boot-protocol](../../boot-protocol/src/lib.rs), line 14, sets
`BOOT_PROTOCOL_VERSION = 3`; the diagram and the contract section already say v3.
Proposed fix: update the crate list to v3.

### 6. [P3] The allowed duration is confused with the sleep test's error

Document: line 229, "50 ms with 5..7 ticks of error".

[scheduler.rs](../../kernel/src/task/scheduler.rs), lines 374–382, compares
the total duration against `5..=7`. At 100 Hz that is 50–70 ms in total, not a
tolerance of 50–70 ms extra.

Proposed fix: write "50 ms, with an observed duration of 5–7 ticks".

### 7. [P3] The invalidation debt omits thread stacks

Document: line 294, "only the heap and the self-test invalidate".

[stack.rs](../../kernel/src/task/stack.rs), lines 54–59, unmaps each page
of the stack, runs `invlpg` and returns the frame to the PMM.

Proposed fix: include that path and separate the already-implemented local
invalidation from the absence of a generic TLB shootdown mechanism.

## Minor observations

- Line 154: `Vec`, `Box`, `String` and `format!` come from the `alloc` crate;
  `core::alloc` provides interfaces such as `Layout` and `GlobalAlloc`.
- Lines 280–281: phase 12 is presented as future work although the document
  states it is complete. Keep the one-thread-per-process limitation and the
  absence of `mmap`, but adjust the roadmap reference without inventing an
  implementation phase.

## Verification and resolution

- Clean working tree at start; HEAD `3a321d7`.
- Evidence obtained by reading the diff and checking against the current code.
- No builds or QEMU runs were performed: this is a documentation review.
  Test results from earlier audits are not presented as validation of this
  commit.
- The heap fixes described in the resolution of the
  [previous audit](2026-09-26-audit-9d8c7e5.md) are present by inspection:
  checked arithmetic, page rollback and separate borrows via locks.
  Their tests were not rerun in this review.
- The seven findings and two observations remain pending a fix.
  Neither `architecture.md` nor production code has been edited.
- Report created on `fix/audit-architecture-3a321d7`, without commit or merge.

---

# Resolution (branch `fix/audit-architecture-3a321d7`)

| # | Finding | Fix |
|---|----------|------------|
| 1 | Incorrect lock order | The Synchronization section publishes the actual nestings verified in the code (`VMM -> PMM`, `heap.mapped -> VMM/PMM`, `PROCESSES -> SCHED`, all others leaf), distinguishes the growth lock from the allocator lock and lists the resulting prohibitions. The comment in `kernel/src/sync.rs` says the same. |
| 2 | Old phase state | CPU section: user exceptions and dispatch to the APIC. PMM section: `Boot` regions are returned by the VMM. Phase 8 section: `time::sleep_ms` delegates to the scheduler. Debt: removed the missing-APIC and lockless-console entries; crate list without `SingleCpu`. |
| 3 | TSS selector | CPU section with the full layout (0x08, 0x10, 0x18/0x1B, 0x20/0x23, 0x28) and a pointer to `aricode-x86-cpu::layout` as the single reference. |
| 4 | Paging contract | Replaced `PagePool` with the `FrameAlloc` and `PhysMap` (`unsafe` trait) contracts and the validity required of tables shared through `share_kernel_half`. |
| 5 | BootInfo v2 | Crate list: v3. |
| 6 | Sleep tolerance | "observed duration of 5 to 7 ticks (50 to 70 ms in total)". |
| 7 | TLB invalidation | Debt rewritten: explicit local invalidation in the heap, thread stacks and self-test; no generic mechanism or shootdown until SMP. |
| obs. | `alloc` vs `core::alloc` | Fixed in the Heap section. |
| obs. | Phase 12 as future | Debt: `memory_map`/user threads point to section 22 and Phases 14 and 27. |

No production code changes beyond the `sync.rs` comment.
