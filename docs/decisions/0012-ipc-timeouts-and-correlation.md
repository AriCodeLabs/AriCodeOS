# ADR 0012: Deadlines, correlation and retries in SDK IPC calls

- **Date:** 2026-09-28
- **Status:** Accepted

## Context

The SDK clients (block, files, network, windows, input, broker)
reused one reply endpoint per client and waited without a deadline. A
reply that arrived after an expired deadline stayed on that endpoint and
was consumed by the next request; a dead server left the client
waiting forever. The 2026-09-28 study (stage 1B) calls for a uniform
contract for failures, deadlines and retries.

## Decision

1. **One reply endpoint per request** (`aricode_sys::rpc::call`): the
   client creates the endpoint, sends its duplicate with the request, waits for the
   reply and closes it. A late reply reaches a closed endpoint
   and is lost; it is never attributed to another request. The
   protocols do not need to change: the channel provides the correlation, not an identifier.
2. **Every wait is bounded**: send and receive carry a deadline. Each
   client chooses its own (the requested deadline plus a margin for the service); only
   the launch broker waits without a deadline for the program's exit, because
   that wait is the result.
3. **An expired deadline undoes nothing**: the result of that request is
   unknown (it may have executed). The SDK does not retry writes on its
   own; the caller decides with that information. Side-effect-free reads and queries
   can be retried.
4. **Dead server**: the client gets `Timeout`; with the supervisor
   (ADR 0011) the endpoint stays valid and the next generation
   serves new requests. Superseded in part by ADR 0030: a request the
   dead server held, or one queued on an endpoint with no receiver left,
   ends at once with `PeerClosed`; `Timeout` remains for a server that is
   alive and slow.

## Consequences

- Costs two more syscalls per call (create and close the endpoint), which
  the IPC measurements (`ipc.ping_pong`) keep bounded.
- `test=ipc_late` (`ipc.late_reply`) demonstrates discarding the late
  reply and the correlation; the services scenarios cover deadlines
  with dead servers.
