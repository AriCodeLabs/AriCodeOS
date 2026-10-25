# Incident 0040: one process could push every other event out of the kernel ring

- Opened: 2026-10-25. Status: fixed on `fix/init-supervisor`.
- Found by: the failure-pattern audit of 2026-10-25 (audit E, L4: a
  shared log with no per-writer bound). Not seen on a machine.
- Environment: any boot; any process holds `event_record`.

## Evidence

`diag.event_flood` on the kernel at `df15084`: after `run hello_app 4`,
`run event_flood` (200 `event_record` calls) and `diag`, hello_app's
events were gone from the ring: the last 32 entries `diag` shows were all
the flood's. A program could hide what it did before (a refusal, a quota
hit, a fault of another process) the same way.

## Mechanism

The ring holds 64 entries and drops the oldest. `event_record` and the DMA
quota refusals (`dma_alloc`, `mmio_map`), which a process can cause in a
loop, recorded every call. netd and fsd meter themselves
(`aricode_faults::Meter`, ADR 0028), but nothing bounded a process that
does not.

## Fix

`aricode_faults::Budget`, kept per process by the kernel: 16 events at
once, one more every 500 ms, for the events a process's own calls cause.
What is over budget is counted, and the next event that fits is preceded
by `events dropped` with the count. Events the kernel records about a
process (created, exited, killed, DMA faults) are not charged.
