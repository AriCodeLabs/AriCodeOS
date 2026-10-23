# ADR 0020: Receiving from several endpoints (`ipc_receive_any`)

- **Date:** 2026-09-29
- **Status:** Accepted

## Context

fsd serves a main endpoint and up to eight granted ones (private
directories and shared files), but `ipc_receive` only waits on one: the
loop waited 10 ms on the main one and polled the rest without waiting. A
request on a granted endpoint (every normal application) waited on
average 5 ms for that wait to expire. `bench` (stage 5) measured it: an
IPC call to fsd from an application cost milliseconds while the rest of
the path costs microseconds. This is the first measurement-driven
optimization.

## Decision

1. **Syscall `ipc_receive_any` (31)**: receives the oldest message from
   the first (in list order) of up to 16 endpoints that has one, with the
   same timeout and the same buffers as `ipc_receive`; it also returns the
   endpoint's index. It requires `READ` on all of them and that all of them
   are endpoints, checked before waiting.
2. **A global wait queue for multi-receivers** (`ANY_RECEIVERS`): each send
   wakes the endpoint's own receiver (`wake_one`) and all multi-receivers
   (`wake_all`), which recheck their endpoints. Spurious wakeups are the
   price of not registering a thread on several queues; with few services
   it is negligible and can be changed without touching the interface.
3. **fsd waits on all its endpoints at once** (`ipc_receive_any` with a
   one-second timeout) and dispatches by index; services that combine IRQ
   and IPC (input drivers) keep their loop.

## Rejected alternatives

- One endpoint per service with the namespace inside the message: breaks
  the capability model (the endpoint is the authority).
- Registering the thread on each endpoint's queue: correct, but requires
  cancelling registrations on wakeup and complicates `WaitQueue`; left for
  when measurements call for it.
- Threads per endpoint in fsd: there are no user threads, and it would
  multiply shared state.

## Consequences

- `ipc.receive_any` (program `ipc_any`) covers timeout, index, several
  messages, a handle without `READ` and a handle that is not an endpoint.
- `docs/performance.md` records the latency of `ipc fsd list` before and
  after: from milliseconds (dominated by the 10 ms wait) to tens of
  microseconds under TCG.

## Note (2026-10-23, ADR 0044)

Point 3's "services that combine IRQ and IPC keep their loop" is over: since
ABI 6 the set may also hold up to four interrupt lines (`WAIT`), and netd,
virtio-input and usb-input wait on their endpoints and their line at once,
until their next deadline. The shared queue of point 2 is gone: with
those services waiting on sets, every send woke all of them. A set's
waiter now registers on each endpoint's own queue (the alternative point
2 deferred, finding E of the performance audit of 731c398) and passes on
a wakeup it took for an endpoint it did not receive from.
