# Incident 0015: the scheduler self-test assumed the wait-queue order

- Opened: 2026-10-20. Status: closed 2026-10-20, fixed in `fix/sched-selftest-wake-order`
  (see Fix); pending the reruns listed under Tests.
- Seen: `cargo xtask test --all --cpus 4` on `fix/arifs-descriptor-reclaim`
  b7222fc (a user-space change; the kernel is develop 743c85b's), scenario
  `scheduler.selftest` (`test=sched`): kernel panic at
  `kernel/src/task/scheduler.rs:600`, "wake_one must release only one:
  left 0, right 1", CPU 0. The same scenario had passed in the five
  previous 4-CPU full runs of 2026-10-18/19 and passed 8 of 8 reruns
  right after the failure: a rare ordering race, not a regression of the
  branch under test.

## Mechanism

Part 3 of the self-test spawns two consumers that block on a wait queue
with per-consumer conditions (bit 0 for consumer-0, bit 1 for
consumer-1), sets the condition of consumer-0 only and calls `wake_one`,
assuming consumer-0 is the first waiter in the queue because it was
spawned first. With one CPU that order always holds. With four CPUs the
two threads start on different CPUs and block in either order; when
consumer-1 reached the queue first, `wake_one` popped it, its condition
was false, `wait_until` blocked it again, consumer-0 was never woken and
`CONSUMED` stayed 0. `WaitQueue::wake_one` itself is correct: it releases
the first waiter, whose predicate decides whether it proceeds.

Same family as incident 0002 (an SMP self-test assuming a scheduling
order), found by the failure protocol's rule that a red suite is never
rerun into green: the 4-CPU run of a user-space branch was not merged
until the panic was explained.

## Fix

The condition is made true for both consumers before `wake_one`, so
exactly one runs whichever blocked first, and the assertions check that
`CONSUMED` is 1 and that exactly one of the two is still blocked, without
naming which; `wake_all` then releases the other and both are consumed.
The test still proves what it claimed: `wake_one` releases exactly one
waiter and `wake_all` the rest.

## Tests

- `scheduler.selftest` with `--cpus 4` run 10 times in a row after the
  change, and with 1 CPU (the smoke level); the full suite with 1 and 4
  CPUs on `develop` after the merge.
- The failing interleaving cannot be forced deterministically from the
  test; the reruns and the reasoning above are the evidence.

## Open

- Incident 0002 (`smp.selftest`) stays as it is; its self-test was
  already changed to wait with a deadline.

## Closure (2026-10-20)

Merged into `develop`; 10 of 10 reruns of `scheduler.selftest` with 4 CPUs
after the change, then the full suite green with 1 and 4 CPUs on the merged
`develop` (see the merge commit).
