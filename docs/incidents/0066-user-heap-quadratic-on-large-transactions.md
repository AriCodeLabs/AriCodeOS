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

## Study

(Reports attached below as they arrive.)
