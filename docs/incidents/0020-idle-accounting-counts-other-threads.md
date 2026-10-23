# Incident 0020: idle accounting counts other threads' time as idle

- Opened: 2026-10-23. Status: reproduced on branch `fix/idle-accounting`
  (1 and 4 CPUs).
- Found by: the performance audit of `731c398`
  (`docs/audits/2026-10-23-performance-731c398.md`, finding A), which
  reviewed the raw counters of its control workload (evidence in
  `docs/audits/731c398-performance/`, `guest-instrumentation.patch`).
- Environment: any. QEMU 10.0.13 TCG in the `aricodeos-dev` container
  (`--cpus 10 --memory 12g --memory-swap 12g`), `-smp 1` and `-smp 4`.
- Affects: `sysinfo::IDLE`, hence `cpus` and `diag` in the shell and any
  measurement built on them; the smp self-test's incident 0002 report
  (`report_cpus_without_roamers`) reads the same counter.

## What the code did

`scheduler::idle_loop` read the TSC immediately before `sti; hlt` and
again after `hlt` returned, and added the difference to the CPU's
`idle_cycles`. `hlt` returns only after the interrupt handler that woke
the CPU has run and returned to the idle thread. The timer handler
(`apic::handle_interrupt` -> `scheduler::on_tick`) can switch threads
before it returns: on the BSP, when `wake_due` wakes a sleeper, it
preempts the idle thread there, on the idle thread's stack. The idle
thread then resumes only when the CPU next has nothing to run, possibly
hundreds of milliseconds later, returns through the handler, reads the
TSC and adds the whole interval, including the time other threads ran.

Two consequences:

1. The busy time of whatever ran in between is counted as idle.
2. The interval is added at its end, so a sample taken in between misses
   it and a later sample gets it at once: an interval that started before
   the first sample is counted in full in the difference, which is how
   idle could exceed the elapsed time (the audit's 105 %).

## Reproduction (2026-10-23, commit of this record)

`test=idle` (`scheduler::idle_selftest`), the audit's control in the
kernel: for CPU 0 and, with more CPUs, the last one, a thread pinned to
that CPU sleeps 50 ms (so the CPU idles and the thread is switched in from
the idle thread), samples the raw counters (`tsc_since_boot` and every
CPU's `idle_cycles`, what `sysinfo::IDLE` publishes), stays runnable for
500 ms, sleeps 200 ms and samples again. main waits on another CPU when
there is one. The assertions:

- on the control's CPU, `idle + busy <= elapsed` (1 % slack for where the
  samples fall);
- aggregate idle `+ busy <= cpus * elapsed` (same slack per CPU);
- the 200 ms sleep counts as idle: at least 150 ms on that CPU.

Scenarios `scheduler.idle_accounting` (1 CPU) and `smp.idle_accounting`
(4 CPUs, control on CPUs 0 and 3); area `idle` selects both.

Result on today's code (`cargo xtask test --all --area idle --jobs 2`):

```text
[FAIL] scheduler.idle_accounting
[INFO ] idle selftest: control on CPU 0: elapsed 2668720240 cycles (700 ms), busy 1905537930 cycles (500 ms), idle 2669298600 cycles (700 ms, 1000 permille; at most 285 permille possible); aggregate 2669298600 of 2668720240 cycles; idle permille per CPU [1000]
Reason: CPU 0 counted its busy interval as idle: idle 700 ms + busy 500 ms > elapsed 700 ms

[FAIL] smp.idle_accounting
[INFO ] idle selftest: control on CPU 0: elapsed 2705303942 cycles (709 ms), busy 1905470556 cycles (500 ms), idle 2706465070 cycles (710 ms, 1000 permille; at most 295 permille possible); aggregate 10823773928 of 10821215768 cycles; idle permille per CPU [1000, 999, 1000, 1000]
Reason: CPU 0 counted its busy interval as idle: idle 710 ms + busy 500 ms > elapsed 709 ms
```

On one CPU the control's 500 ms of spinning were reported as idle and
the CPU was "idle" for 100.02 % of the interval. On four CPUs the same
happens on CPU 0, and the aggregate exceeds `4 x elapsed`.

A first version of the test ran the control only on the last CPU; with 4
CPUs it passed on CPU 3 (idle 295 permille of at most 296). That matches
the mechanism: only the BSP wakes sleepers in its timer handler and
switches there. An AP's idle thread rarely gets switched away inside a
handler: the RESCHED IPI only leaves `hlt`, and the idle thread's quantum
counter has wrapped (see "Other observations"), so an AP picks new work in
`idle_loop`'s own `yield_now`, after closing the interval. The audit's 4
vCPU control did not show the aggregate overcount for the same reason: its
benchmark thread happened not to be switched in by CPU 0's handler.

## Mechanism

Confirmed by the reproduction: the idle interval is closed by the idle
thread itself when it next runs, not when it stops being the running
thread. Any path that switches away from the idle thread inside an
interrupt handler taken from `hlt` (today: the BSP's `on_tick` with
`woke > 0`, and a quantum expiry if it ever fires on the idle thread)
adds the other threads' time to idle.

## Other observations (not this incident's fix)

- `Cpu::quantum_tick` (`kernel/src/cpu.rs`) does `fetch_sub(1) <= 1`.
  When the quantum expires and `preempt` finds nothing else ready, no
  switch happens and the counter is not reset: the next tick takes it
  from 0 to `u32::MAX` and the thread is not preempted by quantum again
  for about 4 x 10^9 ticks. A thread then made ready with `wake` or
  `spawn` on that CPU (not a sleeper waking on the BSP, which preempts
  regardless) waits until the running thread blocks or yields, unless
  another CPU takes it. To be recorded and studied as its own incident.
- `kick_idle` sends RESCHED to the first idle CPU other than the caller,
  even when the thread made ready is pinned to another CPU: the pinned
  thread waits for its CPU's next tick (up to 10 ms). A latency cost, not
  a correctness defect.
- The shell's `cpus` clamped each CPU's idle cycles to the elapsed cycles
  (`.min(elapsed)`), which hid exactly this overcount on screen.
