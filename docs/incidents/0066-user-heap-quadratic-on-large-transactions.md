# Incident 0066: the program heap turns large allocations quadratic

- Date: 2026-11-20
- Environment: the reference laptop (ASUS, Core i3-3217U, 3.7 GiB), dev build 68,
  running the AriDB app v1 (`run aridb N`, branch `feature/database`, commit `b97dc7d`);
  reproduced on the build host (Ryzen 7 5800X, Linux 6.12) with a scratch probe.
- Work branch: `fix/user-heap-quadratic`, from `develop` at `f84e72f`.

## What was seen

AriDB's bulk load (N rows inserted in one transaction) on AriCodeOS, in memory
(no device involved), per row:

| Rows | 5 000 | 10 000 | 25 000 | 50 000 |
|---|---|---|---|---|
| µs per row | 27 | 56 | 202 | 444 |

The cost per row grows with N: the whole load is quadratic. The same code on the
host costs 2.36 µs per row at 50 000 rows. Point reads on the laptop cost 4-5x the
host's, which is the CPU difference; the bulk load costs 188x.

## Evidence so far

- A write transaction of the engine keeps every changed page alive until commit, so
  the heap holds tens of MiB of live blocks (peak measured on the laptop: 48 400 KiB).
- The user runtime's heap (`sdk/runtime`, ADR 0084) is `aricode-heap`
  (`libs/heap`), the kernel's first-fit allocator over a linked free list.
- Scratch probe (`~/aricode-scratch/db-scratch/heap-probe`, outside the repo) runs
  the same AriDB workload on the host twice, changing only the global allocator:
  2.4 µs per row with the system allocator, 190.9 µs with `aricode-heap`, at 50 000
  rows. Logs: `probe-system`, `probe-list`, `probe.log` there.

Hypothesis (to be confirmed by the study, not assumed): every allocation walks the
free list from its head, and the list grows with the number of live blocks and
holes, so an allocation costs O(blocks).

## Timeline

- 2026-11-20: seen on the laptop by the AriDB app benchmark; probe on the host;
  incident opened; study launched (three independent angles).
- 2026-11-20: the three reports attached; mechanism confirmed (Confirmation); fix on
  `fix/user-heap-quadratic` (Fix), measured on the host (Measured after). Audit and the
  laptop's confirmation pending.

## Study

Three independent reports, attached as they arrived. Their scratch work is outside the
repository, under `~/aricode-scratch/heap-scratch/` (`a1/` for angle 1; `veterans/`,
`bench/`, `traces/` and `logs/` for angles 2 and 3).

### Study, angle 1: how our code causes it

Author: study agent, angle 1. I only read the repository. Instrumented copies and logs are under `/home/serverbig/aricode-scratch/heap-scratch/a1/`:
- `heap-instr/`: a copy of `libs/heap` with counters.
- `probe/`: the instrumented probe.
- `probe-orig/`: the unmodified probe, built against `wt-heap/libs/heap`.
- `micro/`: a minimal reproduction.
- `logs/`: all run output.

The AriDB sources are a `git archive` of `wt-db` at `4307df1`, so they cannot change under the runs. `libs/heap` there is identical to `wt-heap/libs/heap`, which is develop at `f84e72f`. Every benchmark ran pinned with `taskset -c 10,11`, three times, and the tables give medians. Load average during the runs was 2-4 because other agents were working.

#### Result

**The cost grows with N in `Heap::deallocate`, not in `allocate`.** `deallocate` calls `insert_free` (`libs/heap/src/lib.rs:90-100`, called from `:199-203`). That function walks the address-sorted free list from its head to find where the freed block goes. During the insert loop the list stays short, at 1-15 nodes.

At `COMMIT` the write transaction is dropped. Its `dirty: BTreeMap<u64, Node>` (`db/crates/aridb-core/src/txn.rs:84`, dropped at `txn.rs:913`) frees about 3 blocks per row: the `Vec<u8>` of each `LeafEntry`. They are freed in page-number order, so their addresses are scattered, and 45% of frees land below the previous one. Most freed blocks still have live neighbours, so they do not coalesce. The list grows to about 0.46·N holes, and each free walks a large share of it. The commit is therefore O(N²).

#### Measurements

**Original allocator against the system allocator** (`probe-orig-*`, bulk insert, µs per row, median of 3):

| Rows | 5 000 | 10 000 | 25 000 | 50 000 |
|---|---|---|---|---|
| System allocator | 1.93 | 2.06 | 2.34 | 2.41 |
| aricode-heap | 7.40 | 14.30 | 57.23 | 192.55 |

**Instrumented counters**, per bulk run including the commit:

| Rows | 5k | 10k | 25k | 50k |
|---|---|---|---|---|
| Nodes visited per alloc (first-fit) | 2.2 | 2.2 | 2.2 | 2.2 |
| Nodes walked per split re-insert | 2.9 | 2.9 | 2.9 | 2.9 |
| **Nodes walked per free** | **84.7** | **171.8** | **421.9** | **839.2** |
| Nodes walked per row by frees | 2 342 | 4 741 | 11 634 | 23 131 |
| Nodes walked per row by allocs and splits | 93 | 93 | 94 | 96 |
| Longest free list | 2 197 | 4 528 | 11 544 | 22 986 |
| Free blocks after commit | 19 | 22 | 37 | 54 |

- **Free-walk nodes per row are 0.46·N, exactly linear**, so the total is quadratic.
- First-fit finds its block in the first 8 nodes 99.6% of the time, at every N.
- **The work happens at the commit.** A time series sampled every 32 768 allocations shows the list at 1-15 nodes and about 3 nodes per free during the whole insert loop. Live blocks climb to about 155 000 at 50k rows (about 27 MiB in use).
  - A series sampled every 4 096 frees at 25k rows shows the commit window: the list grows 1 746 → 9 961 → 11 369 → 939 nodes, at 3 100-3 260 nodes per free.
  - A backtrace taken on the first free that walked more than 5 000 nodes points to `drop_glue<LeafEntry>` ← `BTreeMap<u64, Node>` ← `WriteTxn` drop ← `commit` (`txn.rs:913`) ← `session.rs:247`.
- **Causal check** (`LEAK=long`): I skipped only the frees made while the list was longer than 1 000 nodes, which are the commit frees. The cost per row then stays flat (instrumented build, median of 3):

  | Rows | Unchanged | Commit frees skipped | All frees skipped |
  |---|---|---|---|
  | 25 000 | 108.9 | 8.2 | 8.2 |
  | 50 000 | 235.2 | 8.3 | 8.2 |

  The remaining ~8 µs is constant in N. Part of it is the instrumentation, part is the global `Mutex` taken around every allocation and free.
- **Minimal reproduction** (`micro/`): allocate K 48-byte blocks interleaved with K blocks that stay live, then free the first set.

  | Free order | K | Nodes walked per free | Time per free |
  |---|---|---|---|
  | Random | 1 000 | 246 | 0.53 µs |
  | Random | 64 000 | 16 029 | 59.9 µs |
  | Ascending | 64 000 | 32 000 | 46.5 µs |
  | Descending | 64 000 | 1.0 | 5.7 ns |

  Even freeing in allocation order (FIFO) is quadratic. Only descending order is cheap.
- **Cost per node rises with list size.** On the host the excess over the system allocator is about 2.2 ns per node at 5k rows and 8.2 ns at 50k: the list stops fitting in cache. The laptop's i3-3217U has a 3 MiB L3, so it pays DRAM latency much sooner. That fits 27 µs per row there already at 5k rows, against 7.4 µs on the host.

#### Ranked hypotheses

1. **CONFIRMED, the cause: `deallocate` is O(free-list length).**
   - Code: `insert_free`, `lib.rs:92-100`, walks from `self.head` until it finds the first node above the freed address.
   - Mechanism: a mass free in non-address order with live neighbours grows the list to O(N), so each free costs O(N).
   - Evidence: the free-walk counters above, the commit-window series, the backtrace, the `LEAK=long` check and the microbenchmark.
2. **CONFIRMED, minor and constant: the split remainder is re-inserted by walking from the head again.**
   - Code: `lib.rs:177-180`.
   - The remainder belongs exactly where the unlinked block was, and `prev` is already known, so the second walk repeats the first-fit walk: about 2.9 nodes per split, 34 per row. It is not the cause, but the work is redundant.
3. **REFUTED for this workload, but a latent defect: the first-fit walk in `allocate`** (`lib.rs:147-192`).
   - Constant at 2.2 nodes per allocation, because the AriDB loop frees almost in LIFO order and the head holes fit small requests.
   - It is still O(list) in the worst case. A long-lived program that keeps O(n) small holes at low addresses makes every larger request walk them all.
4. **REFUTED: heap growth by memory objects creates many regions or boundaries that never merge.**
   - `map_memory` takes its address from `mmio_ranges.alloc(span)` (`kernel/src/user/process.rs:1839`). That is a bump allocator (`libs/vranges/src/lib.rs:40-57`), so consecutive heap regions are adjacent and `insert_free` merges them.
   - I emulated the runtime's policy from `sdk/runtime/src/lib.rs:54-58,132-160`: a 256 KiB arena plus regions of max(1 MiB, request + alignment + 4 KiB). At 50k rows it gives 34 regions and **48 400 KiB in total, exactly the laptop's measured peak**.
   - The free list is unchanged: 22 984 nodes at most against 22 986 with one region, and 839.2 nodes per free in both.
   - With a 4 KiB gap forced between regions: 23 004 nodes at most, 840.3 per free.
   - Each growth walks about 10-12 nodes. The timings are the same in all three layouts (194 / 195 / 194 µs per row at 50k).
   - The static arena in `.bss` stays one boundary that never merges, at the head of the list. Its cost is negligible.
5. **OPEN, plausible but bounded: the kernel heap has the same O(list) free and first-fit, with interrupts off.**
   - The kernel uses `IrqLock` (`kernel/src/memory/heap.rs:115,136`), so every walk runs with interrupts disabled.
   - Its live set is small and bounded: `HANDLE_LIMIT` 256 (`object/mod.rs:158`); `MAX_FOREIGN` 512 mappings per process (`process.rs:140`); IPC queues of 16 messages of at most 256 bytes each (`syscalls.rs:203`, `ipc.rs`); FPU areas per thread (`fpu.rs:177`); `sysinfo` replies made with `to_vec`. Kernel stacks and page tables come from the PMM, not the heap.
   - The same pattern of many frees in non-address order with live neighbours can happen when a process exits and its handles, mappings and threads are dropped. k should be in the hundreds, which means microseconds, not seconds, but that is untested.
   - **No measurement is possible today.** Neither the kernel nor the runtime reports free-list length or walk counts: `HeapStats` has only `total`, `free` and `allocations`, and `free_blocks()` is used only in unit tests.
   - Instrumentation to confirm: nodes walked per operation, the longest list, and the longest time with interrupts off in `alloc`/`dealloc`, exported through `sysinfo`/`diag`. Then run the IPC stress test, spawn/exit churn, the WebAssembly runner and the desktop.
6. **OPEN, constant factor: a residual of about 3x over the system allocator, even with no long walks.**
   - The causes are a lock taken on every operation, and `realloc` left at the `GlobalAlloc` default (allocate, copy, free; `sdk/runtime/src/lib.rs:124-169` defines no `realloc`).
   - This is not part of the growth with N and I did not measure it separately. The comparison with other allocators should measure it.

#### What this means for the fix

These are inputs for the comparison with other allocators, not a fix.
- The free path must find a block's neighbours in O(1), with boundary tags or footers as dlmalloc and talc do, or in O(log n) with an address-ordered tree. Walking a list is what fails here.
- Allocation should not depend on hole order. Size-segregated bins avoid that.
- A deterministic test that fails before the fix and passes after: the `micro/` pattern of K interleaved kept and freed blocks, freed in scrambled and in ascending order. Assert a bound on nodes walked per free, which does not depend on timing; the counter would have to become a real diagnostic that `diag` shows, so it is not dead code.
- The diagnostic gap breaks failure-protocol principle 1. Whatever replaces the list should export its fragmentation and its worst-case operation cost.

### Study, angle 2: veterans and their failures (allocator designs, measured against ours)

Agent: angle 2 (veterans). Worktree `wt-heap` at `95d4964`. I read the code and changed nothing in the repository. All scratch work is under `/home/serverbig/aricode-scratch/heap-scratch/`.

#### 1. Our allocator as it is

`libs/heap/src/lib.rs` keeps one free list sorted by address, uses first-fit, and puts a 16-byte header on each allocation.
- **`allocate_rounded` (l.142-195)** walks the list from `head` until a block fits. Cost is O(free blocks).
- **`deallocate` → `insert_free` (l.88-122)** also walks from `head` to find the address position, because coalescing needs the neighbours. Cost is O(free blocks). **Both paths are linear, not only allocation.**
- **Kernel exposure:** the kernel (`kernel/src/memory/heap.rs`) runs both paths inside an `IrqLock`. The time interrupts stay off is therefore proportional to the length of the free list.
- **User exposure:** the user runtime (`sdk/runtime/src/lib.rs` l.43-170) runs both paths under a spin lock. Its `GlobalAlloc` does not override `realloc`, so every `Vec`/`String` growth costs alloc + copy + free, which is two list walks.

#### 2. Measurement: ours against the veterans on the same workload

**Bench:** `/home/serverbig/aricode-scratch/heap-scratch/veterans/` (`src/main.rs`).
- Same workload as `heap-probe`: AriDB bulk load, `MemStorage`, `sql_config`.
- The AriDB crates are snapshotted at `b97dc7d` into `heap-scratch/db-snap`, because `wt-db` was being edited mid-run (its manifest did not parse).
- Every allocator gets the same 1 GiB static arena behind the same `Mutex`. Only the allocator differs.
- Runs pinned with `taskset -c 10,11`. Three repeats; medians shown.
- Logs: `matrix.log`, `adv.log`, `adv2.log`, `adv2-talc5-sweep.log`, `medians.txt`. Summariser: `heap-scratch/tools/medians.py`.

**Bulk load, µs per row (median of 3), and peak resident memory at 50k rows**

| Allocator | 10k | 25k | 50k | Peak RSS 50k |
|---|---|---|---|---|
| system (glibc) | 2.05 | 2.31 | 2.39 | 43.3 MiB |
| **aricode-heap (ours)** | **14.25** | **55.39** | **192.76** | 50.8 MiB |
| linked_list_allocator 0.10.6 | 12.87 | 52.73 | 176.30 | 47.6 MiB |
| buddy_system_allocator 0.11 | 18.70 | 74.01 | 179.28 | 70.3 MiB |
| talc 4.4.3 | 2.13 | 2.32 | 2.50 | 48.6 MiB |
| talc 5.1.1 | 2.21 | 2.49 | 2.54 | 58.7 MiB |
| rlsf 0.2.3 (TLSF, SLLEN 32) | 1.92 | 2.09 | 2.24 | 60.4 MiB |
| rlsf (SLLEN 64) | 1.97 | 2.24 | 2.30 | 60.0 MiB |
| dlmalloc-rs 0.2.14 | **1.80** | **2.00** | **2.08** | **49.0 MiB** |

Ours reproduces the probe (192.8 vs 190.9 µs). Every segregated-fit design stays flat with N; every single-list design (ours, `linked_list_allocator`) and the buddy crate grow with N.

**Synthetic latency test**
- Setup: L live blocks of 16-4096 bytes, then 200k rounds of "free one at random, allocate one at random".
- Units: rdtsc cycles, median of 3 runs.
- Columns: allocation p50/p99, then free p50/p99. The max column is dominated by first-touch page faults and preemption, so p99 is the meaningful latency figure.

| Allocator | L=10k alloc | L=10k free | L=100k alloc | L=100k free |
|---|---|---|---|---|
| ours | 35 947 / 93 366 | 20 368 / 47 385 | **673 778 / 2 938 996** | **450 490 / 1 511 260** |
| linked_list_allocator | 25 840 / 65 018 | 24 586 / 60 154 | 441 940 / 1 636 774 | 519 878 / 1 621 916 |
| buddy | 38 / 190 | 266 / 988 | 38 / 190 | 912 / 2 849 |
| talc 4 | 76 / 646 | 114 / 380 | 228 / 7 942 | 228 / 1 368 |
| talc 5 | 113 / 266 | 113 / 341 | 114 / 798 | 152 / 1 216 |
| rlsf | 75 / 227 | 114 / 228 | 76 / 532 | 151 / 873 |
| dlmalloc | 152 / 304 | 152 / 380 | 189 / 532 | 190 / 1 254 |
| system | 152 / 494 | 152 / 379 | 190 / 1 063 | 190 / 1 025 |

At 100k live blocks, one allocation of ours costs about 674k cycles, roughly 177 µs on this 3.8 GHz host. In the kernel that would all be time with interrupts off.

**Adversary test (direct evidence for the mechanism)**
- Setup: N free 520-byte holes, kept apart by live 16-byte separators, then 1000 requests that none of the holes can serve.
- Request 630 bytes:
  - Ours: 8 588 → 101 270 → 1 031 358 cycles (p50) at 1k → 10k → 100k holes. That is linear, about 10 cycles per free-list node.
  - `linked_list_allocator`: the same shape.
  - rlsf, dlmalloc, talc 5, buddy: 38 cycles at every N.
- Request 600 bytes (same bin as the holes):
  - talc 4 also degrades: 51 642 → 510 150 cycles. This is its documented O(n) in-bin search.
  - talc 5 stays at 38 cycles across a sweep of sizes from 530 to 760. Its current search takes the head of the next bin up (`size_to_bin_ceil`, `base/mod.rs` l.126-173). It falls back to a full in-bin search only for the last (huge) bin, or when nothing larger is free, which for us is just before the heap grows.

This confirms the incident's hypothesis on the host in a controlled form: our allocation cost is proportional to the number of free blocks walked. The same holds for free.

#### 3. Veterans: design, failures, fit

| Design / crate | alloc / free | Worst-case latency | Fragmentation, overhead | Growth by regions | Concurrency | Maturity, unsafe, licence | Known failures |
|---|---|---|---|---|---|---|---|
| **First-fit address-ordered list** (ours; `linked_list_allocator`, used by Theseus's early heap and historically Redox's kernel) | O(n) / O(n) | Unbounded (n = free blocks) | Good (first-fit, eager coalescing); ours: 16 B header | Ours: any region; `linked_list_allocator`: only contiguous `extend` | One lock | lla: 7.4M downloads, ~1.1k lines, Apache/MIT | lla: CVE-2022-36086 (out-of-bounds write in `init`/`extend`, fixed 0.10.2); open issue #70 (dealloc hang in Theseus after 0.9→0.10); #66 leak of small back paddings (fixed 0.10.4). Redox pinned an older version at one point; a Redox merge request proposed a slab front end for blocks ≤ 4 KiB because of the list's cost. Phil Opp's tutorial adds a fixed-size-block front end for the same reason. |
| **Buddy** (`buddy_system_allocator` 0.11/0.13) | O(log) / **O(n) per order** (dealloc scans `free_list[class]` for the buddy, l.199-208 in 0.13) | Unbounded on free | Poor: power-of-two rounding; 63% heap efficiency in talc's benchmark; our peak 70.3 vs 49 MiB | `add_to_heap` any region | One lock | MIT, ~700 lines | The linear buddy search measured above (179 µs/row). Fine for page frames, not for a byte heap. |
| **TLSF** (`rlsf`; Masmano et al., ECRTS 2004) | **O(1) / O(1)** (two-level bitmaps; "good fit" rounds the request up so the head of any non-empty list fits) | **Bounded**, with no search loop at all | Internal fragmentation up to 1/SLLEN of the block (measured +23% peak in AriDB vs dlmalloc; SLLEN is a parameter); 32 B granule on 64-bit | `insert_free_block_ptr` / `append_free_block_ptr` (merges with a contiguous pool); `FlexTlsf` + `FlexSource` for automatic growth | Single lock (README: FIFO lock → O(threads) worst case) | MIT/Apache-2.0; ~1.2k-line core, ~96 `unsafe` tokens; quickcheck tests; 2.5M downloads; 0.2.3 (2026-07) | 0.1.1 fixed a `reallocate` copy-length bug; 0.2.2 fixed Miri-detected invalid references. No CVE found. |
| **Bucketed good fit with bounded search** (Zephyr `sys_heap`; Zircon `cmpctmalloc`) | O(1) / O(1) | Bounded: Zephyr's "might fit" scan is capped by `CONFIG_SYS_HEAP_ALLOC_LOOPS` (default 3), then takes a guaranteed fit. Zephyr documents 1-200 cycles. | cmpctmalloc: 8 buckets per power of two, frees round down, eager boundary-tag coalescing | cmpctmalloc grows the heap by at least 12%, caches one free OS block, global mutex with preemption disabled | One lock | Zircon kernel and Zephyr kernel: proven in production | cmpctmalloc dropped its large-allocation path (fxbug.dev/31229); the kernel heap caps allocations at 1 MiB, and bigger ones go to the VM. This is the same family as TLSF, so it confirms the class for kernels. |
| **talc** (dlmalloc-style boundary tags + bins + bitmap) | v4: O(n) in-bin worst case (reproduced above); v5: head of next bin, O(n) only on the last bin or near OOM | v4 unbounded; v5 near-bounded | Best footprint in v4 (48.6 MiB); v5 now pays the same rounding as TLSF (58.7 MiB) | Many heaps; v5 `Source` trait (fits our memory objects) | `TalcLock` over `lock_api`; the README calls concurrency its "main deficit" | MIT; v5 ~6.3k lines / ~241 `unsafe`, a 2025-26 rewrite. Hermit's kernel uses talc. | v5.0.4 fixed chunk tagging for chunks ≥ 2^56 and an infinite-recursion grow bug (wasm); issue #17 (out-of-bounds on wasm, traced to zstd). The v5 API is young. |
| **dlmalloc** (`dlmalloc-rs`, a Rust port of Doug Lea's 2.8.x) | Small bins exact-size O(1); tree bins are a bitwise trie, O(bits) best fit; designated victim, top chunk | Bounded by trie depth, except segment walks on growth/trim (few segments with ≥ 1 MiB growth) | Best fit; lowest measured footprint among candidates (49.0 MiB); ~8-16 B overhead | `Allocator` trait (`alloc`/`free`/`free_part`/`remap`) maps onto `mem_create` + `mmio_map`, and can return memory | One lock (std wraps it) | MIT/Apache-2.0; ~3.2k lines, ~131 `unsafe`; 20M downloads; Rust std's allocator for wasm32-unknown-unknown (and SGX, Xous); last release 2026-11-10 | Issue #41: panics once Rust 1.78 enabled debug assertions in the allocator; #32/#37: the "bugs" were callers passing wrong layouts; rust-lang#144199: a suspected wasm dealloc crash that was not reproduced. No CVE found. It is a translation of C, so it reads like C. |
| **Slab / size-class front ends** (Linux SLUB; Theseus `slabmalloc` per-core heaps; Redox's proposed slab merge request) | O(1) per class | Bounded | Per-class slabs; internal waste per class | Large requests go to a page or back-end allocator | Per-CPU slabs (SLUB, Theseus) | Production kernels | SLUB: freelist hardening (`SLAB_FREELIST_HARDENED`) and cross-cache use-after-free exploitation pressure. SLOB was removed in 6.4 and SLAB in 6.8, leaving one allocator. |
| **mimalloc / snmalloc** (designs only; C, so not built) | O(1): free-list sharding per page (free, local-free and thread-free lists; remote frees by CAS) / message passing of remote frees | Bounded fast path | Size-class pages and segments; more RSS | mmap-backed segments | Per-thread heaps: the real answer for many threads | MIT; production (Microsoft) | Higher RSS in some workloads; needs thread-local storage and atomics. |
| **Tock** | No kernel heap at all; per-process "grants" carved from the process's memory | No allocator in the kernel | — | — | — | — | It is a design choice, not an allocator. It shows the alternative of keeping kernel allocations out of shared memory. |

#### 4. Recommendation

The licences of all candidates are allowed (MIT, Apache-2.0, MIT/Apache-2.0). All are pure Rust, so no C is built.

**Kernel heap: adopt TLSF (`rlsf`), wrapped behind the existing `aricode-heap` API.**
- **Reasons:**
  - The kernel allocates with interrupts off (`IrqLock`), so the criterion is a worst-case bound, not average speed.
  - TLSF is the only measured design with no search loop. Measured p99: 532 cycles at 100k live blocks, and 38-114 cycles in the adversary at any N.
  - The kernel heap grows contiguously from `HEAP_BASE`, which matches `append_free_block_ptr`.
  - It is the smallest bounded candidate (~1.2k-line core) and has a quickcheck-tested history.
  - Zircon's `cmpctmalloc` and Zephyr's `sys_heap` are the same class of design in production kernels.
- **Cost:** about 20% more internal fragmentation than best fit, tunable through SLLEN as a parameter, which fits the no-hardcoding rule.
- **The wrapper keeps `HeapStats`:** rlsf exposes no statistics, and `allocation_usable_size` is unstable, so the wrapper counts the bytes and blocks it hands out.
- **Alternative if the owner prefers no new dependency:** reimplement TLSF in `libs/heap`, following Masmano 2004 and `rlsf` as the reference, with a differential test against `rlsf` on the host. Not a blind copy, and ~600-800 lines of our own `unsafe`.

**User heap: two defensible options; this is an owner choice.**
- **(a) dlmalloc-rs (measured winner for programs).**
  - Fastest on AriDB at every N: 2.08 µs/row at 50k, faster than glibc's 2.39.
  - Lowest footprint of the candidates (49.0 MiB).
  - The most-used Rust no_std allocator (it is std's own on wasm).
  - Its `Allocator` trait maps directly onto memory objects, and it can later return memory to the quota (`free`/`free_part`).
  - User programs do not need hard real-time bounds, and its worst case is still bounded by trie depth.
  - Caveats:
    - It is a larger port of C (~131 `unsafe` tokens).
    - On our user target (`x86_64-unknown-linux-gnu`, ADR 0065) its `unix.rs` default `System` and the `libc` dependency get compiled but stay unused. Use `new_with_allocator` only, with the `global` feature off.
- **(b) The same TLSF as the kernel.**
  - One allocator and one audit for the whole system.
  - 2.24 µs/row: within 8% of dlmalloc and 6% faster than glibc.
  - Peak memory +23% on the AriDB bulk load, which counts against a program's quota.

My ranking for the user heap is (a), then (b). If the owner weighs a single code base over 23% of quota, (b).
- **talc** is not recommended for either heap now. v4 has the O(n) in-bin case, reproduced here. v5 is a recent rewrite with recent fixes, and is otherwise equivalent to TLSF on speed and footprint.
- **`linked_list_allocator` and the buddy crate are rejected,** on the measurements above.

**Changes independent of the choice**
1. Implement `GlobalAlloc::realloc` in `sdk/runtime` using the allocator's in-place grow and shrink (`rlsf::reallocate`, `dlmalloc::realloc`). Today every growth is alloc + copy + free.
2. Do not add a per-thread or size-class front end (mimalloc, snmalloc, SLUB style) yet. The incident is fully explained by the O(n) list. A front end is warranted only if lock contention under ADR 0082 threads is measured.
3. Keep the free-block count in `diag`, so a regression back to long lists is visible.

#### 5. Evidence that still needs confirming on the target

All figures above are from the build host. The laptop (i3-3217U, 3 MiB L3) should show the same flat profile once the allocator is replaced. That is the before/after test the failure protocol asks for: AriDB `run aridb N` at 5k/10k/25k/50k rows, plus a host unit test in `libs/heap` with N holes asserting that allocation cost does not scale with N. The kernel heap's real free-block count under load is not measured here; another angle or instrumentation should record it to size the interrupt-off risk.

**Sources:**
- [rlsf README and CHANGELOG](https://docs.rs/crate/rlsf/latest)
- [talc repository and issue #17](https://github.com/SFBdragon/talc/issues/17)
- [linked_list_allocator advisory CVE-2022-36086](https://github.com/rust-osdev/linked-list-allocator/security/advisories/GHSA-xg8p-34w2-j49j)
- [linked_list_allocator issue #70](https://github.com/rust-osdev/linked-list-allocator/issues/70)
- [Redox merge request: slab for the kernel heap](https://gitlab.redox-os.org/redox-os/kernel/-/merge_requests/76)
- [Theseus heap](https://theseus-os.com/Theseus/book/subsystems/heap.html)
- [Hermit allocator (talc)](https://docs.rs/crate/libhermit-rs/latest/source/src/mm/allocator.rs)
- [Zircon cmpctmalloc](https://fuchsia.googlesource.com/fuchsia/+/7fd09ca07f1449c3a590d9783ad47f13c13a25fd/zircon/kernel/lib/heap/cmpctmalloc/cmpctmalloc.cc)
- [Zephyr sys_heap](https://docs.zephyrproject.org/latest/kernel/memory_management/heap.html)
- [dlmalloc-rs issues #41 and #32](https://github.com/alexcrichton/dlmalloc-rs/issues/41)
- [rust-lang#144199](https://github.com/rust-lang/rust/issues/144199)
- [mimalloc paper](https://www.microsoft.com/en-us/research/publication/mimalloc-free-list-sharding-in-action)
- [Tock (Levy et al. 2017)](https://www.eecs.berkeley.edu/~prabal/pubs/papers/levy17tock.pdf)
- [Phil Opp, allocator designs](https://os.phil-opp.com/allocator-designs/)
- [RUSTSEC-2022-0054 (wee_alloc unmaintained)](https://rustsec.org/advisories/RUSTSEC-2022-0054)

### Study, angle 3: measurement (host trace-replay bench)

Author: study agent, angle 3. Date: 2026-11-20. Host: Ryzen 7 5800X (build host), pinned with `taskset -c 10,11`. All numbers are medians over 5 runs (3 for the fragmentation trace). Everything is outside the repository, under `/home/serverbig/aricode-scratch/heap-scratch/`:
- harness: `bench/src/main.rs`
- trace recorder: `recorder/`
- end-to-end probes: `build-*/probe`, `probe-veterans/`
- logs: `logs/`

Nothing in the worktree was changed.

#### 0. First finding: the trigger depends on the engine version

`wt-db` moved while this study ran: AriDB phase 2 (format 2, rebuilt write path) merged at 22:08 as `dbf12b5`. To avoid replaying a mix of versions, I exported two fixed commits with `git archive` (read-only) and built the probe and the recorder against each:

| End-to-end bulk load, µs/row (medians of 3 at 50k) | 5 000 | 10 000 | 25 000 | 50 000 |
|---|---|---|---|---|
| `b97dc7d` (laptop build), system allocator | 1.93 | 2.13 | 2.34 | 2.44 |
| `b97dc7d`, aricode-heap | 7.50 | 14.56 | 58.60 | **193.2** |
| `b97dc7d`, talc 4.4.3 (global allocator, same setup) | 1.84 | — | — | **2.21** |
| `b97dc7d`, rlsf 0.2.3 (global allocator, same setup) | 1.74 | — | — | **2.17** |
| `dbf12b5` (phase 2), system allocator | 0.88 | 0.92 | 1.02 | 1.06 |
| `dbf12b5`, aricode-heap | 0.81 | 0.87 | 0.96 | 1.00 |

- The incident reproduces exactly on `b97dc7d` (193 vs 2.44 µs/row; probe log said 190.9).
- On the phase-2 engine it is gone. That engine peaks at 2,849 live blocks instead of 158,244, and its free list never exceeds 101 blocks.
- **The allocator defect is unchanged**; only the AriDB trigger moved. The fix must not count on the engine change. The laptop app v2 will probably no longer show the 188x, which could make the incident look closed when it is not.

Logs: `logs/probe-both-engines.log`, `logs/probe-veterans-v1.log`.

#### 1. Mechanism, measured: the cost is in `deallocate`, not `allocate`

I added a "listlen" mode that replays the incident trace (b97dc7d, 50k rows) on aricode-heap and samples `free_blocks()` every 6,003 ops. Log: `logs/listlen400-aridb-v1-50k.log`.
- **During the insert loop** (ops 0 to 2.24M, up to 155k live blocks): the free list holds **2 to 12 blocks** and ops cost 6 to 9 ns. Allocation is cheap, because first-fit finds the big tail block at once.
- **When the transaction's blocks are released** (the last ~160k ops, 6.5% of the trace): they come back out of address order. The free list grows to **22,960 holes**, and each op costs up to **132 µs**.
- About **9.4 s of the 9.52 s replay falls in that phase**.
- Cause: `insert_free` walks the address-sorted list from `head` on every free (`libs/heap/src/lib.rs:90-98`, called from `deallocate` at `:202`). That makes the release of n blocks O(n·holes) = O(n²).
- Per-op timing agrees. Alloc mean 8.8 ns (p99 20 ns); **free mean 9,408 ns, p99 139 µs, max 272 µs**.

The incident's hypothesis ("every allocation walks the free list") needs this correction: on this workload the quadratic term is freeing into a long, address-sorted list. Allocation is also O(holes) when holes are many, though. In the fragmentation trace below, alloc averages 2.6 µs. `allocate_rounded` walks twice: once to search (`:148-149`) and once more from `head` to reinsert the split remainder (`:179` into `insert_free`).

#### 2. Harness

- **Interface.** One `RawHeap` trait (`add` region, `alloc`, `dealloc`, optional `realloc`) and a `Grown<H>` wrapper that applies the growth policy. All candidates take their regions from one 1 GiB source that is touched up front, so page faults are not timed.
- **Modes:**
  - `single`: one 1 GiB region, as in heap-probe.
  - `user`: ADR 0084. A 256 KiB first region, then regions of max(request + align + 4 KiB slack, 1 MiB). Regions are not contiguous (4 KiB gap), except for llff, which can only grow at its top.
  - `kernel`: 1 MiB contiguous chunks, as in `kernel/src/memory/heap.rs`.
- **Region size is a policy parameter per allocator** (`region_for`). Two veterans need it:
  - **buddy** needs a region twice the request's next power of two, because a 2^k block needs 2^k alignment and regions are only page aligned. Without that, a 4 MiB realloc failed right after a fresh 4 MiB+8 KiB region was added.
  - **rlsf** needs request + 1/16, because TLSF searches from the next second-level class up (SLLEN 32). Without it, the realloc after growth failed.
- **Realloc.** aricode-heap, llff and buddy get alloc + copy + free, which is what `sdk/runtime`'s `UserHeap` does, since it has no `realloc`. talc uses `grow`/`shrink` and rlsf uses `reallocate`; both grow in place when they can.
- **Timing.** rdtsc around each call, with the timer overhead (about 9.7 ns) included in the per-op numbers. Total time comes from separate runs without per-op timing.
- **Integrity check.** Each block gets an 8-byte tag written at alloc and checked at free and realloc. No overlaps were found for any candidate.
- **Footprint:**
  - "touched": the source pages any block has ever covered, i.e. what first-touch MEM objects would have to back.
  - "mapped": the bytes the growth policy took from the quota.
  - system: glibc `mallinfo2` (arena + mmap) at peak live.
- **Traces:**
  - **(a) AriDB.** Recorded by a tracing `GlobalAlloc` over System. For b97dc7d at 50k rows: 2.40M ops (1.02M alloc, 1.02M free, 358k realloc), peak live 38.4 MiB in 158,244 blocks, sizes mostly ≤ 256 B, alignments 1/4/8/16. For dbf12b5 at 50k rows: 456k ops, peak live 21.6 MiB in 2,849 blocks.
  - **(b) Kernel-like.** Generated, 2M steps giving 1.88M ops, peak live 0.15 MiB in 1,323 blocks. Sizes are read from the kernel sources, approximate, not measured:
    - IPC data `Vec<u8>` of 1..256 B (`IPC_MAX_DATA`); handle vectors of 16 B × 1..4 (`IPC_MAX_HANDLES`).
    - Endpoint `VecDeque<Message>` (48 B × 4..16, `IPC_QUEUE_CAPACITY`); `Arc<KernelObject>` of about 56 B.
    - Futex `VecDeque<Tid>` buffers; `Box<Thread>` of about 96 B plus name; `FpuArea` of 832 B aligned to 64.
    - Process structures: handle table `Vec` with 24-byte positions, console line up to 1,024 B.
  - **(c) Fragmentation stress.** Generated, 1M steps giving 2.0M ops, peak live 181 MiB:
    - 95% of blocks short-lived (1..2,000 ops), 5% long-lived (10k..200k ops).
    - Sizes 8 B..1 MiB (log-uniform above 256 B); 5% have alignment 16..4096.
    - The size mix shifts toward larger blocks at half-time; 3% of steps realloc a live block.

#### 3. Results

**(a) AriDB incident trace (b97dc7d, 50k rows), `single` mode**

| Allocator | Total | alloc mean / p99 (ns) | free mean / p99 (ns) | free max | Touched / peak live |
|---|---|---|---|---|---|
| aricode-heap | 9,621 ms | 8.8 / 20 | **9,408 / 139,112** | 272 µs | 1.21x |
| llff 0.10.6 | 8,621 ms | 8.1 / 20 | 8,449 / 131,732 | 259 µs | 1.12x |
| buddy 0.11.0 | 9,527 ms | 8.0 / 20 | 9,337 / 150,132 | 662 µs | 1.38x |
| **talc 4.4.3** | **16.4 ms** | 8.2 / 20 | 11.4 / 90 | 2.9 µs | 1.17x |
| **rlsf 0.2.3** | **17.6 ms** | 8.9 / 20 | 10.2 / 80 | 2.9 µs | 1.47x |
| system (glibc) | 25.4 ms | 8.7 / 30 | 15.1 / 110 | 969 µs | — |

**(a) The same trace in `user` mode (ADR 0084 growth)**

| Allocator | Total | Mapped |
|---|---|---|
| aricode-heap | 9,475 ms | 47.4 MiB in 34 regions |
| llff | 8,739 ms | 43.3 MiB in 30 regions (contiguous only) |
| talc | 17.6 ms | 45.4 MiB in 32 regions |
| rlsf | 18.0 ms | 48.4 MiB in 34 regions |
| buddy | 8,730 ms | **79.4 MiB** in 34 regions |
| system | 25.0 ms | — |

**Scaling (single mode, 5k to 50k rows):** aricode-heap goes from 29.9 ms to 9,621 ms (322x for 10x the rows, quadratic). talc goes from 1.4 to 16.4 ms (12x, linear).

**Phase-2 trace (dbf12b5, 50k rows):** every candidate takes 2.1 to 2.8 ms. In `user` mode, mapped is 26.3 MiB for aricode-heap, llff and talc, 32.3 MiB for rlsf and 55.3 MiB for buddy.

**(b) Kernel-like trace, `kernel` mode** (`single` mode gives the same within 1%)

| Allocator | Total | ns/op | alloc mean / p99 | free mean / p99 | realloc mean |
|---|---|---|---|---|---|
| aricode-heap | 87.1 ms | 46.4 | 53.4 / 360 | 33.9 / 130 | 192.5 |
| llff | 105.9 ms | 56.5 | 53.6 / 170 | 50.7 / 170 | 200.7 |
| talc | 24.8 ms | 13.2 | 12.6 / 30 | 11.6 / 30 | 28.6 |
| **rlsf** | **19.5 ms** | **10.4** | 11.1 / 30 | 10.2 / 30 | 26.1 |
| buddy | 27.8 ms | 14.8 | 8.1 / 10 | 19.7 / 50 | 28.4 |
| system | 21.2 ms | 11.3 | 10.2 / 60 | 9.7 / 30 | 50.5 |

Even with only 1,323 live blocks, aricode-heap is 4 to 5x slower than TLSF or talc, with a p99 alloc of 360 ns. All candidates fit in the first 1 MiB chunk (touched 0.17 to 0.20 MiB).

**(c) Fragmentation stress**

| Allocator | Total, `single` | alloc mean / p99 | free mean / p99 | Touched, `single` | Total, `user` | Mapped, `user` |
|---|---|---|---|---|---|---|
| aricode-heap | 4,094 ms | 2,625 / 10,350 | 1,449 / 5,290 | 1.12x | 4,155 ms | 221.3 MiB (1.22x) |
| llff | 4,628 ms | 2,338 / 6,660 | 2,246 / 6,390 | 1.12x | 4,607 ms | 204.4 MiB (1.13x, contiguous) |
| talc | 121 ms | 72.5 / 350 | 29.4 / 100 | 1.31x | 112 ms | 252.5 MiB (1.39x) |
| **rlsf** | **73 ms** | 32.7 / 30 | 27.0 / 100 | 1.19x | **37 ms** | **221.4 MiB (1.22x)** |
| buddy | 140 ms | 24.2 / 10 | 114 / 640 | 1.41x | 86 ms | 259.7 MiB (1.43x) |
| system | 179 ms | 84.9 / 280 | 48.9 / 130 | — | 222 ms | — |

A max of about 4 ms appears for every allocator here, the system allocator included, so it is preemption on a loaded host, not an allocator effect. rlsf's `user` vs `single` gap (37 vs 73 ms) was not investigated.

#### 4. Which candidates win where

- **Speed.**
  - rlsf (TLSF) is fastest or tied on every trace: best on kernel-like and fragmentation, within 7% of talc on AriDB.
  - talc is a close second and has the cheapest realloc on AriDB.
  - Both beat glibc on these single-threaded traces. Neither is ever slower than glibc by more than about 1.2x in the per-op mean (lock costs are not included).
- **Bounded latency.** Only rlsf is O(1) by design ("guaranteed to complete in constant time", its README), which suits a kernel heap under `IrqLock`. talc's own README says allocation is O(n) worst case, near-constant in practice. Here its p99 stayed at or under 350 ns and its max was set by the host.
- **Footprint.** Under ADR 0084 growth, rlsf matches aricode-heap: 1.22x on fragmentation, plus about 1 MiB on AriDB. In `single` mode it touches the most on the AriDB trace (1.47x), and it needs the region-sizing rule. talc is the worst of the two fast ones on fragmentation (1.39x mapped). llff is the tightest but only because it grows contiguously, which ADR 0084's MEM objects do not guarantee.
- **The same failure among veterans.**
  - linked_list_allocator has the same algorithm class and **fails exactly like ours** (8.6 s on the incident trace).
  - buddy_system_allocator **also fails**: its `dealloc` searches the per-order free list for the buddy (9.5 s). It also has power-of-two waste (1.4x) and needs 2x-sized regions under page-aligned growth.
  - Neither should be adopted.
- **Licences and sources.** talc is MIT; rlsf is MIT/Apache-2.0. Both are pure Rust, `no_std`, built from source, with no C code involved.

**Recommendation from the measurement angle:** TLSF (rlsf, or our own TLSF measured against it) as the first choice for both the kernel and the user heap. talc is the alternative if in-place realloc on large buffers matters more than bounded latency. In either case, `sdk/runtime` should implement `GlobalAlloc::realloc` once the allocator offers in-place growth. Today every `Vec` growth is alloc + copy + free (on fragmentation, realloc averages 5.9 µs vs 1.0 to 1.6 µs).

#### 5. Limits of this measurement

- Host CPU, not the laptop. No lock costs (`SpinLock`/`IrqLock` are not modelled).
- The replay writes only an 8-byte tag per block, not the program's real memory traffic.
- Kernel-trace sizes are read from the sources, not recorded from a running kernel. A kernel-side recording would confirm them.
- The fragmentation trace is synthetic.
- The rlsf/buddy rows in `user` mode come from the rerun with the region-sizing fix (`logs/m2-*.log`, binary `target-listlen/`). The rest of the matrix ran on `target/release/heap-bench`, built before that fix, which only changes growth for those two.
- Superseded logs: `logs/record-{5000,10000,25000,50000}.log`, `logs/smoke-aridb5k.log` and `logs/probe-current-engine.log` were built from an uncertain mix of engine versions while `wt-db` was moving. Do not use them.

#### Files

- Harness: `/home/serverbig/aricode-scratch/heap-scratch/bench/src/main.rs` (usage in its header; `listlen` mode for the mechanism check)
- Recorder: `/home/serverbig/aricode-scratch/heap-scratch/recorder/src/main.rs`
- Traces: `/home/serverbig/aricode-scratch/heap-scratch/traces/aridb-{b97dc7d,dbf12b5}-{5000,50000}.bin`
- Veteran end-to-end probe: `/home/serverbig/aricode-scratch/heap-scratch/probe-veterans/src/main.rs`
- Matrix script: `/home/serverbig/aricode-scratch/heap-scratch/run-matrix.sh`
- Logs: `/home/serverbig/aricode-scratch/heap-scratch/logs/` (`m-*.log`, `m2-*.log`, `listlen400-aridb-v1-50k.log`, `probe-both-engines.log`, `probe-veterans-v1.log`)

## Confirmation

What the evidence supports (the three angles agree, each with its own instrument):

- **Cause: `deallocate` cost O(free-list length).** `insert_free` (`libs/heap/src/lib.rs`
  at `f84e72f`, lines 90-100) walked the address-ordered free list from its head to find
  the freed block's place, which coalescing needed. While a large AriDB write transaction
  is alive the list stays at 2-15 blocks; at `COMMIT` the transaction's ~3 blocks per row
  are freed in page-number order, scattered in address and mostly between live
  neighbours, so the list grows to about 0.46·N holes (22,960 at 50,000 rows) and each
  free walks a large share of it: O(N²) for the commit. Evidence: the free-walk counters
  (angle 1: 84.7 → 839.2 nodes per free from 5k to 50k rows, exactly linear), the
  commit-window series and backtrace (angle 1), the `LEAK=long` causal check (skipping only
  those frees makes the cost flat, angle 1), the `listlen` replay (9.4 s of the 9.52 s in
  the release phase, angle 3) and the minimal reproduction (angle 1).
- **Latent, same class: `allocate`'s first-fit walk is O(free blocks)** too. Not the
  trigger here (2.2 nodes per allocation on this workload) but measured directly by the
  adversary test (angle 2: 8.6k → 1.03M cycles from 1k to 100k holes) and by the
  fragmentation trace (2.6 µs per allocation, angle 3).
- **The laptop's worse figure is the same mechanism with less cache** (i3-3217U, 3 MiB
  L3): the cost per node walked rises once the list leaves the cache (angle 1).
- **The incident's first hypothesis is corrected:** it is the free, not mainly the
  allocation, that walked the list on this workload.

Refuted: growth by memory objects making regions that never merge (angle 1: the same
list lengths and times with one region, 34 regions or forced gaps).

Left open:

- **The kernel heap's real free-list length under load** was never measured on a running
  kernel (angle 1, hypothesis 5): its live set is bounded (handles, IPC queues, threads),
  and the kernel-like trace of angle 3 was generated from sizes read in the sources. With
  the fix every kernel heap operation is bounded, so the question no longer decides
  anything, but the numbers for the kernel stay host replays.
- **The trigger moved, not the defect:** AriDB phase 2 (`dbf12b5`) keeps 2,849 live
  blocks instead of 158,244 and no longer shows it (angle 3). The confirmation on the
  laptop needs the v1 app (`b97dc7d`) on a system with the fix.

## Fix

### Decision, with the numbers

The veterans that won (angles 2 and 3) are segregated-fit designs with boundary tags:
TLSF (`rlsf`), `dlmalloc`, `talc`. Every single-list design (ours,
`linked_list_allocator`) and the buddy crate fail the same way. Of the winners:

- **`rlsf` and `dlmalloc` cannot be linked into user programs.** Both declare `libc` as an
  unconditional dependency on `cfg(unix)`, which the user programs' target
  (`x86_64-unknown-linux-gnu`, ADR 0065) is; `cargo xtask check` refuses `libc` in a user
  program's graph (`tools/xtask/src/deps.rs`, `HOST_OS_CRATES`). `rlsf` also ships no
  licence file, which the notices need. Adopting either for the user heap would mean
  breaking ADR 0065's rule or forking the crate.
- **`talc`** is MIT and links nothing, but v4 has the O(n) in-bin search (reproduced by
  angle 2: 51k → 510k cycles), v5 is a young rewrite, and it maps the most on the
  fragmentation trace (252 MiB against 214-221).
- So, following ADR 0063 (compare, then reuse or improve), **`aricode-heap` was improved
  with the winning design**: TLSF (Masmano et al., ECRTS 2004, with `rlsf` as the
  reference implementation, not copied) plus one idea from Zephyr's `sys_heap`, below.
  No third-party code was added, so no notice or ADR is needed for one.

**One allocator for both heaps** (the owner's rule of one base interface): `Heap` is the
same type in the kernel and in `sdk/runtime`. For the kernel the numbers allow it and
argue for it: it was not hit by the quadratic case in practice (a small live set), but on
the kernel-like trace the old list costs 4x per operation with a p99 allocation of 370 ns,
all of it with interrupts off under `IrqLock`; TLSF costs 11 ns with a p99 of 40 ns, and
no operation depends on the number of blocks. Keeping the list there would leave two
allocators to maintain and an unbounded interrupts-off path. The risk of changing the
kernel's allocator is covered by the randomized model test with a full invariant walk, the
double-free check and the kernel's own heap scenarios (Tests).

### Design (`libs/heap/src/lib.rs`)

- **Free lists by size, two levels:** 56 first-level classes (powers of two; level 0 for
  sizes under 512 bytes, one list per 16-byte granule) times 32 second-level ranges, and
  two bitmaps. Found with two bit scans, no loop.
- **Boundary tags:** every block has a 16-byte header (size with a `USED` bit, and the
  block physically before it), so `deallocate` merges with both neighbours in constant
  time. Each region ends in a 16-byte used sentinel; a region that starts where the last
  one ended (the kernel's contiguous chunks) continues it. The header still rules over the
  `Layout`, as before: large alignments split their leading gap off as a free block, so
  the header is always right before the pointer.
- **Bounded probe, then good fit.** Pure TLSF rounds a request up to the next list
  boundary so the head of any list from there fits. That never reuses a block of exactly
  the size just freed when the size is not a boundary: a 4096-byte page buffer needs a
  4112-byte block, and good fit only looks at 4224 and up. On the AriDB trace that cost 9
  extra regions (56.8 MiB mapped against 47.8). `allocate` therefore first looks at up to
  `PROBES` blocks of the list the request falls in, as Zephyr's `sys_heap` does
  (`CONFIG_SYS_HEAP_ALLOC_LOOPS`, default 3), then takes the good fit. `PROBES` is a type
  parameter with default `DEFAULT_PROBES` = 3, so the policy is tunable and an empty heap
  is all zeros (it lives in `.bss`, 14.6 KiB, costing no file space; as a field it put
  14 KiB into every program's `.data`).
- **`region_size_for(layout)`**: the size a fresh region needs to serve a request, the
  allocator's rounding included. `sdk/runtime` and the kernel grow by it (the old
  `size + align + slack` would fail right after growing for large requests, as angle 3
  found for `rlsf`).
- **`resize_in_place`**: shrink always, grow into a free block after it; `GlobalAlloc::realloc`
  in `sdk/runtime` and in the kernel uses it before falling back to allocate, copy, free
  (angles 1-3: realloc was left at the default).
- **Diagnostics:** `HeapStats` gains `free_blocks`, kept in O(1) (it was a list walk).

**Probe sweep** (bench harness, `user` mode = ADR 0084 growth; MiB mapped from the quota):

| `PROBES` | AriDB v1, 50k | AriDB v2, 50k | Fragmentation |
|---|---|---|---|
| 0 (pure good fit) | 56.80 | 31.70 | 222.59 |
| 1 | 47.76 | 25.68 | 223.55 |
| 3 | 47.76 | 25.68 | 214.00 |
| 8 | 47.76 | 25.68 | 217.55 |

Logs: `heap-scratch/logs/fix-exp-probes-sweep.log` (its times are inflated alike for every
row by an environment read the harness then made per call; footprints are unaffected),
`fix-exp-growth-causes.log` (what triggered growth: 19 growths for 4096-byte requests
with pure good fit), `fix-exp-realloc-modes.log`.

### Code

Commits on `fix/user-heap-quadratic`: `cc02c77` (the allocator, growth by
`region_size_for`) and `c120118` (`realloc` in place).

- `libs/heap`: the allocator above (one `unsafe` block per function, each with its
  `SAFETY` comment), and its tests.
- `sdk/runtime`: `SpinLock<Heap>` (no `Option`; the arena is added on first use), growth by
  `region_size_for`, `realloc` in place first.
- `kernel/src/memory/heap.rs`: growth by `region_size_for` (at least a chunk), `realloc`
  in place first under the lock, with the copy outside it.

## Tests

- **Fails before, passes after:**
  `libs/heap` `cost_does_not_grow_with_the_number_of_free_blocks`. With 1,000 and then
  100,000 free 48-byte holes kept apart by live blocks, it times the best of 7 batches of
  1,000 allocations of 256 bytes (a size no hole serves) and their frees in scrambled
  order, and asserts the large case costs less than 8 times the small one plus 2 ms.
  - Old allocator: 3.9 ms → 45 ms → 425-450 ms at 1k / 10k / 100k holes. It fails, about
    110x.
  - New: about 24 µs at every size. It passes.
  - The before run is in `heap-scratch/logs/fix-cost-test-before.log`, on a scratch copy of
    the old crate with the same test. The timings are in `fix-cost-timings.log`.
- **Correctness, all in `libs/heap`:**
  - `randomized_against_a_model`: 4 seeds × 20,000 random allocations, frees and resizes.
    It uses alignments from 1 to 4096 and sizes from 0 to 16 KiB, adds a contiguous region
    and a separate one along the way, and checks against a model of the live blocks:
    alignment, inside a region, no overlap, contents kept, and out of memory returning
    `None` with the heap unchanged. A full walk of the invariants runs every 997 steps:
    tiling, `prev_phys`, no two adjacent free blocks, each free block in its list, bitmaps
    and counters. At the end everything coalesces back to one block per region.
  - Unit tests:
    - reuse and coalescing;
    - alignments, with frees that ignore the `Layout`;
    - zero-sized requests;
    - out of memory;
    - unrepresentable sizes and alignments (audit 9d8c7e5, finding 1);
    - `region_size_for` serving every size and alignment from a fresh region;
    - contiguous regions merging and separate ones not;
    - absorption of a small tail;
    - `resize_in_place`;
    - the probe reusing a freed 4096-byte buffer (and pure good fit not reusing it);
    - a double free caught after a backward merge.
- **QEMU** (`cargo xtask test --all --area memory,user,wasm`, last run on the final code):
  58 scenarios, 0 failed.
  They include `memory.heap_selftest`, `memory.heap_oom_recovers`, `memory.big` (the runtime
  heap grown to 40 MiB through `region_size_for`), `memory.full`, the threads scenarios
  (the runtime lock) and the WebAssembly runner (`extend_heap`). The CPU peaked at 80 °C
  over the runs; the thermal guard was not running (its log stops on 2026-11-11), so the
  temperature was logged by hand (`heap-scratch/logs/fix-temp-during-area-*.log`). No
  release-level scenario belongs to these areas.
- No new QEMU scenario: the mechanism is in `libs/heap` and is proven on the host with a
  deterministic workload. A timing check under QEMU would be noisier than the host test
  and would prove nothing more. The target's proof is the AriDB app on the laptop (open
  points).
- `cargo xtask check` and `cargo xtask test --level unit` pass. Logs:
  `heap-scratch/logs/fix-xtask-*.log`.

## Measured after

The host, Ryzen 7 5800X, pinned with `taskset -c 10,11` while other agents loaded the
machine (load 2-5). The bench harness is angle 3's, copied to `heap-scratch/bench-fix`
with the old list (`aricode-heap-old`, a scratch copy of the crate at `f84e72f`) beside the
new one. Medians of 5 runs (3 for the fragmentation trace). Logs:
`heap-scratch/logs/fix-m-*.log`, `fix-matrix-summary.txt`.

**AriDB incident trace (`b97dc7d`, 50k rows)**

| Allocator | Total, `single` | Total, `user` | Mapped, `user` | Touched / peak live, `single` |
|---|---|---|---|---|
| aricode-heap (TLSF) | 17.8 ms | 18.3 ms | 47.76 MiB in 34 regions | 1.21x |
| aricode-heap-old | 9,660 ms | 9,700 ms | 47.39 MiB in 34 regions | 1.21x |
| rlsf 0.2.3 | 19.2 ms | 17.2 ms | 48.39 MiB in 34 regions | 1.47x |
| talc 4.4.3 | 17.4 ms | 16.5 ms | 45.39 MiB in 32 regions | 1.17x |
| system (glibc) | 25.3 ms | 26.8 ms | — | — |

- **Scaling, 5k to 50k rows (`single`):**
  - TLSF: 1.4 → 17.8 ms, 12.7x for 10x the rows, so linear.
  - Old list: 31.1 → 9,660 ms, 311x.
- **Mechanism, the `listlen` replay** (`fix-m-listlen-aridb-v1-50k.log`):
  - The free list still reaches 23,291 blocks: the fragmentation is the workload's.
  - The worst window costs 9.9 ns per operation, against 99,581 ns with the old list.

**AriDB phase-2 trace (`dbf12b5`, 50k rows, `user`):**

| Allocator | Total | Mapped |
|---|---|---|
| TLSF | 2.9 ms | 25.68 MiB |
| Old list | 4.0 ms | 26.31 MiB |
| rlsf | 3.8 ms | 32.33 MiB |
| talc | 3.3 ms | 26.31 MiB |
| system | 2.8 ms | — |

**Kernel-like trace (`kernel` mode, 1 MiB contiguous chunks)**

| Allocator | Total | alloc mean / p99 (ns) | free mean / p99 (ns) | realloc mean (ns) |
|---|---|---|---|---|
| aricode-heap (TLSF) | 21.8 ms | 11.3 / 39.7 | 10.4 / 30 | 28.5 |
| aricode-heap-old | 87.9 ms | 54.4 / 370 | 33.4 / 129.7 | 193.2 |
| rlsf | 19.4 ms | 11.3 / 30 | 10.8 / 30 | 26.0 |
| talc | 24.6 ms | 12.5 / 30 | 12.3 / 30 | 28.6 |
| system | 21.3 ms | 10.2 / 59.7 | 9.8 / 30 | 50.7 |

**Fragmentation stress**

| Allocator | Total, `single` | alloc mean / p99 (ns) | Touched, `single` | Total, `user` | Mapped, `user` |
|---|---|---|---|---|---|
| aricode-heap (TLSF) | 48.0 ms | 24.8 / 120 | 1.18x | 49.0 ms | 214.00 MiB |
| aricode-heap-old | 4,092 ms | 2,621 / 10,350 | 1.12x | 4,150 ms | 221.34 MiB |
| rlsf | 35.1 ms | 13.0 / 30 | 1.19x | 36.3 ms | 221.41 MiB |
| talc | 61.3 ms | 32.2 / 340 | 1.31x | 63.1 ms | 252.45 MiB |
| system | 95.0 ms | 43.9 / 280 | — | 93.5 ms | — |

On this trace the probes cost TLSF its speed lead: about two more cache misses per
allocation, so `rlsf` is 1.4x faster. In exchange TLSF has the smallest footprint of all
the candidates, and it is still 2x faster than glibc.

**End to end: the incident's probe** (AriDB `b97dc7d` on the host, each allocator as the
global allocator; the TLSF heap wrapped as `sdk/runtime` does it, with a 256 KiB arena,
regions of `max(region_size_for, 1 MiB)` 4 KiB apart, and `realloc` in place first).
Bulk load, µs per row, median of 3 (`heap-scratch/probe-fix`,
`logs/fix-probe-e2e-summary.txt`):

| Rows | 5 000 | 10 000 | 25 000 | 50 000 |
|---|---|---|---|---|
| aricode-heap (TLSF) | 1.69 | 1.80 | 2.02 | 2.15 |
| system | 2.00 | 2.10 | 2.51 | 2.48 |
| aricode-heap-old | 7.40 | 14.40 | 64.96 | 197.95 |

- The cost per row is flat, with the same slight rise as the system allocator, which is
  the B-tree growing.
- At 50,000 rows the heap took 47 MiB in 34 regions. The laptop measured 48,400 KiB with
  the old list.
- Other operations at 50,000 rows, µs (TLSF / system / old):

  | Operation | TLSF | system | old |
  |---|---|---|---|
  | Point SELECT | 1.41 | 1.16 | 1.48 |
  | Range + aggregate | 106.5 | 111.9 | 113.0 |
  | Index lookup | 60.6 | 62.2 | 70.5 |
  | Single-row durable INSERT | 13.24 | 13.57 | 11.96 |

- The single-row INSERT is the one case where the old list is faster, by about 1.2 µs. It
  is not the in-place `realloc` (13.34 without it) and not the region layout (13.19 over
  one region): `fix-exp-insert-*.log`. It is left open (below).

## What remains open

- **The laptop:** run the AriDB v1 app (`b97dc7d`, whose engine triggers the case) at
  5k/10k/25k/50k rows on a build with the fix; the cost per row should stay flat as on
  the host. The v2 app no longer triggers it (angle 3), so it cannot confirm the fix.
- **Audit** (failure protocol, step 6): kernel memory code changed, so an independent
  audit before merging.
- **Heap figures in `diag`:** neither the kernel heap's `HeapStats` (now with
  `free_blocks`) nor the programs' reach `diag`/`sysinfo`. Exporting them is a separate
  change to the ABI and the shell.
- **The kernel's real allocation mix** is still a generated trace (angle 3, limits);
  recording it from a running kernel would replace it.
- **Single-row durable INSERT ~10% slower than with the old list** on the host probe
  (equal to the system allocator); the two causes tried are ruled out; locality of the
  address-ordered first fit after the bulk load is the remaining guess, untested.
- **Static size:** `Heap` is 14.6 KiB (56 × 32 list heads), in `.bss` for the kernel and
  for every program, backed page by page on first touch (ADR 0049); the old one was a few
  words. Bounding the first level to the 48-bit address space would save about 4 KiB.
- **Lock cost and threads:** one lock per operation remains (angle 2 advises a per-thread
  front end only once contention under ADR 0082 threads is measured).
