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

Or the `perf.bench` scenario (`cargo xtask test --all --area perf --jobs
1`; `--cpus 4` for the SMP figures), whose `serial.log` keeps the numbers.
It runs `bench` twice; keep the second result: the first one pays for the
program's pages and the buffers. With other guests running at the same
time (`--jobs` above 1) the figures are not comparable.

## What `bench` measures

`apps/bench` is an ordinary broker application (`system`, `fs`, `net`,
64 KiB): it sees nothing that any other application cannot see.

| Measurement | What it includes | Samples |
|---|---|---|
| `syscall thread_get_tid` | entry and exit via `syscall`, no work | 10 000 |
| `ipc fsd list` | full `rpc::call`: create reply endpoint, send, wait, receive, close; fsd waits on its main endpoint and every session at once (ADR 0020, ADR 0044) | 1 000 |
| `replace 4 KiB` | atomic replace: journal, two `flush` to virtio-blk, copy and cleanup | 100 |
| `read 4 KiB` | one read over IPC with a copy through the client's DMA buffer | 100 |
| `ipc net info` | full `rpc::call` to netd's session for `NetClient::info`: no frame leaves the machine, so it measures how soon netd sees a request (ADR 0044) | 200 |
| `quiet idle` | per-CPU and overall idle permille over a quiet 2 s sleep of `bench` (`sysinfo::IDLE`, exact since incident 0020): what the system's own wakeups leave | 1 interval |

Percentiles p50, p95 and p99 over the TSC, calibrated against the system
clock (200 ms; 10 ms granularity, error under 5 %).

## Per-CPU idle

Besides the online CPUs, `cpus` shows the percentage of cycles since boot
during which each CPU had nothing to run (`sysinfo::IDLE`): with the
system quiet it should be high; a CPU at 0 % while idle has a thread that
does not sleep (`ps` shows it).

What idle means (since incident 0020): the time the CPU's idle thread is
its running thread. The scheduler opens an idle stretch when it switches
to the idle thread and closes it when it switches away, wherever that
switch happens, including inside a timer handler taken from `hlt`. So:

- another thread's time is never idle, even when that thread was switched
  in by an interrupt that woke the CPU from `hlt`;
- interrupt handlers that run on the idle thread and return to it without
  a switch count as idle (they are short: a tick, an IPI, a device IRQ);
- the kernel samples the elapsed cycles and every CPU's idle cycles at
  one TSC reading, the current idle stretch included, taken with every
  CPU's idle clock locked (each switch reads its own TSC under its CPU's
  lock), so no stretch in a sample ends after that reading: a CPU's idle
  never exceeds the elapsed time and two samples measure exactly the idle
  time between them. `cpus` does not clamp the value: above 100 % would
  be a defect to report.

Before 0.14.0 (incident 0020) the idle loop measured each `sti; hlt` and
added it when `hlt` returned, so a thread switched in by the handler that
ended the `hlt` had its whole run counted as idle: the audit of 731c398
saw 105 % on one CPU, and `test=idle` 1000 permille where at most 285 were
possible. Idle figures recorded before the fix (the 81-82 % below
included) are not comparable.

Idle time is not a power measurement: it says how often the CPU had
nothing to run, not how deep it slept or what it drew. Without `hlt`
there are no sleep states to save anything, but energy claims need
hardware measurements.

## Report

`diag` gathers `uptime`, `mem`, `cpus`, the `entropy` line (ADR 0039), `ps`
and `services --detail` into a single block: it is what you ask from
someone reporting a problem.

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

## One wait for IPC, IRQ and deadline (ADR 0044), 0.14.0 development

Performance audit of 731c398, finding B: netd polled its endpoints and
then slept in `irq_wait(IRQ, 10)`, so a request that arrived after the poll
waited for the next interrupt or for the 10 ms. Same build of everything
else; QEMU 10 TCG, `-cpu qemu64`, `perf.bench` alone (`--jobs 1`), second
run of `bench`, p50 / p95 / p99. Before is `f150092` (the bench of this
change on the former loops), after is `00a310f`.

| Measurement | 1 CPU before | 1 CPU after | 4 CPUs before | 4 CPUs after |
|---|---|---|---|---|
| ipc net info (µs) | 10 001 / 10 121 / 10 244 | 71 / 79 / 89 | 9 999 / 10 073 / 10 104 | 74 / 84 / 106 |
| ipc fsd list (µs) | 58 / 64 / 69 | 62 / 66 / 78 | 80 / 92 / 116 | 69 / 108 / 116 |
| read 4 KiB (µs) | 167 / 189 / 288 | 180 / 188 / 204 | 214 / 248 / 304 | 198 / 219 / 234 |
| quiet idle (permille) | 986 | 989 | 992 (cpu 0: 990) | 997 (cpu 0: 993) |

- The 10 ms plateau is gone: 140 times less on one CPU, 135 on four. A
  second pair of runs gave the same (before 10 012 / 10 132 / 10 246, after
  69 / 75 / 108 on one CPU).
- Idle wakeups went down, not up: an idle netd wakes once a second (its
  client sweep) instead of a hundred times, and an idle input driver only
  for a request or an event. The quiet interval is idle 3 to 5 permille
  more; what remains is mostly each CPU's 100 Hz tick.
- `ipc fsd list` costs about 4 µs more on one CPU: fsd's wait now registers
  on the queue of each of its endpoints (main and sessions) instead of one
  global queue, and checks them again when it returns. That global queue
  was the price of the first version (ADR 0044, "Measured on the way"):
  every send woke every service waiting on a set, which made `diag` take
  9.7 s instead of 4.0 s in `usb.hid_fault` and fsd's list 74 µs.
- `replace 4 KiB` is dominated by the host's fsync and moved between 2.7
  and 9 ms from run to run on both builds (the same build measured 2 743
  and 8 759 µs an hour apart): no comparison is made from it.

## Display flush (ADR 0045), 0.14.0 development

Performance audit of 731c398, finding C: every change wrote the whole
back buffer to the framebuffer. Deterministic pixel counts from the
`aricode-gfx` host test `flushed_pixels_per_event` (`cargo test -p
aricode-gfx flushed -- --nocapture` prints them), which also compares the
result with a whole-screen composition; 4 bytes per pixel.

| Event | Before | After |
|---|---:|---:|
| Pointer moves 1 px, 1024x768 | 786,432 px / 3,145,728 B | 247 px / 988 B |
| Pointer moves 1 px, 1920x1080 | 2,073,600 px / 8,294,400 B | 247 px / 988 B |
| A 400x300 window presents | whole screen | 120,000 px / 480,000 B |
| A 400x300 window moves 10 px | whole screen | 138,276 px / 553,104 B |

- In the guest, `gui.two_windows` (1280x800) logs every frame with
  `gui_trace`: 1,024,000 pixels for the first one, 130,896 for a window
  opening, 120,000 for a `Present`, 456 for a 20-pixel pointer move; each
  of those frames wrote 1,024,000 before.
- Frames are coalesced to one per 16 ms at most, and print nothing
  without the test-only key `gui_trace`.
- These are pixels written, not frame rates: composition time, frame
  latency and the cost of a write to a real framebuffer were not measured.

