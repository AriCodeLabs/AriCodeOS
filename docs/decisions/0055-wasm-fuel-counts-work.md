# ADR 0055: WASM fuel counts work, and the profile's limits that bound it

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25: B-L5; incident
  0042 and its two audits). Amends ADR 0019.

## Context

ADR 0019's interpreter bounds a run with fuel, `MAX_STEPS` = 50 M, one unit
per executed instruction. Incident 0042 and the audits of its fix showed
that one instruction could do unbounded work: rescanning a block body on
every entry, decoding a whole `br_table`, `memory.fill`/`memory.copy` over
the whole memory, branches keeping thousands of values, calls zeroing
thousands of locals, and host imports such as `file_write` (256 KiB, a
durable AriFS replace) at one unit each. Fuel then no longer bounded run
time, and a small module could also exhaust the runner's 256 KiB heap and
panic it.

## Decision

1. **Fuel counts work.** One unit per instruction, plus one per 64 bytes
   (`WORK_BYTES`) an instruction moves, zeroes or compares beyond that
   (bulk memory, kept values, locals, `call_indirect` types). Blocks and
   `br_table` targets come from tables the validator builds once per
   module, so entering a block or branching costs a lookup.
2. **Imports pay too.** `Host::cost(import, args)` gives each import's
   price, charged before it runs; an import the module cannot pay for
   never runs. The runner's prices are part of the profile
   (`docs/wasm-profile.md`), e.g. `file_write` 1 000 000 units plus its
   bytes, at most 50 durable replaces per call.
3. **New profile limits** (`docs/wasm-profile.md`): at most 1000
   parameters and 1000 results per function type (the WebAssembly
   JavaScript API's limits, so nothing a browser accepts is refused); at
   most 16 Ki locals across active calls (`StackOverflow`); at most one
   table and one memory (MVP).
4. **Memory exhaustion is an error.** Every allocation in parsing,
   validation and instantiation is fallible (`Error::OutOfMemory`), and
   run-time growth returns `StackOverflow`, so a module never panics the
   runner by its size.
5. `Module`'s fields are private and `validate` is internal: an instance
   relies on what validation established.

## Consequences

- Bulk-heavy modules run out of fuel sooner than before; `start` has its
  own 50 M, so a run spends at most 100 M.
- `print` costs one unit per byte: a run can still write about 50 MB to a
  slow console (accepted).
- Hosts other than the runner must implement `Host::cost`.
