# ADR 0085: Resizing windows by their edges and corners

- **Date:** 2026-11-09
- **Status:** Accepted. The owner tried the desktop on the reference laptop and
  missed resizing windows by dragging their edges and corners.

## Context

The desktop could already resize windows: stage 1 of ADR 0078 gave every window
whose program asks for it (`wants::RESIZE`) a band 6 px outside its frame. A drag
there sends the program `Configure`, the program paints a buffer of that size and
answers with `Resize`, and the window changes only then. The editor, files,
settings, launcher and terminal ask for it since stage 2.

The owner did not find it, and the code shows why:

- **Nothing showed it.** The pointer was the same arrow over the band as over the
  desktop. The band lies over the window's shadow, so nothing marked it either.
- **The corners were 6x6 squares** beyond the corner of the frame's rectangle. The
  frame's corners are rounded with a 10 px radius, so the visible corner was about
  9 px from that square. With a touchpad, that is a target that cannot be found.
- **The notch of a rounded corner**, inside the rectangle but outside the curve,
  showed the desktop and took clicks for the window: a click on what looks like
  the desktop went to the content's corner pixel.

Measured against the brief's other points:

- **A dragged edge could leave the screen.** Dragging the top into the bar put the
  title bar under it, and the compositor then pushed the window down. A window
  partly off the screen could be made wider than the screen.
- **Each pointer move asked the program for a size.** Queued requests merged, but
  a program that kept up still allocated, painted and handed over a buffer for each
  move: up to one per input packet, about 100 a second with a touchpad
  (incident 0058).
- **An answer to an older size** was put where the window already was. Dragging
  the left edge of a slow program made the right edge jump back and forth.
- **The log** printed a line for every `Resize`, so a program resizing itself in
  a loop filled it (left open by the audit of stage 3 of ADR 0078).
- **No host test** covered the exchange: it lived in the compositor's service.

## What others do and where they failed

- **Wayland (`xdg_shell`).** The compositor sends `xdg_toplevel.configure` with a
  size and `xdg_surface.configure` with a serial. The client answers
  `ack_configure(serial)` and commits a buffer of the new size. The compositor
  applies the new geometry with that commit, so the window never shows a
  half-painted size. During an interactive resize (`xdg_toplevel.resize` names the
  edges), mutter sends one configure per frame and waits for the acknowledgement
  before the next. Sway applies a layout only when every client has acknowledged,
  and gives up waiting after `txn_timeout`, 200 ms by default.
  - *Failures:* a client that never acknowledges keeps its old size, and in sway
    it delays every window of the transaction by the timeout. Clients that draw
    their own decorations resize them a frame late, so the frame and the content
    wobble apart. GTK's client-side shadow makes the resize area part of the
    client, which must get it right.
- **X11.** `_NET_WM_MOVERESIZE` asks the window manager for an interactive move or
  resize, with the edge coded 0 to 7. The window manager resizes the window and
  the client learns it from `ConfigureNotify`, then repaints on `Expose`.
  `WM_NORMAL_HINTS` carry the minimum size and the size increments.
  - *Failures:* the server shows the new size before the client paints it. The
    exposed area is the background or garbage, which is the classic resize
    flicker. `_NET_WM_SYNC_REQUEST`, with an XSync counter, lets the window manager
    wait for the client to draw before the next configure. Without it, a slow
    client gets a storm of `ConfigureNotify`. Window managers offered wireframe
    resizing (an outline, with the window resized once at the end) for slow
    machines.
- **macOS.** Any edge or corner resizes a window since 10.7. Earlier, only the
  grow box in the bottom-right corner did. The cursor shows the direction. The
  application redraws during live resize (`inLiveResize`), and
  `windowWillResize:toSize:` and `contentMinSize` constrain the size.
  - *Failures:* a hung application does not redraw, and its window cannot be
    resized. In Tahoe (26), about 75 % of the corner's resize zone lies outside the
    visible, more rounded corner (ADR 0078), so the visible corner does not resize.
- **Windows.** `WM_NCHITTEST` answers `HTLEFT`, `HTTOPLEFT` and the other sides,
  and the system runs the size loop. `WM_SIZING` lets the program adjust the
  rectangle while it is dragged, and `WM_GETMINMAXINFO` gives the limits. Since
  Windows 10, the resize border is about 8 px, mostly invisible, outside the
  window. The resize cursors show it.
  - *Failures:* `WM_ERASEBKGND` paints the background before `WM_PAINT`, which
    makes the content flicker. Under DWM, a growing window shows black or white
    strips until the program paints them. "Show window contents while dragging"
    can be turned off to get an outline instead. A program that does not answer
    for 5 s is replaced by a "ghost" window, which the system resizes alone.
- **GNOME (mutter)** grabs a band of `draggable-border-width`, 10 px by default,
  and shows the resize cursor for each side and corner.

All of them show the direction with the cursor. The ones that avoid flicker show a
size only once the program painted it. The ones that avoid storms keep one request
outstanding, and stop waiting for a program that does not answer after a timeout.

## Decision

1. **Where a window is grabbed (`aricode_gfx::Grips`).** A value, not constants
   spread in the code:
   - a band of 8 px outside the frame, as on Windows (mutter: 10 px);
   - each corner reaches 16 px along both of its sides (the radius and 6 px more),
     and never more than half a side, so a small window keeps four corners;
   - the notch a rounded corner leaves inside the frame's rectangle grabs that
     corner. On a window that cannot be resized, a click there goes to what shows
     there, under the window;
   - only windows whose program asked for `wants::RESIZE` have grips;
   - the band and the reach count for 256 px at most, whatever the value says.
2. **The cursor says what a press does.** Over a grip, and through the whole drag,
   the pointer is a double arrow: left-right, up-down, or one of the two diagonals
   (`aricode_gfx::Cursor`). The compositor draws them, white outlined in black like
   the arrow, in 15x15 px centred on the point. The scene's damage covers the old
   and the new cursor when either its place or its picture changes. The frame trace
   (`gui_trace`) names the picture.
3. **What a drag asks for (`Grips::resized`).**
   - The side opposite the one dragged stays where it is.
   - The content is never smaller than 160x80: room for a line and a button, and
     for the title bar's circles.
   - A dragged side stops at the screen's edge, and the title bar at the bar's.
   - The frame never grows larger than the screen under the bar. A window that is
     already larger only shrinks: grabbing its side does not make it jump.
4. **The exchange (`aricode_gfx::Placing`).**
   - The compositor sends `Configure`, the program paints a buffer of that size
     and answers with `Resize`. The window takes the new size, and the place asked
     with it, only then.
   - Meanwhile, the window shows its old content at its old size and place: never
     cut, stretched, black or half painted.
   - One request at a time. While one is waiting, the latest place the owner wants
     waits behind it, and goes when the answer comes. Any `Resize` counts as that
     answer, even of another size, as from a program that rounds sizes to its text
     cells. A drag asks for no more sizes than the program can paint.
   - **A program that does not answer** is asked again after 250 ms
     (`PATIENCE_MS`), for the latest place only, and then left alone until the
     owner wants another. It blocks nothing: the compositor never waits for it, and
     every other window moves and resizes as before. It keeps its size, and its
     title bar still moves it.
   - The last 4 places asked are kept. An answer to one of them, even a late one,
     takes the window to the place asked with that size, the way a Wayland serial
     does. So the right side stays still while the left one is dragged. An answer
     to a size not asked, such as one the program chose, leaves the window where
     it is. When the title bar moves the window, the places asked move with it, so
     a late answer does not take it back.
   - Going back to the size shown while another is asked moves the window at once,
     forgets the older requests, and asks the program for this size again once it
     answers. An answer of the size the owner wants now, before it is asked, also
     forgets the older requests: they cannot take the window back.
5. **Programs that do not resize keep working.** Without `wants::RESIZE` a window
   has no grips, gets no `Configure`, and zoom and snapping leave it as it is. The
   protocol is unchanged, so the 0.18 apps run as they did.
6. **Limits.**
   - Every size asked lies within the screen under the bar and `MAX_SIDE`, also when
     zoom or a snap restores an earlier place.
   - The program allocates each buffer from its own DMA quota, so the compositor
     allocates nothing for a resize.
   - `Resize` keeps the checks of `Create`: a refused size leaves the window as it
     was.
   - The log says "window N is now WxH" where an edge drag ends, not at each step.
     Sizes a program chooses for itself are said at most once a second for each
     window, so one program cannot hide another's.
7. **The apps.**
   - The editor, files, settings, launcher and terminal already ask for resizing
     and repaint at each size (stage 2 of ADR 0078). They gain the grips and
     cursors with no change.
   - Bench3d and Pista keep their size. Bench3d's figures are measured at its size,
     and Pista doubles a 480x270 picture: scaling either is not cheap.
8. **SDK.** No change: `Window::create_wanting(.., wants::RESIZE)`,
   `Event::Configure` and `Window::resize`, which paints the new buffer before
   handing it over.

## Consequences

- **Tests.**
  - Host tests in `libs/gfx/src/resize.rs` cover:
    - the grips: the band, the corners' reach, the notches, and a small window;
    - the drag's bounds and minimum, including a window partly off the screen;
    - the cursor of each grab, and the cursor shapes;
    - the exchange: one request at a time, the latest after the answer, a left
      edge that never jumps, a program that never answers, a late answer, going
      back, a size the program chose or rounded, and a title-bar move meanwhile.
  - The randomized test of damage against full composition now also changes the
    cursor's picture.
  - A new scenario, `gui.window_resize`, checks in QEMU that:
    - the cursor is a diagonal over a bottom-left corner grabbed along its side,
      and the drag moves both sides;
    - a program that never answers keeps its size, and is asked only a few times
      during a stroke of 40 moves;
    - the other window resizes after that;
    - a top edge dragged into the bar stops under it.
  - The runner gains `drag_steps`: a drag made of many moves, as a hand makes it.
- **Measured** in `gui.window_resize` (QEMU, 1280x800):
  - a stroke of 40 moves on a window that never answers asked it 3 times;
  - with a request for every move (a build broken on purpose) it asked 41 times.
  - Each of the scenario's three checks failed on a build broken on purpose: no
    corner reach (the cursor said `up-down` and the window became 400x320), a
    request for every move ("a storm"), and no screen bounds (440x356 instead of
    440x336).
  - The scenario takes 14 to 15 s with 10 scenarios at a time, as the other
    `full` scenarios of the area do.
  - The compositor's deepest stack path is unchanged (45.5 of 48 KiB).
- **The log** no longer grows with each step of a drag, which closes what the
  audit of stage 3 of ADR 0078 left open.
- **Not done.**
  - A program cannot declare its own minimum size (Wayland's `set_min_size`, X11's
    `WM_NORMAL_HINTS`, `WM_GETMINMAXINFO`, `contentMinSize`). All windows share
    160x80. Adding one is a protocol change, left for when an app needs it.
  - No outline for a program that does not answer: the cursor shows the drag, and
    the window stays as it is.
  - No resizing from the keyboard.
- **Seen, not changed here:** a change of focus repaints the whole screen. The bar,
  the dock and the window merge into one damage rectangle (1,024,000 px at
  1280x800 in the scenario's trace). It is correct, but it costs a full flip.

## Audit

An independent read-only review found no high findings: no client can make the
compositor allocate, loop or panic through this path, and none can reach another
window's requests. It found three medium findings and seven low ones, all fixed:

- **M1.** An answer of the size the owner had gone back to left older requests
  kept, and a later answer to one of them took the window to a size already left.
  Such an answer now forgets them.
- **M2.** A title-bar move was undone by a late answer, which took the window back
  to the place asked before. The places asked now move with the window.
- **M3.** Grabbing a side of a window larger than the screen made it jump to the
  screen's size at once. It now only shrinks.
- **L1.** An answer of another size than asked did not free the program for the
  next request, so such a program was asked at most 4 times a second, and a drag's
  end was never logged. Any answer frees it now.
- **L2.** One program resizing in a loop hid the size lines of every other. The
  limit is now per window.
- **L3.** Zoom and snap could restore a place larger than the screen. Every size
  asked is now within it.
- **L4.** A drag outlived its window, and the resize cursor stayed until the button
  came up. It ends with the window.
- **L5.** The notch of a window that cannot be resized still took clicks for it. A
  click there now goes to what shows there.
- **L6.** A band or reach of any size could overflow the frame's sums. They count
  for 256 px at most, and are unsigned.
- **L7.** A comment said queued requests always merge, and Demo F ended with 0 even
  after a storm. The comment now says when they merge, and a storm ends Demo F
  with 7.
