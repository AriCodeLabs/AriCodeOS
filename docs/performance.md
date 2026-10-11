# Performance measurements

What is measured, how to reproduce it and what the numbers mean. All
figures come from QEMU with TCG (no KVM) on the development machine: they
are for comparing builds against each other, not for predicting real
hardware. A change of host, QEMU mode or CPU count invalidates the
comparison.

## How

```bash
cargo xtask run --cmdline "shell"
ari> run bench
ari> cpus
```

Or the `perf.bench` scenario (`cargo xtask test --all --area perf`), whose
`serial.log` keeps the numbers. Run `run bench` twice and keep the second
result: the first one pays for the program's pages and the buffers.

## What `bench` measures

`apps/bench` is an ordinary broker application (`system`, `fs`, 64 KiB):
it sees nothing that any other application cannot see.

| Measurement | What it includes | Samples |
|---|---|---|
| `syscall thread_get_tid` | entry and exit via `syscall`, no work | 10 000 |
| `ipc fsd list` | full `rpc::call`: create reply endpoint, send, wait, receive, close; the fsd loop (10 ms wait on the main endpoint, no wait on granted ones) | 1 000 |
| `replace 4 KiB` | atomic replace: journal, two `flush` to virtio-blk, copy and cleanup | 100 |
| `read 4 KiB` | one read over IPC with a copy through the client's DMA buffer | 100 |

Percentiles p50, p95 and p99 over the TSC, calibrated against the system
clock (200 ms; 10 ms granularity, error under 5 %).

## Per-CPU idle

Besides the online CPUs, `cpus` shows the percentage of cycles each one has
spent in `hlt` since boot (`sysinfo::IDLE`): with the system quiet it
should be high; a CPU at 0 % while idle has a thread that does not sleep
(`ps` shows it). This is the basis for power measurements: without `hlt`
there are no sleep states to save anything.

## Report

`diag` gathers `uptime`, `mem`, `cpus`, `ps` and `services --detail` into a
single block: it is what you ask from someone reporting a problem.

## Baseline (0.7, QEMU 10 TCG, 1 CPU, `-cpu qemu64`)

Filled in with the first `perf.bench` run of each version; the values serve
as an alarm (a 2x regression is noticeable), not as a guarantee.

Calibration: 3 813 066 TSC cycles per ms.

| Measurement | p50 | p95 | p99 | Note |
|---|---|---|---|---|
| syscall (ns) | 99 | 109 | 119 | TCG translates the path once; do not extrapolate to hardware |
| ipc fsd list (µs) | 9 993 | 10 078 | 10 227 | dominated by the fsd loop's 10 ms wait on the main endpoint: requests on granted endpoints waited for it to expire (ADR 0020 fixes it) |
| replace 4 KiB (µs) | 9 999 | 10 266 | 12 585 | same; the real work (journal and two `flush`) is masked |
| read 4 KiB (µs) | 9 994 | 10 101 | 10 135 | same |

CPU idle after booting with shell: 81-82 %.

### After `ipc_receive_any` (ADR 0020), same build except fsd

| Measurement | p50 | p95 | p99 | Note |
|---|---|---|---|---|
| syscall (ns) | 99 | 129 | 129 | unchanged |
| ipc fsd list (µs) | 44 | 46 | 58 | 227 times less: fsd waits on all its endpoints at once |
| replace 4 KiB (µs) | 5 708 | 6 343 | 6 458 | the real work is now visible: about 20 single-sector requests to the block service and four `flush`; the next candidate is batching sectors per request |
| read 4 KiB (µs) | 712 | 800 | 838 | eight single-sector reads over IPC to the block service |

### After sector batching and one fewer `flush` (stage 5), same build except AriFS and fsd

| Measurement | p50 | p95 | p99 | Note |
|---|---|---|---|---|
| syscall (ns) | 99 | 109 | 129 | unchanged |
| ipc fsd list (µs) | 46 | 49 | 63 | unchanged |
| replace 4 KiB (µs) | 2 628 | 2 817 | 2 997 | three `flush` (each one a host fsync, 0.5 to 3 ms depending on the disk) and five requests to the block service; between runs it varies with the host more than with the code |
| read 4 KiB (µs) | 141 | 150 | 234 | one eight-sector request instead of eight single-sector ones |

What remains of the cost of `replace` is the fsync: reducing it requires
fewer commits (batching replacements) or storage with a battery-backed
write cache, not more code.
