# ADR 0011: Lifecycle of resident services and queryable status

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

Up to 0.6.0, `init` launched each driver and resident service once and only
checked its exit during boot. A service that died later left its clients
waiting on an endpoint with no server, and nothing reported it.
The study `docs/audits/2026-09-28-referentes-capacidades-oportunidades.md`
(stage 1, "observable recovery") calls for a common lifecycle contract.

## Decision

1. **States and generations** (`libs/supervisor`, no dependencies, tested on
   the host): `Stopped`, `Starting`, `Ready`, `Degraded`, `Failed`. Each start
   of a service gets a new generation. A service is mandatory or
   optional: if an optional one exhausts its budget, the system becomes
   degraded; if a mandatory one does, failed.
2. **Restart with a budget**: after a death (exit, fault or expired start
   deadline) the supervisor waits `backoff_initial · 2^(n-1)` ms,
   capped at `backoff_max`, up to `max_restarts` times; after `stable_ms` in
   `Ready` the budget is restored. Default policy for drivers:
   5 restarts, 100 ms to 5 s, start deadline 5 s, stability 60 s.
3. **The endpoint belongs to init and does not change**: the stable identity of a service
   is its endpoint; each generation receives a duplicate. Clients keep
   sending to the same endpoint and the new generation serves what is queued.
   A PCI function whose driver died can be claimed again: the kernel
   considers it free once no handle to its configuration object exists
   any more (`PciDevice::claimed` is a `Weak`).
4. **Publish only what responds**: a service with a probe (`Info` on
   startup, `ping`) is published when it answers; if it does not answer or its process
   exits, it counts as a death by start deadline.
5. **Queryable status**: protocol `status` v1 (`sdk/proto`, protocol 7)
   served by init on the broker endpoint, one request per service;
   `services --detail` in the shell shows it. It carries no private content.
6. **Test via cmdline** (ADR 0007): `restart_storm` subjects a test
   service to 100 restarts while fsd keeps serving and checks that
   free memory returns to its previous level.

## Note (audit 6ee19fb, finding 4)

When the start probe expires, the supervisor does not simply consider the
generation dead: if the process is still alive it terminates it with `process_kill`
and waits for the kernel to reap it (up to 2 s) before announcing the death
and scheduling the next one; that way the new generation does not compete with the
previous one for its PCI devices. If it still does not exit, the service is left
failed without relaunching (`services.start_timeout`).

## Consequences

- Supervision lives in init for as long as init lives (`shell`/`gui` mode); in
  a test boot without a shell init exits and there is nobody to restart.
- A client with a request in flight when the server dies gets no
  reply: the result of that request is unknown. Clients
  bound their waits and do not retry writes automatically (stage 1B
  will handle correlation and retry semantics).
- Restarting a driver does not stop the device's in-flight DMA: with the
  IOMMU active the dead process's domain is withdrawn before its
  frames are reused; without an IOMMU the restart is a documented risk.

## Note (ADR 0030, 2026-10-17)

init keeps the receive side of every supervised endpoint so that the next
generation serves the same endpoint; the kernel therefore cannot tell on
its own that a dead service will not come back. When the supervisor gives
a service up (restart budget exhausted, or `strict`), it closes the
endpoints the service served with `endpoint_close`: queued requests and
every later one fail at once with `PeerClosed` instead of waiting their
reply timeouts. A request the dead process was holding fails at once in
any case, since its reply capability dies with the process. A closed
endpoint is never served again.
