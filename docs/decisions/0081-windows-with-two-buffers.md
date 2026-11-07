# ADR 0081: Windows with two buffers

- **Date:** 2026-11-07
- **Status:** Accepted, from incident 0063.

## Context

A window has one buffer, which its program draws on and the compositor reads
whenever it makes a frame of the screen (ADR 0046). The compositor makes at most
one frame each `FRAME_MS`, so a frame presented sooner is read later, while its
program may already be drawing the next one there: incident 0063 shows such
frames half drawn on the laptop and reproduces it in QEMU every time.

## What others do

- **Wayland** (`wl_surface.attach` and `commit`, `wl_buffer.release`): a client
  gives the compositor a buffer at each commit and must not touch it until the
  compositor releases it, so clients keep two or three.
- **X11 with Present**: `PresentPixmap` and an idle event for the pixmap given
  before; a compositing manager otherwise reads windows whenever it likes, which
  is why tearing inside windows was common on X.
- **Windows (DWM) and macOS**: a swap chain or layer per window, flipped by the
  system at its own frame; the program draws into a back buffer only.

All of them give the program a buffer the compositor does not read. They differ
in how a buffer comes back.

## Decision

1. **`SecondBuffer` (window protocol op 14)** gives a window a second buffer of
   its size, with the checks of `Create`, once. The window's buffers have fixed
   slots: 0 is that of `Create` or the last `Resize`, 1 the second. The compositor
   never reads the one not shown.
2. **`Present` names the slot to show**, in `width`: 0 keeps the one shown, 1 shows
   slot 0, 2 slot 1; anything else is refused. The answer's value is the slot
   shown. Named, not toggled: if an answer is lost, the program's next present
   names its slot again and learns from the answer which one shows, so nothing
   stays out of step for longer than one frame (audit M1).
3. **A slot change redraws the whole window**, whatever damage rectangles come
   with it: the buffer shown may differ anywhere from the one before (audit L1).
4. **The compositor handles messages between its frames**, never during one, so
   once it answers, it no longer reads the buffer that was shown, and the program
   may draw on it at once. No release event is needed with one compositor
   thread; one would be if the compositor ever read buffers from another thread.
5. **Refused**: a second buffer for a window that shares its buffer with another,
   a buffer some window already holds as a second one, and `Create` or `Resize`
   onto any buffer of a window with two, since a swap would change it under the
   other window (audit L2).
6. **A resize returns to one buffer**: the second had the old size.
7. **SDK:** `Window::double_buffer(hardware)`. From then on `canvas()` draws on
   the buffer not shown, which holds the frame before last; `present()` names it
   and takes the slot shown from the answer; `present_rects` sends the whole window.
   `present` keeps taking `&self`: the buffers live in `Cell`s, so no program had
   to change. `resize` asks for the second buffer again, of the new size, and
   `is_double()` says whether it got it (audit L4). `close()` lets the window's
   buffers go, which it did not even with one (audit L3).
8. **Optional.** A window that redraws only what changed keeps one buffer and its
   damage rectangles; the toolkit's windows do. A program that draws every frame
   whole, such as a game or bench3d, takes two.

## Consequences

- bench3d draws each frame in the buffer not shown and stops copying it: its
  extra heap shrinks by the frame it used to copy into (6144 to 4268 KiB), the
  quota stays at 8 MiB, and its run header says `present=double`. On a system
  whose compositor lacks the op it says so and runs with `present=single`, so
  such figures are not mistaken for the others.
- A program with two buffers uses twice the window's memory, from its own quota.
- The application interface level stays 1 here. Raising it, so that a package
  can require the op, is decided for the next release as a whole: `serve-update`
  and `publish-update` cannot yet set a package's `requires`.
- Regression: `gui.double_buffer`, which also resizes a window with two buffers,
  and its control `gui.double_buffer_control`, which must still see the fault on
  one buffer: without the control the first could pass without testing anything
  (audit M2).
- Audit: `docs/audits/2026-11-07-window-double-buffer.md`.
