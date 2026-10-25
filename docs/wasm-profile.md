# `aricode-wasm` profile v1

What a WebAssembly module may contain and use in AriCodeOS. It is a custom,
explicit profile (ADR 0019); **it is not WASI** nor the Component Model:
a module written for this profile assumes nothing from POSIX, and a WASI
module does not work here. Anything not listed on this page does not exist
for the module.

## Binary format

- Version 1 of the binary format (`\0asm` + 1). Sections: type, import,
  function, table, memory, global, export, start, element, code, data and
  custom (ignored).
- Value types: `i32` and `i64`. `f32` and `f64` are rejected (constants,
  operations, loads, globals and conversions): `Unsupported("floating
  point")`.
- Extensions accepted on top of the MVP: `sign-ext` (`i32.extend8_s`,
  `i32.extend16_s`, `i64.extend8_s`, `i64.extend16_s`, `i64.extend32_s`),
  `bulk-memory` with only `memory.copy` and `memory.fill`, typed `select`
  (`select t`), block types by type index (parameters and multiple
  results in blocks).
- No SIMD, threads, references (`ref.*`, `externref` tables),
  exceptions, multiple memories, `memory.init`/`data.drop` or
  `table.*`.
- At most one `funcref` table (up to 4096 entries), active element
  segments with an `i32.const` offset. A second table, in the same section
  or another, is `Unsupported("more than one table")`.
- At most one memory (a second is `Unsupported("more than one memory")`); its initial pages must fit in the one the host
  provides (4 pages, 256 KiB). `memory.grow` grows within that buffer and
  returns -1 if it does not fit.
- Imports: functions only, and only from the `aricode` module.
- Limits: 4096 locals per function and 16 Ki locals (parameters
  included) across all active calls, 1000 parameters and 1000 results per
  function type (the WebAssembly JavaScript API limits), 1024 nested
  blocks, modules of up to 64 KiB (loaded by the runner), 64 nested calls,
  16 Ki values on the stack and 50 M units of fuel per exported call. The
  `start` function has its own 50 M, so one run of the runner can spend
  up to 100 M.
- Fuel counts work, not instructions: one unit per instruction, plus one
  per 64 bytes that `memory.fill`, `memory.copy` and `memory.grow` touch,
  per 8 values a branch or a return keeps or a call moves and zeroes as
  locals, and per 64 value types `call_indirect` compares (incident 0042).
  An import costs what the host charges for it before it runs
  (`Host::cost`): see the Capability API below.
- The parsed module and the instance must fit the runner's heap: if they
  do not, the module fails with `module too large for the runner's
  memory`, never a panic. Besides the module bytes, validation keeps side
  tables of at most 14 bytes per 3 bytes of code (~4.7 per module byte, a
  module of nothing but dead `br_table`s; 4 per byte for nothing but
  blocks): every module AriFS stores (32 KiB) fits, while a 64 KiB module
  near those bounds does not.

## Validation

`Module::parse` validates the whole module before returning it: indices of
types, functions, table, globals, exports and `start`, and every body with
the specification's typing algorithm (typed operand stack, control frames,
polymorphic unreachable code). A module that does not validate is not
instantiated: `Error::Invalid(reason)`.

To check a module with external tools before bringing it into the system:

```bash
wasm-tools validate --features=mvp,sign-ext,bulk-memory demo.wasm
```

(`wasm-tools` is not part of the repository's tooling; the reference check
is the validator itself, `cargo test -p aricode-wasm`.)

## How a module is produced

`rustc` for `wasm32-unknown-unknown` with `-C target-cpu=mvp -C
target-feature=+sign-ext,+bulk-memory` and no floating point in the code
(`.cargo/config.toml`, `apps/wasm-demo`). Export `main(i64) -> i32`; the
result is the runner's exit code.

## Module manifest

Next to the module goes `NAME.manifest` (text, `#` starts a comment):

```text
caps = print, clock, files
```

| Capability | Functions it enables |
|---|---|
| `print` | `print` |
| `clock` | `uptime_ms` |
| `files` | `file_read`, `file_write` |

An import whose capability is not granted makes the runner refuse to run
the module (`import aricode.X not granted by the module manifest`,
code 12). Without a manifest it does not run (code 11).

## Capability API (`aricode`)

| Function | Signature | Semantics |
|---|---|---|
| `print` | `(ptr: i32, len: i32)` | Writes `len` bytes of the module's memory to the console. Range outside memory: trap. |
| `uptime_ms` | `() -> i64` | Milliseconds since boot (0 if the runner lacks `SYSTEM`). |
| `file_write` | `(name: i32, name_len: i32, data: i32, len: i32) -> i32` | Creates or wholly replaces (atomic and durable, ADR 0014) a file in the runner's private directory. Returns the bytes written or -1. Name of 1 to 32 UTF-8 bytes without `/`; otherwise, trap. |
| `file_read` | `(name: i32, name_len: i32, buf: i32, len: i32) -> i32` | Reads the file into `buf` (up to `len`). Returns the bytes read or -1 if it does not exist. |

Functions that talk to a service (`file_*`) inherit the SDK timeouts
(ADR 0012): a dead service yields -1, never an infinite wait.

Every import costs fuel before it runs, on top of the call instruction,
so a module's fuel bounds the work it makes the system do (incident 0042,
audit M-a):

| Function | Fuel |
|---|---|
| `print` | 1000 + 1 per byte |
| `uptime_ms` | 100 |
| `file_read` | 10 000 + 1 per 64 bytes of `len` |
| `file_write` | 1 000 000 + 1 per 64 bytes of `len` (at most 50 durable replaces per call of 50 M) |

## Runner errors

| Code | Reason |
|---|---|
| 1 | missing `fs` or `hardware` permission (manifest) |
| 2 | cannot reach the filesystem service |
| 3 | no `demo.wasm` |
| 4 | error reading `demo.wasm` |
| 5 | module larger than 64 KiB |
| 6 | malformed, unsupported or invalid module (the reason is printed), or the parsed module does not fit the runner's memory |
| 7 | unknown import |
| 8 | no memory (DMA buffer) for the module's linear memory |
| 9 | instantiation failed: initial pages or data segments outside the 4 pages, or the instance does not fit the runner's memory |
| 10 | the `start` function failed |
| 11 | missing or invalid manifest |
| 12 | import not granted by the manifest |
| 13 | trap, fuel exhausted or stack exhausted during execution |
| 14 | import with a signature other than the host's |
| 15 | `main` is not `main(i64) -> i32` |
