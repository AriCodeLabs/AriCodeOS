# ADR 0052: Compositor sessions: a window belongs to the client that made it

- **Date:** 2026-10-25
- **Status:** Accepted (failure-pattern audit 2026-10-25: C-M5, A-L3)

## Context

The compositor (ADR 0046, `WindowProtocol v1`) served one endpoint. init
gave every graphical program, through the broker's `window` capability or
`run_gui`, a send handle to that same endpoint. Window ids are small
counters, so any client could name another's window. The 2026-10-25 audit
found:

- any client could `Poll` another window's events (its keys, typed
  passwords included), `Present` it or `Close` it (incident 0037);
- the compositor could not tell when a client died, so a crashed
  program's window stayed on screen, kept the focus and the keys typed
  into it, and filled the table of 8 windows for good;
- `Create` mapped any memory object it was handed, an `Mmio` object
  included: the compositor then read device registers every frame, and
  the device joined the compositor's IOMMU domain.

## Decision

1. **The main endpoint makes sessions only.** A new `window::Op::Attach`
   on the compositor's main endpoint creates an endpoint pair; the
   compositor keeps the receive side, the reply transfers the send side
   (`sys::window::attach`) and the session's id (`generation << 8 | slot`,
   so a stale id never names a later session in the same slot).
   `Op::Detach` with that id ends the session. Everything else on the
   main endpoint answers `Invalid`, and `Attach` or `Detach` on a session
   answers `Invalid` too. Only init holds the main endpoint.

2. **One session per program.** The broker's `window` capability and
   `run_gui` hand each program the send side of a session of its own,
   narrowed to `WRITE | TRANSFER` (the rights it had before). At most 8
   sessions; when all are taken, `Attach` first retires the sessions whose
   clients are gone, and answers `Busy` (the launch fails) only if none
   is. A session has at most 2 windows open, so no client fills the
   table of 8.

3. **A window answers only its session.** `Create` records the session;
   `Present`, `Poll` and `Close` on a window of another session answer
   `BadWindow`, exactly as for a window that does not exist, so a client
   learns nothing about the others.

4. **A dead client's windows close.** Two paths, the first one to see it
   wins:
   - init tracks the process: when the broker (or `run_gui`) collects a
     program's exit, it sends `Detach`, and the compositor closes the
     session's endpoint (`endpoint_close`), which also cuts off any other
     holder of its send side. A client cannot keep its session alive past
     its exit by handing the send side away, for instance as the reply
     endpoint of a `Poll`; a launch refused after `Attach` detaches too;
   - every 100 ms the compositor asks each session whether its send side
     is gone (`PeerClosed` or `Revoked`).

   Then the session closes and so does every window it made: a pending
   `Poll` answers `BadWindow`, the buffer is unmapped and the screen under
   it repainted. The compositor says `client N gone: M window(s) closed`.

5. **No client starves the others.** The compositor receives on the main
   endpoint and every session at once (`ipc_receive_any`, which serves
   the first ready member); the sessions' order rotates every receive, so
   a client that floods its session takes at most its turn. A `Poll`
   waits at most 60 s, so a reply handle is not held for days.

6. **Window buffers are DMA buffers.** `Create` refuses (`BadHandles`) a
   buffer that is not a DMA object (`sys::dma_phys` fails).

## Consequences

- Applications do not change: `Window::create` takes the handle they
  are given, now a session. The protocol keeps its number; `Attach` is
  new and only init sends it.
- A client can still pass its session's send side to another program,
  which then shares its windows: delegation by the owner, as with any
  capability.
- A dead client's window lingers until init collects its exit, or at
  most 100 ms after its last send handle goes.
- Tested by `gui.two_windows`: Demo B cannot poll, present or close Demo
  A's window, nor attach or detach from its session, and leaves without
  closing its own, which the compositor closes when B is gone; the order
  of those steps is checked.
