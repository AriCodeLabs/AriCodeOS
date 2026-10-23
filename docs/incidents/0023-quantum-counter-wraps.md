# Incident 0023: a thread alone on its CPU stopped being preempted by quantum

- Opened: 2026-10-23. Status: fixed on `fix/qmp-typing-and-quantum`.
- Found by: the agent of incident 0020 while reading the scheduler;
  confirmed by the independent audit of the incident 0020 branch.
- Environment: every build since SMP (Phase 32); every CPU, the BSP
  included, but only the BSP has a second way to preempt (below).

## Mechanism

`Cpu::quantum_tick` (kernel/src/cpu.rs) did
`quantum_left.fetch_sub(1) <= 1`, and only `set_current` reset the
counter, which happens only when the scheduler switches threads. When the
quantum expired and `preempt` found nothing else ready for the CPU,
`RoundRobin::switch` returned `None` (next == current), `set_current` was
not called and the counter was left at 0. With a quantum of 2 ticks:

| tick | counter before | `fetch_sub` returns | expired |
|---|---|---|---|
| 1 | 2 | 2 | no |
| 2 | 1 | 1 | yes, nothing ready, no reset |
| 3 | 0 | 0, counter wraps to `u32::MAX` | yes, nothing ready |
| 4 on | `u32::MAX - n` | huge | never (about 497 days at 100 Hz) |

So a thread that ran alone on a CPU for 30 ms was never again preempted by
quantum. A thread that later became ready for that CPU (woken from a wait
queue, spawned there, or unpinned with every other CPU busy) waited until
the running thread blocked, slept, yielded or exited. Nothing else
interrupts a busy CPU: `kick_idle` sends its IPI only to idle CPUs, and
the only other preemption, `woke > 0` in `on_tick`, happens on the BSP
when it wakes sleepers and preempts only the BSP itself
(kernel/src/task/scheduler.rs). Concrete failure (audit): with 4 CPUs, a
CPU-bound process alone on AP1 wraps within 30 ms and then keeps AP1 to
itself forever, while every other thread that lands in the queue for AP1
waits; a thread pinned there starves.

## Reproduction

`test=quantum` (scenario `scheduler.quantum`, 2 CPUs,
`kernel/src/task/scheduler.rs::quantum_selftest`): main waits and
measures on CPU 0; a thread pinned to CPU 1 blocks on a wait queue; a
thread that never blocks then runs alone on CPU 1 for 100 ms; main wakes
the waiter and measures in ticks until it runs, waits another 100 ms,
spawns a thread pinned to CPU 1 and measures again. Each must start within
three quanta (6 ticks); main gives up after 50 ticks.

Before the fix (`cargo xtask test --level full --area scheduler` in the
development container, commit 377bf31):

```text
[INFO ] quantum selftest: CPU 1 busy alone for 100 ms, then a woken thread started after None ticks and a spawned one after None ticks (limit 6, gave up after 50)
ARICODEOS KERNEL PANIC
Reason: a woken thread pinned to busy CPU 1 waited None ticks (limit 6): its quantum no longer expires
```

After the fix:

```text
[INFO ] quantum selftest: CPU 1 busy alone for 100 ms, then a woken thread started after Some(0) ticks and a spawned one after Some(1) ticks (limit 6, gave up after 50)
[INFO ] quantum selftest ok (woken after Some(0), spawned after Some(1) ticks)
```

## Fix

- `aricode_sched::quantum_tick(left, quantum)` (libs/sched): saturating
  decrement; on expiry the quantum starts again at once, whether or not a
  switch follows. `Cpu::quantum_tick(quantum)` applies it (load and store:
  only the owning CPU touches the counter, with interrupts disabled). A
  lone thread now gets an expiry every quantum; `preempt` then finds
  nothing and returns at once, or finds the newly ready thread and
  switches. The longest wait for a busy CPU is one quantum.
- `PREEMPTIONS` (`SchedStats::preemptions`, printed by `test=sched`) now
  counts only the expiries and wakeups that switched threads: with the fix
  every CPU expires every 20 ms, and counting the empty ones would turn the
  statistic into a clock.
- Idle CPUs now also go through `preempt` from `hlt` every quantum (before,
  their counter wrapped too). It finds nothing and costs one acquisition of
  the scheduler lock; the idle accounting of incident 0020 measures idle
  time at the switches, so a handler taken from `hlt` that does not switch
  leaves the idle stretch open, as `test=idle` checks.

## `kick_idle` (fixed here)

`kick_idle` sent one IPI to the first idle CPU other than the caller,
without looking at what was ready:

- a thread pinned to CPU X could wake idle CPU Y, which cannot run it,
  while X stayed in `hlt` until its next tick (up to 10 ms);
- a batch of wakeups (`wake_due` returning more than one) woke a single
  CPU.

`RoundRobin::idle_cpus_to_wake(me, online)` (libs/sched) now chooses the
CPUs: each ready thread, in queue order, claims the first idle online CPU
other than the caller that may run it. A pinned thread wakes its own CPU or
nobody, a batch wakes as many idle CPUs as can take a thread, and never
more CPUs than ready threads. `kick_idle` sends one RESCHED IPI per chosen
CPU. `cpu::idle_other_than` had no other user and is removed.

## Tests

- Host (libs/sched): `a_lone_thread_keeps_getting_quantum_expiries`
  (1000 ticks without a switch: an expiry every quantum, the counter always
  in 1..=quantum, a zero counter restarts instead of wrapping),
  `a_pinned_thread_wakes_its_own_idle_cpu`,
  `a_batch_of_wakeups_wakes_one_idle_cpu_per_thread`.
- QEMU: `scheduler.quantum` (above); areas `scheduler` and `smp`; the full
  suite, since this changes scheduling on every CPU: `--all` 170 of 170
  and `--all --cpus 4` 170 of 170 in the development container, branch
  rebased on `develop` adc43a0.
