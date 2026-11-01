# WebAssembly profile v2

What a WebAssembly module may contain and use in AriCodeOS. The profile is
custom and explicit (ADR 0019, ADR 0071). **It is not WASI** nor the
Component Model: a module written for this profile assumes nothing from
POSIX, and a WASI module does not work here. Anything not listed on this
page does not exist for the module.

Version 2 (ADR 0071, 2026-11-01) runs modules with wasmi 2.0 and accepts
every stable WebAssembly feature except SIMD. Version 1 (ADR 0019) was the
integer-only subset of the system's own interpreter.

## Binary format and features

- Version 1 of the binary format (`\0asm` + 1). The whole module is
  validated by wasmparser before it is instantiated; one that does not
  validate is never run.
- Accepted:
  - the MVP, with `f32` and `f64`;
  - mutable globals, sign extension, non-trapping float-to-int, multi-value;
  - bulk memory (all of it, `memory.init`, `data.drop` and `table.*`
    included);
  - reference types (`funcref`, `externref`);
  - multiple memories and tables;
  - tail calls;
  - extended constant expressions;
  - 64-bit memories (`memory64`).
- Refused when the module is validated:
  - SIMD and relaxed SIMD;
  - threads;
  - exceptions;
  - GC and typed function references;
  - proposals not yet standard: custom page sizes, wide arithmetic,
    stack switching.
- Floating point follows the specification. A NaN's payload bits can
  differ from one machine to another, as the specification allows.
- Imports: functions only, and only from the `aricode` module, with the
  signatures of the Capability API below.

## Limits

The runner bounds what a module may hold, so a module can never exhaust
the runner's memory:

| What | Limit |
|---|---|
| module size | 64 KiB |
| each memory | 1 MiB (16 pages of 64 KiB) |
| memories | 4 |
| tables | 4, of at most 16 Ki elements |
| instances | 1 |
| value stack | 64 Ki slots (512 KiB) |
| nested calls | 1024 |
| fuel | 50 M units for `start`, 50 M for `main` |

`memory.grow` and `table.grow` past a limit return -1, as the
specification says for a failed grow; they do not trap.

Fuel counts work, not only instructions:
- about one unit per instruction;
- one per 64 bytes that `memory.{grow,copy,fill,init}` and
  `table.{grow,copy,fill,init}` touch;
- the translation of each function into wasmi's own code, the first time
  it is called (7 units per byte of code).

An import costs what the runner charges for it before it runs (see the
Capability API below): one the module cannot pay for never runs.

## How a module is produced

`rustc` for `wasm32-unknown-unknown`. The repository's samples use `-C
target-cpu=mvp -C target-feature=+sign-ext,+bulk-memory` and a 16 KiB
stack (`.cargo/config.toml`), which keeps them small. The default target
features of `rustc` are within the profile too. Export `main(i64) -> i32`;
the result is the runner's exit code. Export the memory the module passes
pointers into as `memory` (`rustc` does).

To check a module before bringing it into the system:

```bash
wasm-tools validate --features=-simd,-relaxed-simd,-threads,-exceptions,-gc,-function-references demo.wasm
```

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

Pointers and lengths refer to the memory the module exports as `memory`.
A module that imports a function but exports no `memory` traps when it
calls it.

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
| 6 | malformed, unsupported or invalid module (wasmparser's reason is printed) |
| 7 | unknown import: not a function, or not from `aricode` |
| 8 | no memory: the quota did not give the three heap buffers |
| 9 | instantiation failed: a memory, table or data segment over the limits |
| 10 | the `start` function trapped or ran out of fuel |
| 11 | missing or invalid manifest |
| 12 | import not granted by the manifest |
| 13 | trap, fuel exhausted or stack exhausted during `main` |
| 14 | import with a signature other than the host's |
| 15 | `main` is not `main(i64) -> i32` |
