# Audit: windows with two buffers (2026-11-07)

Independent, read-only audit of `ffe2977` on `fix/window-double-buffer` (ADR 0081,
incident 0063): the protocol op, the compositor's mappings and swap, the SDK, and
bench3d's use. It read the kernel's `map_foreign` and `unmap_foreign` as well.

No High finding: no use after unmap, no read outside a buffer, no reach into
another client's window, no way round the quota. Sound as checked: every path
that drops a window unmaps both its buffers through `unmap_unused`; a second
buffer is checked against the window's size, which only a resize changes, and a
resize drops it; the compositor reads windows' buffers only while composing and
never during a message; bench3d's budget (2 x 1876 + 4268 = 8020 of 8192 KiB).

| Finding | Fix |
|---|---|
| M1: `Present` toggled the buffers, so a lost answer left program and compositor out of step for good, and a lost resize answer made every present fail | `Present` names the slot to show and the answer says which shows; the SDK takes its step from the answer |
| M2: `gui.double_buffer` could pass without the deferred frame ever happening | `gui.double_buffer_control` runs the old way and must see the fault |
| L1: a swap kept the program's damage rectangles, leaving patches two frames old | a slot change redraws the whole window; `present_rects` sends the whole window with two buffers |
| L2: "no buffer of a window with two is shared" held only in `SecondBuffer` | `Create` and `Resize` refuse any buffer of a window with two; `SecondBuffer` once per window |
| L3: the SDK kept its mappings of closed windows' buffers | `close()` unmaps and closes them; a lost `SecondBuffer` answer is documented |
| L4: a resize went back to one buffer without saying so | `resize` asks for the second again; `is_double()` says whether it got it, and the scenario resizes |
| L5: bench3d without the op draws on the buffer shown | kept: labelled `present=single` with a warning, and ADR 0081 says such figures are not comparable |
| L6: an old SAFETY comment; unknown `Present` widths accepted | comment corrected; widths above 2 refused |

Checked after the fixes: `cargo xtask check`, unit tests, every `gui` scenario up
to release level, and both new scenarios: on one buffer the colour painted after
presenting shows in 60 of 60 deferred frames, on two in none of 140, across a
resize.
