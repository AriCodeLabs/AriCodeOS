# Incident 0002: `smp.selftest` panics "unpinned threads must pass through every CPU"

- **Opened:** 2026-10-12. **Status:** open.
- **Environment:** QEMU, 4 virtual CPUs (`cargo xtask test --all --cpus 4`),
  host with 16 cores under load from a concurrent build and another QEMU
  suite finishing.
- **Software:** branch `fix/usb-endpoint-recovery` at e78ce2d plus
  user-space changes only; the kernel binary is the one of `develop`
  e78ce2d (no kernel file differs on the branch).

## What was seen

One run of the full suite with 4 CPUs (the fourth version of the USB
recovery branch):

```
[INFO ] requested test: smp
ARICODEOS KERNEL PANIC
Where: kernel/src/task/scheduler.rs:701:5
Reason: assertion `left == right` failed: unpinned threads must pass through every CPU
  left: 7
 right: 15
CPU: 1
```

The self-test records, as a bit mask, the CPUs on which an unpinned
thread has run and asserts the mask equals all online CPUs (15 = CPUs
0-3); it reached 7 (CPUs 0-2): the thread never ran on CPU 3 within the
test's time.

## Frequency

The same scenario passed in the 16 previous 4-CPU full runs of this
session. The panic coincided with a host loaded by a concurrent cargo
build and the tail of another QEMU suite. Retried five times in a row on
a quiet host (load average 1.3) right afterwards: 5 passes. One failure in
22 runs, only under load, which supports H1 but does not prove it.

## Hypotheses

- H1 The self-test assumes that migration reaches every CPU within a
  bounded number of scheduling rounds; under host load a virtual CPU can
  be descheduled by the host long enough for the test to finish without
  it. The assertion then measures host scheduling, not the kernel.
- H2 A real scheduler defect (a CPU that stops picking unpinned threads)
  that only shows under load. Nothing in the kernel changed since the
  passing runs, which makes this less likely but not excluded.

## Plan

1. Rerun the scenario with 4 CPUs several times on a quiet host and on a
   loaded host; record the outcomes here.
2. If H1: make the self-test wait for the condition with a deadline (and
   report which CPU did not run the thread, with its idle time) instead of
   asserting after a fixed number of rounds, so a slow host produces a
   message, not a panic; keep the panic for a CPU that never runs the
   thread within the deadline.
3. If H2: instrument the per-CPU run queues in the self-test output.

## Open

Not caused by the USB recovery branch; the merge of that branch is not
blocked by this incident.
