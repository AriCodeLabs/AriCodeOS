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
3. **A program that does not answer.** The compositor waits for the
   program to come back:
   - A request counts as answered once the program polls again with no
     request left queued. Until then, more clicks add nothing.
   - If the program has not come back for 2 s (`FORCE_CLOSE_MS`), the next
     click on the box marks the window: its title ends with
     "(not responding)".
   - The click after that ends the program's session. All its windows
     close, and its next call fails, so it ends when it can. The
     compositor says so on the console.
   - A program that answers is never closed for it, including one that
     asks the owner first about unsaved changes.
   - The owner always sees the mark before the click that ends the
     session. A program that was only busy, for example on a slow disk,
     gets that warning first.
4. **The apps.**
   - **The editor** closes at once if nothing changed. Otherwise it says
     "unsaved changes: close again to discard them", and the second click
     discards them.
   - **The file manager and settings** close at once. Settings applies
     each change when it is chosen, and the file manager has nothing
     pending.
   - **The terminal** closes. Its shell ends by itself on its closed input
     (ADR 0074 phase C), and so do the programs it started.
   - **The launcher**: its close box and its "Leave the desktop" button
     both leave the desktop, and both ask first, because every app it
     started closes with it.

## Consequences

- **Older apps.** Version 1 of the desktop apps, from a 0.18 stick, does
  not decode the new event. Their poll fails, and they end as they do when
  the compositor goes. The window closes, abruptly. Version 2 of the apps
  handles the event.
- **Tests.**
  - The host tests check where the box is and that a click on it hits it.
  - A new scenario, `gui.close_box`, checks both cases. A window whose
    program reads its events is asked to close and closes. A window whose
    program does not read them is marked at the second click, 2.5 s after
    the first, and its session ends at the third.
  - A new runner option, `wait_ms` in `[host_input]`, pauses a stage after
    its marker.
- **Not done.**
  - No title-bar drag (moving windows) and no minimize. They are separate
    decisions.
  - A program hung for good keeps its process and its launcher slot until
    the desktop ends. The compositor can end its session but cannot stop
    it. Stopping it would need init to learn from the compositor which
    session ended.

## Audit

An independent read-only review found no high findings: no session reaches
another's window, and no buffer is unmapped twice. It found three medium
findings and seven low ones, all fixed:

- **M1.** The launcher's box left the desktop at one click, and the text
  of an editor it had started was lost with it. Leaving now asks first:
  "again to leave the desktop: open apps close with it".
- **M2.** A forced close only removed the window. The process kept its
  session, and a program that was merely busy for 2 s lost its work. Now
  a window is first marked "(not responding)", and only the click after
  that ends the session.
- **M3.** A full event queue dropped its oldest event, which could be the
  pending request, and the 2 s started over. A request to close is never
  dropped now.
- **L4.** A request that answered a poll directly was not timed. Now the
  time starts when the request is handed over, whether queued or not.
- **L5.** After a close, no window had the focus, and keys went nowhere
  until a click. The front window now takes it.
- **L6.** In the editor, the confirmation stayed armed through other
  actions. Anything but a second click on the box disarms it.
- **L7.** On a very narrow window the box covered the whole title bar.
  Such a window now has no box. Right and middle clicks on the box are
  clicks on the window again.
- **L8.** `Event` is now `#[non_exhaustive]`, so a future kind does not
  break the build of programs outside the tree.
- **L9.** `gui_demo` closes its window and ends when asked.
- **L10.** The scenarios now cover more cases:
  - the editor asks about unsaved changes, then closes
    (`usb.desktop_editor_es`);
  - the launcher asks, then its box leaves the desktop
    (`usb.desktop_launcher`);
  - a hung window is marked, then its session is ended (`gui.close_box`).
  - Not tested: the terminal's box. It returns from its loop through the
    same path as a closed window.
