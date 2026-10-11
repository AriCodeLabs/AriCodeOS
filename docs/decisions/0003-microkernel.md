# ADR 0003: Pragmatic microkernel

- **Date:** 2026-09-26
- **Status:** Accepted

## Decision

The kernel contains only the scheduler, virtual memory, IPC, interrupts,
capabilities and SMP. Storage, networking, USB, audio, the filesystem, the
compositor and the device manager live in user space.

## Consequences

- `driver failure != kernel failure`; drivers can be restarted.
- IPC is critical and is measured from the early phases.
- Cost: more context switches than a monolithic kernel; it will be optimised with metrics.
