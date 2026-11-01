# ADR 0071: WebAssembly through wasmi, profile v2

- **Date:** 2026-11-01
- **Status:** Accepted. Step 6 of ADR 0063's order of work, its second
  part. The owner chose on 2026-11-01, after the measurements below:
  - to adopt wasmi;
  - to enable every stable feature except SIMD.

  Changes ADR 0019 (the profile and the interpreter) and ADR 0055 (fuel,
  which wasmi now meters).

## Context

`aricode-wasm` (ADR 0019, ADR 0055) was this system's own interpreter:
- the MVP's integers, sign extension, part of bulk memory;
- an up-front validator;
- fuel that counts work;
- no `unsafe` outside its tests.

ADR 0063 asks for its comparison with wasmi. wasmi 2.0.0 (crates.io, MIT
or Apache-2.0) is the reference WebAssembly interpreter written in Rust:
- `no_std` with `alloc`;
- validation by wasmparser (Bytecode Alliance);
- fuel metering;
- two audits: SRLabs on 0.31 (2023) and Runtime Verification on 0.36 to
  0.38 (2024, three critical or high findings, fixed).

## Measurements

The scratch harness (`aricode-scratch/wasm-compare`) runs both
interpreters on the same inputs.

### Spec testsuite

The WebAssembly testsuite (main branch of 2026-11-01). Over the 62 files of
the MVP and the integer-only features `aricode-wasm` covered, counting
commands per interpreter:

| | Pass | Fail | Not checkable | Invalid or malformed modules accepted |
|---|---|---|---|---|
| `aricode-wasm` | 7 732 | 348 | 2 862 | 313 |
| wasmi 2.0 | 10 157 | 44 | 741 | 0 |

- "Not checkable" for `aricode-wasm`: floating point and SIMD values,
  several results (its API returned one), registered modules. For wasmi,
  the harness's own gaps (some `spectest` imports, the newest text
  syntax); wasmi runs the whole testsuite in its own CI.
- `aricode-wasm` returned no wrong result and never panicked. Its
  failures were all at loading:
  - 313 invalid or malformed modules accepted, among them:
    - custom section names that are not UTF-8 (176);
    - alignments larger than natural (35);
    - LEB128 integers too long or too large (27);
    - content after the last section (20);
    - duplicate export names (18);
    - section sizes that do not match (7);
    - memory or table instructions with no memory or table, sizes over the
      maximum, two `start` sections;
  - 35 valid modules refused, outside its profile but misreported as
    malformed.
- Over the whole testsuite, `aricode-wasm` also refused a valid module
  `rustc` produces: a closure called through `dyn FnMut`
  (`call_indirect: table`), found while writing the benchmark below.

### Speed, host

An integer-only module (`rustc`, `wasm32-unknown-unknown`), median of
several calls, release builds, both with fuel on:

| Workload | `aricode-wasm` | wasmi | Ratio |
|---|---|---|---|
| `fib(25)`, recursive | 28.5 ms | 1.8 ms | 16× |
| sieve over 100 000 bytes | 32.4 ms | 0.96 ms | 34× |
| SHA-256, 64 KiB | 59.6 ms | 4.9 ms | 12× |
| 64×64 matrix product | 32.3 ms | 2.1 ms | 16× |
| `br_table` dispatch, 10⁶ steps | 330 ms | 8.8 ms | 38× |

`aricode-wasm` decoded the WebAssembly bytes as it executed them. wasmi
translates each function once into a register-based code of its own.

### Speed, inside AriCodeOS

The sample module's benchmark (`run wasm_runner 99`, scenario
`wasm.bench`), QEMU TCG, the same module and results on both runners:

| Workload | Before | After | Ratio |
|---|---|---|---|
| `fib(22)` | 90 ms | 6 ms | 15× |
| sieve over 100 000 bytes | 404 ms | 14 ms | 29× |
| SHA-256, 16 KiB | 233 ms | 11 ms | 21× |
| 64×64 matrix product | 498 ms | 21 ms | 24× |
| `br_table` dispatch, 500 000 steps | 1 976 ms | 57 ms | 35× |

### Size and memory

| | Before | After |
|---|---|---|
| `wasm_runner` code | 71 KB | 892 KB |
| initrd | 4.66 MB | 5.82 MB (limit 12 MiB) |
| heap to load and instantiate the sample module, beyond its memory | 6 KiB | 25 KiB |
| `unsafe` in the interpreter | none outside tests | about 340 (wasmi), 1 (wasmparser) |

### Security history

wasmi had two memory-safety advisories, both fixed before 2.0.0:
- CVE-2024-28123, an out-of-bounds write in host calls with more than 128
  parameters;
- CVE-2025-66627, a use-after-free in linear memory.

`aricode-wasm` could not have such a bug, having no `unsafe` code. A module
that escaped wasmi would hold what the runner holds:
- `SYSTEM` for reading;
- its private directory;
- its DMA quota;
- the console.

## Decision

1. **The runner uses wasmi 2.0.0**, pinned, with its validator.
   `aricode-wasm` is removed.
2. **Profile v2** (`docs/wasm-profile.md`): the stable features except
   SIMD:
   - floating point;
   - all of bulk memory;
   - reference types;
   - multiple tables;
   - tail calls;
   - 64-bit memories.

   The audit took two features back:
   - **extended constants:** wasmi evaluates them recursively (H2);
   - **multiple memories:** wasmi's strict limits allow one (M2).

   Imports stay functions of the `aricode` module, granted by the module's
   manifest.
3. **Fuel stays ADR 0055's**:
   - 50 M units for `start` and 50 M for `main`;
   - one unit per 64 bytes that bulk operations move (wasmi's default);
   - imports priced by the runner and charged before they run;
   - translating a function also costs fuel.
4. **Memory comes from the quota.** The runner adds three 2 MiB DMA
   buffers to its heap (`aricode_rt::extend_heap`). wasmi's state and the
   module's memory live there. What a module may hold is bounded:
   - by `StoreLimits` and the engine: one memory of 1 MiB, four tables of
     16 Ki elements, one instance, a 512 KiB value stack, 1024 nested
     calls;
   - by wasmi's strict limits when the module is parsed: 10 000 functions
     of 40 bytes on average, 1000 globals, 1000 element and data segments,
     32 parameters and results;
   - by the runner before that: blocks nested at most 1024 deep, since
     wasmi's translator cannot fail gracefully when its control stack runs
     out of heap.

   The quota grows from 512 KiB to 7 MiB. The DMA buffers are allocated,
   physically contiguous and zeroed, at every run.
5. **The policy is a library** (`libs/wasm-host`), tested on the host with
   wasmi: the engine, the limits, the charging of imports, the manifest,
   the Capability API's names and signatures. The runner keeps the system
   calls.
6. **Dispatch.** wasmi's `auto-dispatch` stays on:
   - optimized builds chain the instruction handlers as tail calls;
   - unoptimized builds use a loop.

   Without it, the tail calls run at every optimization level and, without
   the optimizer, grow the stack per instruction: the host test of an
   endless loop overflowed. The loop alone is 2 to 4 times slower
   (`portable-dispatch`, measured).

   In the release runner two of the 1106 handlers, `memory.grow` and
   `table.grow`, call the next handler instead of jumping to it (audit,
   H1). The trade-off is accepted:
   - a module that loops on them exhausts the runner's 64 KiB stack after
     a few hundred grows;
   - the guard page stops it, and the kernel kills the runner and nothing
     else (`wasm.grow_loop`);
   - the module ends its own run, which it could do anyway.

   `cargo xtask stack-check` disassembles the release runner and fails if
   any other handler dispatches with a call, so a wasmi or compiler
   upgrade cannot add more unnoticed.
7. **Watching wasmi.** Its version is pinned in `Cargo.lock`. A new
   advisory against the version in use is a release blocker.

## Consequences

- WebAssembly programs run 15 to 35 times faster, and floating-point Rust
  programs run at all.
- The validator refuses everything the testsuite says is invalid or
  malformed.
- The runner's binary grows by 821 KB, and its quota to 7 MiB of DMA
  memory, allocated at every run.
- The sample module's benchmark is part of the full suite (`wasm.bench`),
  with pinned results.

## Limits

- **Memory safety now rests on wasmi's `unsafe` code.** Containment is the
  runner's isolation and its few capabilities.
- **Optimized builds rely on the compiler turning the dispatch's calls into
  tail calls.** Stable Rust has no guarantee (`become` is unstable).
  - `memory.grow` and `table.grow` are not turned (decision 6).
  - `stack-check` holds the rest.
  - `wasm.fuel` runs 50 M units of an endless loop on the release build
    and ends out of fuel, not out of stack.
- **`print` costs one unit per byte**, as ADR 0055 accepted: 50 M units
  still buy about 50 MB of console output.
- **NaN payload bits** may differ between machines, as the specification
  allows.

## Tests

- Host (`libs/wasm-host`, 16 tests), on a build without the optimizer (the
  dispatch loop):
  - floating point and multiple results run;
  - SIMD, exceptions and GC are refused;
  - extended constants and blocks nested past 1024 are refused;
  - wasmi's strict limits on functions and globals, and a value stack that
    holds 1000 calls with 16 locals each;
  - a module cannot write its own code or manifest;
  - an endless loop ends out of fuel;
  - an import the module cannot pay for never runs;
  - memories grow to the limit and no further;
  - modules over the limits are not instantiated;
  - deep recursion is a stack overflow trap;
  - bulk memory pays for its bytes;
  - an ill-typed module is refused;
  - the manifest, the API's signatures and file names.
- QEMU, the `wasm` area, 8 scenarios:
  - `wasm.demo`, `wasm.restricted`, `wasm.bad_import`, `wasm.invalid` and
    `wasm.many_blocks`, as before, with wasmparser's message for the
    invalid module;
  - `wasm.bench`, the benchmark with its results pinned;
  - `wasm.fuel`, the endless loop, on the release build's tail-call
    dispatch;
  - `wasm.grow_loop`: on the release build only the runner is killed, by
    its stack guard; on a dev build it ends out of fuel.

  All eight on release and dev builds.
- `cargo xtask stack-check`: the release runner's handlers dispatch with a
  tail jump, `memory.grow` and `table.grow` excepted (unit test of the
  check on a sample disassembly).

## Audit (2026-11-01)

An independent audit reproduced the runner on the host: the same engine,
heap and limits, a 64 KiB stack, and the release profile. It checked and
found correct:
- the host functions' bounds (i32 pointers through u32, `checked_add`);
- every price charged before its effect;
- a missing or wrong `memory` export;
- memory64 and multiple memories in the API;
- the import checks;
- the private directory;
- the refused features (wasmi starts from no feature);
- fuel on translation, grows and bulk operations;
- the `extend_heap` contract and the multi-region heap;
- that no advisory applies to wasmi 2.0.0.

Applied:
- **H1 (high).** `memory.grow` and `table.grow` are not tail-dispatched in
  the release runner, so a module overflows the runner's stack with almost
  no fuel. Accepted, as decision 6 says: only the runner dies, by its guard
  page (`wasm.grow_loop`), and `stack-check` fails if any other handler
  does the same.
- **H2 (high).** A 4 KB extended constant expression overflowed the stack
  at instantiation (wasmi's recursive evaluation): the feature is off.
- **M1.** 17 000 nested blocks made wasmi's translator exhaust the heap,
  and the runner panic: nesting is limited to 1024 before wasmi parses
  the module.
- **M2.** Nothing bounded functions, globals or segments: wasmi's strict
  limits are on, so the module has one memory.
- **L1.** The value stack limit is in bytes; it was 64 KiB, and is now the
  intended 512 KiB.
- **L2.** Four memories of 1 MiB could not all grow in three separate
  chunks. Now there is one memory.
- **L4.** A module with `files` could rewrite its own manifest or code
  (this predates ADR 0071): `demo.wasm` and `demo.manifest` are no longer
  writable.
- Two corrections to this ADR: the DMA buffers are allocated at every run,
  not on use; and the host tests run without the optimizer, so they cover
  the dispatch loop, not the tail calls.

Recorded: L3 (`print`'s price, ADR 0055) under Limits.
