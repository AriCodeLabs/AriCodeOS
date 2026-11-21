# Incident 0063: frames shown half drawn

- Date: 2026-11-07
- Environment: the reference laptop (dev build 36, bench3d 1 to 5) and QEMU/OVMF
  (`gui.bench3d`, `gui_demo` modes 4 and 5).
- Work branch: `fix/window-double-buffer`, from `develop` at `dbffb94`.

## What was seen

The owner watched bench3d 1 on the laptop: "many models appeared partially, the
rendering was cut in zones". Drawn on the host frame by frame, every scene was
whole, so the renderer was not the cause.

bench3d 1 drew each frame straight into its window's buffer. bench3d 2 drew apart
and copied the finished frame in one pass (`53665df`): the cut models went away,
but a `gui.bench3d` capture of bench3d 5 kept a fractal torn by a straight
horizontal line, the new frame above it and the one before below.

## Cause

The compositor reads the buffer of each window whenever it makes a frame of the
screen (ADR 0046), and it makes at most one frame each `FRAME_MS` (16 ms). A
`Present` that comes sooner than that is left for later: the compositor waits
for its deadline and then reads the buffer, by which time the program is already
painting its next frame there. A copy of a finished frame narrows the window but
does not close it: the compositor can still read during the copy.

With one buffer per window no discipline in the program closes this: the program
cannot know when the compositor will read.

## Reproduction

`gui_demo` mode 5 presents two frames of one colour each in a row, so that the
second is left for later, then paints the buffer in a third colour (`0x00fe01`)
and pauses 30 ms, as a program starting its next frame does. The compositor's
frame trace samples the window's pixels on screen. On one processor, in QEMU:

| Mode | First colour | Second colour | Third colour |
|---|---|---|---|
| 5, one buffer | 59 | **0** | **60** |
| 4, two buffers | 59 | 60 | **0** |

On one buffer the second frame never reached the screen: each deferred frame of
the compositor showed what the program painted after presenting.

Two attempts did not reproduce it and are recorded so the test is not weakened
again. Frames with a 5 ms pause each got their own frame at once (the pause is
longer than `FRAME_MS` in QEMU). Frames without any pause kept the compositor
answering presents, each frame made just after an answer, before the program
painted. The deferred frame, made from the deadline while the program works, is
the case that shows it.

## Fix

ADR 0081: a window may have two buffers. The compositor reads only the one shown;
a `Present` with `width` 1 swaps them, and the compositor, which handles messages
between frames, never reads the other again until the next swap. The program
draws on the one not shown. bench3d uses it, and its own copy of each frame goes.

The regression is `gui.double_buffer` (mode 4): the third colour must never be
sampled.
