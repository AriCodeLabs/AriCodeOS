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

- A share of the ring per process (`aricode_faults::SharedRing`,
  `events::SHARE` = 8): the events a process caused (`event_record`, its
  DMA quota refusals, the DMA faults of a device in its IOMMU domain) never
  hold more than 8 entries; the next one replaces that process's own
  oldest. A loop, however slow, overwrites itself, not the boot's events or
  another process's.
- A rate per process (`aricode_faults::Budget`): 16 events at once, one
  more every 500 ms counted from the burst; what is over it is counted and
  the next event that fits is preceded by `events dropped` with the count.
- Events the kernel records about a process (created, exited, killed) stay
  unbounded: only init and the broker create processes. A shell user who
  launches programs in a loop can still fill the ring with their
  lifecycle events (residual, needs an authenticated shell).

## Audit

The independent audit found that the first version (the rate alone)
bounded how fast a process wrote, not how much of the ring it held: a
loop at one event per 500 ms still emptied the ring in half a minute.
The share above closes it; the DMA faults, recorded from the IOMMU
interrupt, are now charged to the device's owner, and refills count from
the start of a burst so the test's count is exact.
