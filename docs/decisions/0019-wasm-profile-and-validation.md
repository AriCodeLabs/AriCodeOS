# ADR 0019: `aricode-wasm` v1 profile: up-front validation and per-module manifest

- **Date:** 2026-09-29
- **Status:** Accepted

## Context

The interpreter (Phase 29) trusted the module's producer: it checked
bounds at run time, but an ill-typed module could fail halfway through,
and the Capability API was decided by the runner without the module
declaring what it needs. The 2026-09-28 study (stage 4, section H) asks
for a validator, a per-module manifest with granted resources kept
separate from the runner's privileges, files and clock, and an explicit
profile that can be checked outside the system, without claiming WASI
support.

## Decision

1. **Validation before instantiation.** `Module::parse` runs the
   specification's typing algorithm over every function (typed operand
   stack, control frames, polymorphic unreachable code) and checks
   indices, table, globals, exports and `start`. What does not validate is
   not instantiated (`Error::Invalid`); what the interpreter does not
   execute (floating point, SIMD, other extensions) is rejected here, not
   with a trap halfway through execution.
2. **Own, explicit profile** (`docs/wasm-profile.md`): MVP with integers,
   `sign-ext`, `memory.copy`/`memory.fill`, `select t`, block types by
   index; one table, one memory, imports only from the `aricode` module.
   WASI is not claimed: a WASI module does not work and is not meant to.
3. **Per-module manifest** (`NAME.manifest`, `caps = print, clock,
   files`): the runner grants an import only if the manifest asks for the
   capability that covers it; everything else does not exist for the
   module, even if the runner holds the resource. The module's resources
   are a subset of the runner's (its private directory, its clock), never
   more.
4. **Files and clock** in the Capability API: `file_write` (atomic replace
   in the runner's private directory) and `file_read`, with short names
   without `/`, plus `uptime_ms` and `print`. Calls that talk to a service
   inherit the SDK timeouts (ADR 0012).

## Rejected alternatives

- Integrating an external runtime (wasmtime, wasmi): license, size and
  `no_std` do not fit today with a kernel and an SDK that fit in a few
  hundred KiB; the in-house validator covers the subset that is executed.
- A "minimal" WASI preview 1: requires a descriptor and errno model the
  system does not have, and would promise compatibility that cannot be
  delivered.
- Manifest inside the module (custom section): harder to read and to
  change without recompiling; a file alongside is the same thing native
  applications do.

## Consequences

- `wasm.demo` validates and runs the real module from `rustc`, which
  writes and rereads a file; `wasm.restricted` (manifest with only
  `print`) does not run because of an import that is not granted;
  `wasm.invalid` rejects an ill-typed module and reports the reason.
- The `aricode-wasm` host tests cover empty stack, types, leftover values,
  labels, `if` without `else` with results, out-of-range indices, unknown
  opcodes and polymorphic unreachable code.
- Extending the profile (floating point, more API) goes through the
  validator and this page; the profile is versioned.

Amended by ADR 0055 (2026-10-25): fuel counts work, imports pay fuel, and new
limits on arity, locals, tables and memories.
