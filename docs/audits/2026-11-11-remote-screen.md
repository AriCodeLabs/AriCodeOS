# Audit: remote screen (2026-11-11)

Scope: commit eaeee42 on `feature/remote-screen`: `libs/qoi`, the window protocol's
`Capture`, the launch flag `SCREEN`, init's `copy_screen`, the compositor's `capture`, the
shell's `screenshot`. Independent read-only review.

**Reach:** only the shells' endpoints reach `SCREEN`: the console's and SSH sessions' shells
(init's main broker endpoint, handed out only by `spawn_shell`) and the Terminal's (an endpoint
of its own). The launcher's endpoint refuses it, only init holds the compositor's main endpoint,
and window sessions get `Invalid`. Handles are closed or transferred once on every path. The
compositor accepts only a DMA buffer of the screen's size. The QOI encoder matches
specification 1.0. No high finding.

| Finding | Fix |
|---|---|
| M1: the documented recipe dropped every Base64 line containing `QOI`, corrupting most images | the recipe drops only the exact marker lines; the runner does the same |
| M2: output the SSH session could not take was dropped silently | the shell checks each write, stops, prints no end marker and says the image is incomplete; the last line gives length and SHA-256 |
| L1: a hung compositor could hold init 2 s a copy, repeatedly | 250 ms to queue, 1 s to answer, and one copy every 2 s (`Limit`) |
| L2: a buffer a window shows was not refused | refused (`BadHandles`) |
| L3: the log did not say which shell asked, and a session's `Capture` was refused silently | init names the console/SSH shell or the Terminal's; the compositor says the refusal once |
| L4: the Terminal can read the screen through its shell | written down in ADR 0078 and `docs/hardware.md` |
| L5: `ssh -t` adds carriage returns | the recipe strips them |
| L6: the request's length was never read; width 0 | the length is gone (`arg` must be 0); an empty screen is refused |

Tests: `gui.screenshot` now decodes the image whole in the runner (length, SHA-256, size,
every pixel) and checks the 2 s limit; `apps.launch` checks the launcher's refusal; QOI
decoder refusals have host tests. Not covered by a scenario: the Terminal's endpoint (the same
code as the console's, with another log line) and a window session's `Capture`.
