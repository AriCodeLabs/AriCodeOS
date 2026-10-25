# Incident 0042: WASM fuel did not bound run time

- Opened: 2026-10-25. Status: fixed on `fix/wasm-block-table`.
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

The interpreter maps where every block of a function ends in one pass,
the first time one of its blocks is entered, and looks the end up after
that (`Instance::block_end`). Scanning is now bounded by the code size,
whatever the module does; the same test reads 4020 instructions.
