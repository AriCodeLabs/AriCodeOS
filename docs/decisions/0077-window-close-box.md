# ADR 0077: The close box of a window

- **Date:** 2026-11-05
- **Status:** Accepted. The owner, testing 0.18 on the reference laptop,
  opened the editor and could not close it. No window had a way to close:
  each app closed only when its program chose to, and only the launcher
  had a button, which leaves the desktop.

## Context

- The window protocol (`aricode_proto::window`) had events for keys,
  clicks and focus. The compositor drew a title bar, but the bar did
  nothing.
- A program closes its window with `Op::Close`, or by ending: the
  compositor closes the windows of a session whose client is gone.
- A program may have reasons not to close at once: the editor with unsaved
  changes. And it may not be able to: it hangs, or it was written before
  there was any way to ask.

Desktops answer this the same way:
- X11's `WM_DELETE_WINDOW`, Wayland's `xdg_toplevel.close` and Windows'
  `WM_CLOSE` ask the program, which closes or asks the user.
- When the program does not answer, the window manager offers to end it
  (`_NET_WM_PING` in X11, "not responding" in Windows and GNOME).

## Decision

1. **A close box.** The right end of every title bar holds a 16-pixel box
   with `×` (`Window::close_box`). A click there is `Hit::Close`. The title
   text stops short of the box.
2. **The program is asked.** A left click on the box gives the window the
   focus and its program a new event, `Event::Close` (kind 4 in the
   reply). The program closes, or asks first. While one request waits
   untaken in the window's queue, more clicks add nothing.
3. **A program that never takes it.** If the request has waited in the
   queue for 2 s (`FORCE_CLOSE_MS`) and the box is clicked again, the
   compositor closes the window for the program. The program learns this
   at its next poll (`BadWindow`), and the compositor says so on the
   console. The test is "has it taken the event", not "has it closed":
   a program that reads its events and asks the owner first (unsaved
   changes) is never closed for it.
4. **The apps.**
   - **The editor** closes at once if nothing changed. Otherwise it says
     "unsaved changes: close again to discard them", and the second click
     discards them.
   - **The file manager and settings** close at once. Settings applies
     each change when it is chosen, and the file manager has nothing
     pending.
   - **The terminal** closes. Its shell ends by itself on its closed input
     (ADR 0074 phase C), and so do the programs it started.
   - **The launcher**'s close box leaves the desktop, as its "Leave the
     desktop" button does.

## Consequences

- **Older apps.** Version 1 of the desktop apps, from a 0.18 stick, does
  not decode the new event. Their poll fails, and they end as they do when
  the compositor goes. The window closes, abruptly. Version 2 of the apps
  handles the event.
- **Tests.**
  - The host tests check where the box is and that a click on it hits it.
  - A new scenario, `gui.close_box`, checks both cases. A window whose
    program reads its events is asked to close and closes. A window whose
    program does not read them is closed for it at the second click,
    2.5 s after the first.
  - A new runner option, `wait_ms` in `[host_input]`, pauses a stage after
    its marker.
- **Not done.** No title-bar drag (moving windows) and no minimize. They
  are separate decisions.
