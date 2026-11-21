# Audit: compute on the HD 4000 (ADR 0086, stage B), 2026-11-09

Independent, read-only review of `47739f0..89fb03a` (`feature/gpu-compute`: B3 to B6). The
properties under attack: nothing a client puts in a `gpu::Dispatch` or a blitter batch makes
a command the kernel writes reach registers, the global GTT or MMIO; no page either engine
may still touch is freed; one client's EU registers and stray writes never reach the next;
nothing reachable from user input panics the kernel.

**Result:** no High finding; the main claim holds (every field of the emitted commands
checked against its bit range, the gateway's MMIO, the walker's indirect and predicate bits
always 0, no post-sync write or global-GTT bit in the emitted `PIPE_CONTROL`s, the rings
quadword-aligned and parsed at the check's boundaries). Two Medium and three Low findings,
all fixed in `b8c25df`, verified by the host tests, `gpu.absent` and on the laptop (dev build
64: `gpu_probe` whole, the two-client residue test, the clear on 128 of 128 slots in 85 us).

| # | Finding | Fix |
|---|---|---|
| M1 | A clear that reached fewer slots than the GPU has counted as success; nothing forces a group's 64 threads to be resident together | A clear missing a slot runs again, up to 3 times, then the render engine is left off |
| M2 | The clear did not check that the render engine still ran in the PPGTT; without it its state addresses would be global-GTT ones, in the screen's pages | `ensure_ppgtt` before the clear, as `dispatch` and `submit` do |
| L1 | `gpu_wait` remembered only the last reset's lost numbers | The last 8 resets' ranges |
| L2 | A mapping that failed halfway left PTEs another engine may have cached, and freed the object | Quarantined when an engine is not idle |
| L3 | The scratch page was zeroed only after a successful clear | Zeroed first |
| Info | The clear left the accumulators, flags and address register, as Linux's does | Zeroed too |
| Info | The heartbeat ran only inside `gpu_wait` | The `gpu` thread watches both engines every 100 ms |
| Info | `claim` before the descriptor was checked; comments left from the copy pages; the scratch size code's meaning | Reordered; fixed; both readings recorded (PRM and Mesa), nothing spills yet |

Open, not a defect found: whether the L3 cache can hand a freed and reused frame's stale
lines to the next client is unmeasured. The clear writes Linux's 64 KiB through the render
cache and every request flushes and invalidates what `PIPE_CONTROL` can; a two-client test
on a reused frame is the next measurement (ADR 0086, B8).
