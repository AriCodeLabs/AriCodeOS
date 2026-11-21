# Audit: GPU stage A (ADR 0083), 2026-11-07

Independent, read-only review of `feature/gpu-stage-a` against `develop`: `libs/intel-gt`,
`kernel/src/gpu.rs` and its glue, ABI 10, the broker's `gpu` capability. The property under
attack: a process with the `GPU` object reaches no physical memory but the DMA buffers it
mapped, changes no GTT, PPGTT, ring or engine register, corrupts no kernel state, and never
leaves the GPU able to reach freed memory; the display keeps working; nothing panics or holds
interrupts off for long.

**Result:** no byte sequence was found that passes the batch check and does anything outside
its list. One High, four Medium and eight Low findings; all but L8 fixed in `45f66e0`, and
the fixes ran on the laptop (dev build 43: `gpu_probe` exit 0) and in the smoke and area
suites.

| # | Finding | Fix |
|---|---|---|
| H1 | Once a reset failed (`dead`), `gpu_unmap` and the end of the client let buffers go that the stuck engine might still write | `gpu_unmap` returns `Hung` while dead; a dead engine's client buffers go to `hw::quarantine` |
| M1 | The firmware's run was checked against the screen's size, not where it ends; the ring and status page sat inside the CPU's aperture, which the framebuffer window could reach on another machine | The run must cover the screen to its end; the kernel's GPU pages sit past the aperture (below 512 MiB, as the ring needs) |
| M2 | A bring-up failing after the tables were written freed pages the hardware pointed at and claimed the firmware's state | Stop the ring and the PPGTT, quarantine the pages, report `Partial` |
| M3 | Directory entries changed at runtime without reloading `PP_DIR_BASE`; a failed table allocation left a region silently unmapped | All 512 tables (2 MiB) and every directory entry are written at boot, never changed |
| M4 | Unbounded mappings could exhaust the kernel heap | At most 256 mappings (`Limit`) |
| L1 | Undefined blits accepted: any raster operation, overlap with two bases, unaligned addresses | Only 0xF0 for fills and 0xCC for copies, PRM 1.7.1's overlap rule, 4-byte alignment |
| L2 | Idle was the ring's head alone; the PPGTT's registers were never re-checked | Idle also needs the last number stored; `submit` checks the PPGTT is in place, else resets |
| L3 | An ended client was released only at another program's call | A `gpu` kernel thread releases it within 100 ms |
| L4 | `gpu.hang_ms` unchecked (0 resets constantly; a huge value overflows) | Clamped to 100..=60000 |
| L5 | ABI docs differed from the code | Documented: bit 8 on the self jump, every error each call returns, the ring's caching |
| L6 | The status page was read as plain memory | Volatile reads |
| L7 | `report` took the engine, so any `SYSTEM` holder could make it wait or release a client | It never waits nor releases; while a call has the engine it reports what needs none |
| L8 | `gpu` is granted to any application (`APP_ALLOWED`) | Kept, recorded in ADR 0083: it adds no authority over memory, one program at a time; whether to narrow it to signed packages is the owner's call |
