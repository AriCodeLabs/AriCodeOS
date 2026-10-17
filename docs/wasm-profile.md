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
  segments with an `i32.const` offset.
- At most one memory; its initial pages must fit in the one the host
  provides (4 pages, 256 KiB). `memory.grow` grows within that buffer and
  returns -1 if it does not fit.
- Imports: functions only, and only from the `aricode` module.
- Limits: 4096 locals per function, 1024 nested blocks, modules of up to
  64 KiB (loaded by the runner), 64 nested calls, 16 Ki values on the stack
  and 50 M steps per exported call.

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
(ADR 0012): a dead service yields -1, never an infinite wait. The
interpreter's step counter does not count time spent inside an import.

## Runner errors

| Code | Reason |
|---|---|
| 3 | no `demo.wasm` |
| 5 | module larger than 64 KiB |
| 6 | malformed, unsupported or invalid module (the reason is printed) |
| 7 | unknown import |
| 11 | missing or invalid manifest |
| 12 | import not granted by the manifest |
| 13 | trap, steps exhausted or stack exhausted during execution |
