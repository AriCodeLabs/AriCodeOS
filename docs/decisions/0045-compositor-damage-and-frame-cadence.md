# ADR 0045: The compositor flushes damage at a frame cadence

- **Date:** 2026-10-23
- **Status:** Accepted (performance audit of 731c398, finding C and
  implementation order 4; amends `WindowProtocol v1`'s `Present`)

## Context

The compositor kept one `dirty` flag. Any change (a pointer move, a
`Present`, a focus change, a window opened or closed) recomposed the whole
desktop into the back buffer and `flip` wrote every pixel of it to the
framebuffer with volatile stores: 2,073,600 pixels (8.3 MB) at 1920x1080
for a one-pixel pointer move that changes 80 pixels. Every frame also
printed a frame line and one pixel sample per window over COM1, which
`write_byte` drives by busy-waiting at 115200 baud, in ordinary use as
much as in the scenarios that read those lines. Input events composed
one frame each, however close together they came.

## Decision

1. **Damage, not a flag.** `aricode-gfx` gains `Scene` (what a frame
   depends on besides the window contents: windows with geometry and
   title from back to front, focus, pointer) and `Damage` (at most
   `MAX_DAMAGE = 8` disjoint rectangles inside the screen). The compositor
   keeps the scene on screen and each frame diffs the current scene
   against it (`Scene::damage`): both cursor rectangles when the pointer
   moved; old and new frame of a window that appeared, went away, moved,
   was retitled or gained or lost the focus; the overlap of two windows
   whose stacking swapped. Diffing snapshots instead of marking each
   mutation means no change to the desktop model can be forgotten.
2. **Merging is bounded and only adds pixels.** A rectangle overlapping
   one already kept is merged into their bounding box; with eight kept,
   it is merged with the one whose bounding box grows least; past half of
   the screen (`FULL_SCREEN_FRACTION`) the damage becomes the whole
   screen. The set always covers everything added since it was cleared.
3. **Compose and flush only the damage.** `Desktop::compose_region`
   repaints one rectangle over the previous back buffer; `compose` is the
   whole-screen case of it. Drawing is confined: `Canvas` carries a clip
   rectangle (`Canvas::clipped`), each window draws only inside its frame
   (a title longer than the frame is now cut at its edge) and the content
   callback gets a canvas clipped to the content, the cursor only inside
   `cursor_rect`. `flip` writes only the damaged rectangles, with the same
   volatile stores and pixel conversion. The back buffer stays a complete
   frame.
4. **`Present` is what makes content appear.** A client's buffer is read
   when its window's content rectangle is damaged; `Present` damages it
   (and that of every window on the same buffer, ADR 0038). Pixels painted
   and not presented may stay off screen until the next `Present`;
   before, any recomposition picked them up by accident. The SDK's
   `Window::present` already said "asks the compositor to repaint the
   content"; the protocol does not change.
5. **A frame cadence.** At most one frame per `FRAME_MS = 16` ms (about
   60 per second): changes that arrive sooner accumulate in the damage and
   go out together. An event waits at most one frame; the loop's 1 ms
   device reads bound the rest.
6. **Frame diagnostics are a test key.** The frame line (now with the
   pixels and rectangles flushed) and the window samples are printed only
   with `gui_trace` on the command line, a `TEST_ONLY` key that `cargo
   xtask usb-image` refuses. `gui.two_windows` sets it; `gui.frames_untraced`
   checks that without it no frame line or sample reaches the serial.

## Measured

Deterministic counts from the `aricode-gfx` host test
`flushed_pixels_per_event`, which also checks the result against a
whole-screen composition; 4 bytes per pixel:

| Event | Before (any resolution: whole screen) | After |
|---|---:|---:|
| Pointer moves 1 px | 786,432 px / 3,145,728 B at 1024x768; 2,073,600 px / 8,294,400 B at 1920x1080 | 247 px / 988 B (13x19: both cursor positions) |
| A 400x300 window presents | same | 120,000 px / 480,000 B |
| A 400x300 window moves 10 px | same | 138,276 px / 553,104 B (old and new frame) |

Of the 247 pixels, 80 really change. In the guest, `gui.two_windows`
(1280x800, `gui_trace`) flushes 1,024,000 pixels for its first frame,
130,896 for a window opening (its 404x324 frame), 120,000 for a
`Present`, 456 for a 20-pixel pointer move (two separate cursor
rectangles) and 130,896 for the focus change of its click; before, each
of those frames wrote 1,024,000. No FPS, composition time or latency was
measured on hardware.

## Rejected alternatives

- **Explicit damage at each mutation** (`raise`, `create`, ... return the
  rectangles). Every new mutation would have to remember it; a forgotten
  one leaves stale pixels that only show on screen. The scene diff costs
  a few hundred bytes copied per loop.
- **Exact regions (lists of spans).** Unbounded without `alloc`; eight
  rectangles cover the cursor, a couple of windows and a move, and fall
  back to the whole screen beyond that.
- **SIMD or non-volatile copies for the flush.** The audit asked to
  measure bytes first; volatile MMIO semantics stay.

## Consequences

- `aricode-gfx` host tests compose random event sequences (create,
  close, raise, move, pointer moves small and large, presented content,
  focus loss, several events per frame) both ways and require identical
  back buffer and framebuffer, pixel for pixel, after every frame.
- `Desktop::move_window` exists in the model and is covered by those
  tests, but the compositor offers no way to move a window yet.
- The compositor still reads its input services one after another with
  1 ms reads (ADR 0044, "Consequences").
