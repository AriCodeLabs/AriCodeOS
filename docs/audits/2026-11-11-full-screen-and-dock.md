# Audit: full screen and the dock's applications (2026-11-11)

Two independent read-only reviews on `feature/desktop-fit`: commit 78c2dc9 (full screen) and
the dock part of 1c86050 (the launcher's applications in the dock). Neither found a way for a
program to enter full screen, fill the dock or start anything without the owner's act: F11 and
Alt+Tab never reach a program, menus stay closed full screen, only the session init made for the
package `launcher` (the one that holds `launch`) gives the dock its list, and the broker still
checks everything the launcher starts.

## Full screen

| Finding | Fix |
|---|---|
| M1: a part presented while stretched left a column of stale pixels: the fixed-point blit can show the part's last source pixel one screen pixel past the exact ratio | the damage reaches a pixel further; a host test checks every screen pixel of three stretches |
| M2: with the bar hidden, a full-screen picture can look like the desktop, and the notice did not name the program | the notice names it; the pointer at the top edge shows it again |
| L1: a drag or a grab begun before going on against a window whose place changed | let go on entering and leaving |
| L2, L3: a closed or hidden window left full-screen state behind | cleared in every case |
| L4: a resize full screen at the same proportions repainted only the old frame | the whole screen is repainted |
| L5: a resizable program that never answers the size back stays at the screen's size | left: ADR 0085 lets a program keep its size |
| L6: no hung mark full screen | left: F11 still leaves |

## The dock's applications

| Finding | Fix |
|---|---|
| H1: once the last application closed, the launcher stayed in the dock with no window in sight, and the keyboard reached nothing (`usb.desktop_launcher` red) | the launcher comes back with the focus when the last window in sight closes |
| M1: a name the dock refused stopped the list part way, so a click could start another application than the one shown | the launcher keeps the list the dock took, skips a name it cannot hold, and answers a click from that list; on any other refusal the dock is emptied; capitals are accepted |
| L1: a second launcher session could receive the first one's clicks | the list belongs to the session that gave it, goes with it, and only its window hears a click |
| L2: a click queued while a refresh runs can name the neighbouring application | left: both lists change in the same step of the launcher; a list number in the event would close it |
| L3: a program minimizing itself while the owner held a button in it kept getting the pointer | what is held is let go |
| L4: two equal names get two places | left: the broker's names are unique |

Tests: `gui.full_screen` (stretched and resized, a mapped click, the notice, a program refused in
the dock), `usb.desktop_dock_apps` (a click opens an application, the launcher steps aside and
comes back), host tests for the layout, the names and the damage; 105 scenarios of the gui, usb
and apps areas at level release pass.
