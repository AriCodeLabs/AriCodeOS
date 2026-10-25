# Incident 0037: any graphical program could read or close another's window

- Opened: 2026-10-25. Status: fixed on `fix/compositor-ownership` (ADR 0052).
- Found by: the failure-pattern audit of 2026-10-25 (audit C, M5; audit
  A, L3: authority checked by a name the caller chooses). Not seen on a
  machine.
- Environment: boots with `gui` and programs launched through the broker
  with `caps = window`.

## Evidence

Reading `services/compositor/src/main.rs` at `df15084`: one endpoint for
every client; `Present`, `Poll` and `Close` look the window up by
`req.window` alone, and ids count up from 1. A window's `Content` had no
owner, and nothing in the loop noticed a client exiting. `Create` passed
any handle to `mmio_map`.

## Mechanism

The compositor trusted the window id as the authority over a window,
and the id is guessable. Every client shared one endpoint, so the
compositor had no way to tell the callers apart, nor to notice one dying.
A program with `caps = window` could:

- `Poll` the focused window of another program and take its keys;
- `Close` the other windows, or leave its own on screen after crashing,
  holding the focus and one of the 8 slots for good;
- hand an `Mmio` object as a window buffer.

## Fix

ADR 0052: one session endpoint per program, a window answers only the
session that created it (`BadWindow` otherwise), the windows of a session
whose client is gone close, and `Create` takes DMA buffers only.
`gui.two_windows` checks the first two.
