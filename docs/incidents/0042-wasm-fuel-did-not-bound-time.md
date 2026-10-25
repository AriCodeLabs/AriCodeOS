# Incident 0042: WASM fuel did not bound run time

- Opened: 2026-10-25. Status: fixed on `fix/wasm-block-table`, with the
  follow-ups of its audit (section "Audit (2026-10-25)").
- Found by: the failure-pattern audit of 2026-10-25 (audit B, L5: a cost
  meter that counts one unit for work of unbounded size). Not seen on a
  machine.
- Environment: `wasm_runner` running any module (`run wasm_runner ...`).

## Evidence

Host test `entering_a_block_costs_no_scan_of_its_body` on `libs/wasm` at
`e2f6192` (with a test-only counter of the code bytes read to find block
ends): a loop entering a block whose body is 4000 `nop`s, 200 times, read
804 210 instructions to find block ends in 4020 bytes of code.

## Mechanism

Every `block`, `loop` and `if` looked for its `end` (and `else`) by
scanning forward from its start, each time it was entered. Fuel
(`MAX_STEPS`) counts executed instructions: a block entered N times cost N
units of fuel but N times its body in scanning, so a module could run for
far longer than its fuel suggests (a nested block per loop pass makes it
quadratic).

## Fix

First fix (`52d5c64`): the interpreter mapped where every block of a
function ends in one pass, the first time one of its blocks was entered,
and looked the end up after that. Scanning was then bounded by the code
size, whatever the module does; the same test read 4020 instructions.
Its audit (below) replaced the map with a table the validator builds once
per module; the test now checks that running reads no more code bytes
than the instructions it runs (plus a quarter), where a rescan per entry
reads about twice as many.

## Audit (2026-10-25)

An independent audit of the fix (`52d5c64`) found that the lazy map
traded run time for memory, and that the same class of fault (work that
grows with an operand for one unit of fuel) was older than the map in
several instructions. All findings are fixed on this branch; each has a
host test in `libs/wasm` that fails with its fix reverted (checked by
reverting each one alone).

- **M1 (introduced): the map did not fit the runner's heap.** The
  instance-level `BTreeMap` took ~63 bytes per block: a valid 64 KiB
  module holds ~21 000 blocks (~1.3 MiB), and `wasm_runner`'s heap is
  256 KiB, shared with the module bytes and the operand stack. Running out
  of heap panicked the runner instead of returning an error. Fix: the
  validator, which already walks every block, records one 12-byte entry
  per `block` and `if` (start, `else`, `end`, as u32 positions; loops need
  none, a `br` to a loop goes to its start) when it opens, so starts come
  out sorted; the `else` and `end` are filled in when they appear. The
  table lives in `Module`, shared by its instances, and execution finds an
  entry by binary search. It is reserved once, exactly, from a counting
  pass over the code, with `try_reserve`: failure is the new
  `Error::OutOfMemory` (runner: `module too large for the runner's memory`,
  exit 6). Every other allocation in parsing, validation and instantiation
  is fallible too; growing the operand stack, the labels or the locals at
  run time returns `StackOverflow`, call frames are reserved whole and
  imports get their arguments as a slice of the stack. Tests:
  `block_table_is_compact_and_built_by_validation` (a 64 KiB module of
  blocks: one entry per block, capacity equal to length, at most 4 bytes
  of table per module byte) and the QEMU scenario `wasm.many_blocks` (the
  largest module AriFS stores, 32 KiB, ~10 900 blocks: the table, ~128 KiB,
  fits and the module runs). Evidence: the same scenario with `libs/wasm`
  and `apps/wasm-runner` from `52d5c64` prints `wasm_runner: module
  manifest: print clock files`, then `panic in user program`, and the
  runner exits with 255. A 64 KiB module of nothing but blocks, which
  the runner accepts but AriFS cannot store, now needs ~256 KiB and fails
  with `out of memory`.
- **L1 (introduced): `mapped.contains` was a linear search per block
  entry** (8x slower with 6000 functions). Gone with M1: there is no
  per-function map.
- **H1 (pre-existing): `br_table` decoded all its labels on every
  execution**: 60 000 labels took ~310 s per call at `MAX_STEPS`. Fix: the
  validator decodes the labels into a side table (`BrTable` entries and
  `u16` depths, 2 bytes per label) and execution picks one by index. Test:
  `br_table_picks_a_label_without_decoding_the_others`, and
  `br_table_with_multibyte_labels_and_default` for the decoding.
- **M2 (pre-existing): `memory.fill` and `memory.copy` touched up to the
  whole memory for one unit of fuel.** Fix: one more unit per 64 bytes
  (`WORK_BYTES`), and the same for the pages `memory.grow` zeroes. Test:
  `bulk_memory_costs_fuel_by_length`.
- **M3 (pre-existing): a `br` to a loop with many parameters, or out of a
  block with many results, copied that many values for one unit.** Fix:
  branches and function returns cost one more unit per 8 values kept.
  Test: `branch_keeping_many_values_costs_fuel_by_them`.
- **L2 (pre-existing): entering a function zeroed up to 4096 locals for
  one unit**, and 64 frames of 4096 locals reached ~2 MiB, a panic in the
  runner instead of `StackOverflow`. Fix: a call costs one more unit per 8
  parameters and locals, the locals of all active calls are capped at
  `MAX_LOCALS` (16 Ki values) and reserved with `try_reserve`, both
  returning `StackOverflow`. Test:
  `calls_pay_for_their_locals_and_locals_are_bounded`.
- **L3 (pre-existing, validation): validating `br_table` was labels x
  arity** (0.5 s for a 40 KB module). Fix: validation frames borrow their
  types from the module instead of cloning them, labels are compared by
  arity first, and each distinct label depth is type-checked once per
  `br_table` (checking a label leaves its types on top of the stack, so a
  second check of the same depth cannot fail). Function types are capped
  at 1000 parameters and 1000 results (`MAX_ARITY`), which bounds the work
  per block, call and branch everywhere in validation and execution. Test:
  `validating_br_table_checks_each_label_depth_once` (counts type checks)
  and `function_types_are_bounded`.
- **I2: `Module` fields were public** and instances trust them after
  validation. They are private now, with `types()` and `imports()` for the
  runner; `validate` is no longer public (it returns the side tables).
- **I3: a parse error left a partial map.** Moot: the tables are built by
  validation, and a module that fails is not returned at all.

The same pass also charges `call_indirect` for comparing the expected and
actual function types (one unit per 64 value types).

### Second audit (2026-10-25)

An independent audit of the branch (`52d5c64`, `1d5539d`, `74dd6d8`)
found no high finding and no memory-safety problem. It found that the
claim above, every allocation in parsing and validation fallible, was not
true, and more of the same class:

- **M-a (pre-existing): an import cost one unit of fuel whatever it did.**
  `wasm_runner`'s `file_write` replaces up to 256 KiB durably on disk and
  `file_read` loops IPC over as much: `loop { file_write(256 KiB) }` could
  do ~10 M durable AriFS replaces within `MAX_STEPS` (time, and wear of a
  USB stick). Fix: `Host::cost(import, args)` prices an import and the
  interpreter charges it before the import runs (one it cannot pay never
  runs). The runner charges `file_write` 1 000 000 units plus one per 64
  bytes (at most 50 per call), `file_read` 10 000 plus one per 64 bytes,
  `print` 1000 plus one per byte and `uptime_ms` 100. Test:
  `imports_cost_what_the_host_says`.
- **L1: the validator's locals were filled with infallible pushes** (up
  to 4096 + 1000 entries): a module tuned to the runner's heap could still
  panic it. Fix: reserved once with `try_reserve_exact` for the most a
  function can have, filled with `resize`; failure is `OutOfMemory`. Test:
  `running_out_of_heap_is_an_error_at_every_step`, with a test allocator
  that fails after a given number of bytes: parsing, validating,
  instantiating and calling a module are run under every budget up to
  what they need, and each must return a value or `OutOfMemory` /
  `StackOverflow`. Without the fix the test process aborts (`memory
  allocation of 8 bytes failed`).
- **L2: the runner's own `imports` list grew with infallible pushes.**
  Reserved with `try_reserve_exact`; failure exits with 6.
- **I1: dead `br_table`s make the densest side tables**, 14 bytes per 3
  bytes of code (~4.7 per module byte; a 32 KiB module needs ~150 KiB,
  which still fits). Stated in the profile; test
  `dead_br_tables_keep_side_tables_bounded`.
- **I3 (pre-existing): the table section accepted any number of tables**,
  each resetting a table of up to 4096 entries (and so did repeated table
  sections). One table and one memory at most, as in the MVP:
  `Unsupported("more than one table")` / `("more than one memory")`. Test:
  `one_table_and_one_memory_at_most`.
- **I4 (pre-existing): the host passes `memory` on every call**, and one
  shorter than the instance's pages made loads, stores, `memory.fill` and
  `memory.copy` slice out of bounds (a panic). Every access is now checked
  against the shorter of the two. Test:
  `a_memory_shorter_than_the_instance_traps_instead_of_panicking`.
- **I5: the profile's table of runner exit codes missed 1, 2, 4, 8, 9, 10,
  14 and 15**, and put an instantiation `OutOfMemory` (exit 9) under 6.
  Completed.
- **I8: `start` has its own fuel**: one run of the runner can spend 2 x
  50 M. Stated in the profile.

Each fix's test fails with that fix reverted (checked one by one).

### Contract changes (profile v1, ADR 0019)

Recorded in `docs/wasm-profile.md`:

- Fuel counts work, not instructions: `MAX_STEPS` (50 M) is units of
  work, one per instruction plus one per `WORK_BYTES` (64) bytes an
  instruction moves, zeroes or compares beyond that. A module that does
  bulk work runs out of fuel sooner than before.
- New validator limit: at most 1000 parameters and 1000 results per
  function type (`Unsupported("too many params or results")`), the
  WebAssembly JavaScript API limits, so no module a browser accepts is
  rejected for it.
- New run-time limit: 16 Ki locals (parameters included) across all active
  calls (`StackOverflow`).
- New error `Error::OutOfMemory`; the module and its instance must fit the
  host's heap.
- `Host` has a new required method, `cost`: hosts price their imports in
  fuel, and the runner's prices are part of the profile.
- At most one table and one memory (MVP), now enforced.
- `Module`'s fields are private and `validate` is internal (library API;
  only `wasm_runner` uses the library).
