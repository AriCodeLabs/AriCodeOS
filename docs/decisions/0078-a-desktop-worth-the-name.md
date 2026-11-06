# ADR 0078: A desktop worth the name

- **Date:** 2026-11-05
- **Status:** Accepted on 2026-11-05. That day the owner judged the 0.18 desktop
  "very basic" and asked for a desktop with good UX. It should take the
  best of the existing ones, drop their faults and save resources. The
  owner chose:
  - a toolkit of our own, "even if it costs more, because we can improve
    and optimize it";
  - a macOS-like style: typography done right, the style of the windows,
    space used to the pixel, "neither cramped nor separated by tedious
    lines", correct type sizes, and coherence.

  The four points in "The owner's choices" were decided on the same day.

## Context

Three independent studies, made on 2026-11-05:

1. **How the veterans do it.**
   - Apple's Human Interface Guidelines for macOS: the type scale, the
     controls, the colours and the materials.
   - Haiku's app_server and Interface Kit.
   - GNOME (libadwaita) and Wayland, Windows 11 Fluent, Redox Orbital and
     OrbTk, and SerenityOS.
   - Seven open fonts, with their metrics measured on the files.
2. **The Rust crates for each layer.** Each one was built `no_std` and
   linked like our programs, then measured for time on the host, code
   size, heap and licence. This was done in a scratch harness outside the
   repository.
3. **This system's code**: what the compositor, the window protocol, the
   memory model, the screen and the input give today.

### What the system lacks today

Most blocking first:

1. **The window protocol.**
   - There is no resize, and no move from the pointer: `move_window`
     exists, but nothing reaches it.
   - It does not deliver motion, button release, the wheel or hover. The
     wheel is decoded, then dropped in the compositor.
   - It does not deliver keycodes or modifiers either.
   - Each window holds 8 queued events.
2. **Memory.**
   - The DMA quota per program is cumulative and never refunded, and at
     most 8 MiB. A window buffer is one contiguous DMA allocation.
   - There is no shared memory between processes, so each program must
     carry its own copy of a font.
   - A 1920×1080 buffer is 7.9 MiB: 92 KiB under the limit.
3. **Fonts.** The only font is the 8×16 bitmap Terminus. There is no
   vector font and no rasterizer.
4. **Composition.**
   - Window contents are blitted opaque, with no alpha.
   - The framebuffer is mapped uncached (`cacheable: false`) and written
     one volatile store per pixel. It has never been measured
     (docs/performance.md).
   - There is no vsync.
5. **The screen.**
   - AriBoot keeps whatever GOP mode the firmware left. The reference
     laptop reports 800×600 (docs/hardware.md), not its panel's native
     mode.
   - A screen over 8 MiB fails (2560×1440).
   - The GOP `Bitmask` format is treated as BGR.
6. **Limits.** 8 windows, 2 per session, 24-byte titles, and a fixed
   cascade placement.
7. **Input.** No absolute pointer (tablet) and no software key repeat.
8. **The widgets.** Their layout is in fixed pixels, they assume 8 px per
   character, and there are no scroll views.
9. **Services.** No clipboard, notifications, preferences store or time
   zone, and no battery: ACPI is read as static tables, without AML. The
   manifests have no icons or display names.

### What the veterans teach

| | Take | Avoid |
|---|---|---|
| **macOS** | One type scale with fixed leading, and tracking that changes with the size. Controls 20 to 28 px. Regions told apart by tint and space, not lines. Every toolbar command also in a menu. Motion that can be turned off | Corner radii that vary between windows and releases: in Tahoe (26) about 75 % of the resize zone lies outside the visible window. Unhinted text that looks soft at 1x. Live blur |
| **Haiku** | One server and one toolkit, so every program looks alike. Damage handled in update sessions: nothing repainted twice | No composition at all, so no shadows |
| **GNOME / Wayland** | The header bar (title and toolbar in one). Clients that cannot see each other's windows or input | Client-side decorations: frames differ, and a hung program's close button hangs with it. Hidden settings. A single-threaded JavaScript shell |
| **Windows 11** | Snap layouts. Mica: the wallpaper sampled once, a tint at almost no cost | Several generations of interface side by side. Advertising |
| **Redox OrbTk, SerenityOS** | SerenityOS: one toolkit, one window server, layouts in markup | OrbTk retired, overtaken by toolkits with more hands. SerenityOS slowed when its founder left. Lesson: a small toolkit, owned, tested from the first day |

### What the crates measured

Host timings, Ryzen 7 5800X; the code sizes are of a static `no_std`
binary linked like ours. Our desktop programs are 28 to 93 KiB today.

| Layer | Candidates | Finding |
|---|---|---|
| Glyphs | fontdue 0.9.5, ab_glyph 0.2.32 (+ ttf-parser 0.25), swash 0.2.10, skrifa 0.49 (+ zeno) | **ab_glyph: 77 KiB, 6 crates**, glyphs drawn when first asked for. fontdue decodes every glyph at load: 18.6 MB of heap for DejaVu, rejected. skrifa, which Chrome uses instead of FreeType since version 133: the only one with hinting, 758 KiB, and a 16 KiB stack scratch that breaks our frame budget unless given a heap buffer. Unhinted, all four give the same coverage quality |
| Shaping | rustybuzz 0.20 (archived), harfrust 0.15, cosmic-text, parley | harfrust: 4.9 µs per line but 959 KiB. Latin-1 needs only pair kerning (AV, To); ligatures are optional |
| Shapes | tiny-skia 0.12, vello_cpu 0.3, raqote (std only) | tiny-skia: 256 KiB, mature (resvg), 0.40 ms for a window frame at 1280×800. vello_cpu is faster but 858 KiB and young; an uncached blurred shadow costs it 10.9 ms |
| Layout | taffy 0.15, morphorm | taffy: 324 KiB, 91 µs for 221 nodes |
| Toolkits (to learn from) | iced, egui, xilem, floem, slint | iced's software compositor redraws only damaged rectangles: the model. egui repaints everything on any input, which is wrong for software rendering. Slint's licence is refused (GPL-3.0 or own terms) |

All the accepted licences are on the `ACCEPTED` list of `tools/xtask/src/deps.rs`.

## Decision

### Look: the tokens

These start the toolkit. 1 px is one Apple point: the 11.6-inch MacBook
Air ran macOS at 1366×768 at 1x.

- **Type, size/leading in px:**

  | Style | Size/leading | Weight |
  |---|---|---|
  | Large Title | 26/32 | regular |
  | Title 1 | 22/26 | regular |
  | Title 2 | 17/22 | regular |
  | Title 3 | 15/20 | regular |
  | Headline | 13/16 | semibold |
  | Body | 13/16 | regular |
  | Callout | 12/15 | regular |
  | Subheadline | 11/14 | regular |

  Nothing goes under 11 px at 1x. Tracking follows Inter's own formula.
  Figures are tabular (`tnum`) in clocks and tables.
- **Fonts:** **Inter 4.1** for the interface and **JetBrains Mono 2.304 NL**
  for the terminal, both SIL OFL 1.1, as Terminus is today. Their
  x-heights match: 0.546 and 0.550 em.
- **Space:**
  - a 4 px unit;
  - 20 px window margins, 8 px between the controls of a group, 20 px
    between groups;
  - controls 22 px (28 large, 18 small);
  - list rows 24 px, sidebar rows 28 px, a sidebar of 220 px.
- **No lines.** Regions are told apart by tint and space:
  - a tinted sidebar beside a white content area;
  - groups on 8 px rounded cards against the window's background.

  A separator, black at 10 %, is the exception.
- **Windows.**
  - The frame is drawn by the compositor: a trusted title, and a hung
    program can still be closed (ADR 0077).
  - The title bar is 28 px, with three 12 px circles 20 px apart (close,
    minimize, zoom).
  - The radius is 10 px everywhere and does not vary; controls use 6 px.
  - The resize band is a fixed 6 px, outside a square frame.
  - The shadow is blurred once per size and drawn in nine slices: 24 px,
    black at 30 %; 12 px and 15 % when inactive.
  - A program can ask for a unified toolbar by declaring its height. The
    compositor then draws the title and the circles inside it.
- **Colour**, light and dark:
  - window background #ECECEC and #323232;
  - content #FFFFFF and #1E1E1E;
  - labels in black and white at 85, 50, 25 and 10 %.

  The accent is the "increased contrast" blue #1E6EF4, which keeps white
  text at 4.6:1; plain macOS blue reaches only 3.5:1.
- **Focus:** a 3 px ring outside the control, following its radius, in
  the accent at 50 %.
- **Motion:** 200 ms ease-out, only the position and opacity of cached
  window images, never a layout per frame. A switch turns it off.

### Building it: what we adopt, what we write

Following "compare, then reuse or improve" (ADR 0063):

1. **Glyphs: adopt skrifa** (fontations) with zeno, with light vertical
   hinting: baselines and x-heights fall on whole pixels. This is the
   owner's choice, made for sharpness on 1x screens.
   - skrifa is the only candidate with hinting. It is the rasterizer
     Chrome uses instead of FreeType, it is `forbid(unsafe_code)`, and it
     is fuzzed.
   - It is given a heap buffer (`DrawMemory::Buffer`), so its 16 KiB
     stack scratch stays inside our frame budget (`cargo xtask stack`).
   - Its API changes with every 0.x release, so it is pinned and updated
     on purpose.
   - ab_glyph (77 KiB, unhinted) is the fallback if skrifa fails on
     size, stack or speed.
   - Our own glyph atlas, keyed by font, size and glyph, with quarter-pixel
     positions. Blending is gamma-correct with slight stem darkening, 4
     pixels per SSE2 instruction.
2. **Shaping: our own**, for Latin-1 and Latin Extended-A, on
   ttf-parser's OpenType tables: cmap, GPOS pair kerning and an optional
   `liga`. Host tests check it glyph for glyph against harfrust, which
   stays the reference and is not shipped (it would cost about 1 MiB per
   program).
3. **Shapes: our own analytic ones** for rectangles, rounded rectangles,
   circles, gradients and the shadow masks. Pixel diffs check them
   against tiny-skia. tiny-skia is adopted for general paths (icons), and
   vello_cpu is the challenger once it matures.
4. **Layout: our own stacks** (horizontal, vertical, spacers, alignment),
   measured against taffy.
5. **Toolkit (`libs/ui`): retained widgets**, as in xilem and iced's
   software path, not immediate mode.
   - Each widget marks the rectangle it dirtied, which feeds
     `aricode_gfx::Damage`.
   - It has no asynchronous runtime.
   - Themes are the tokens above, light and dark.
6. **Composition stays in software.** There is no GPU driver, and the
   design does not need one.
   - The compositor writes to a cached back buffer and copies only damaged
     rectangles to a framebuffer mapped write-combining (PAT).
   - Windows declare opaque regions, and nothing they cover is drawn.
   - Only the corners and shadows are blended. Shadows are cached, and
     blur is never live.
   - A software cursor with save-under.
   - A frame comes at most every 16 ms, and frames are dropped, not
     queued.

### Stages

Each stage is several branches, each audited where the rules ask for an
audit. Each stage ends usable on the laptop.

0. **Foundations**, measured first:
   - write-combining framebuffer, then measure a flip;
   - AriBoot chooses the panel's native GOP mode, and handles `Bitmask`;
   - the DMA quota refunded when a buffer is freed, so resizing and
     double buffering do not exhaust it;
   - window protocol 6, keeping the 0.18 apps' events: configure and
     resize, pointer motion and release, enter and leave, the wheel, key
     codes with modifiers, damage rectangles in `Present`;
   - software key repeat;
   - `perf` gains `ui.*` figures: glyph cold and warm, a line, a full
     repaint, a button, a shadow, a layout.
1. **Windows and text:**
   - the macOS frame of the compositor, with its circles, shadow and
     corners;
   - moving, resizing, zoom, and snapping to halves and quarters;
   - Alt+Tab;
   - Inter and JetBrains Mono, with the atlas and our shaper;
   - the terminal moves to JetBrains Mono.
2. **The toolkit** (`libs/ui`):
   - controls with keyboard focus, text with selection, scroll views and
     lists, menus and dialogs, light and dark;
   - a clipboard service with its own capability;
   - the five apps rewritten on it.
3. **The shell:**
   - a menu bar at the top, with the clock, network and the active
     program's menus;
   - a dock with the running programs;
   - a launcher that searches as one types;
   - notifications, display names and icons in the manifests;
   - a time zone in settings.
4. **Polish:**
   - cached animations;
   - 2x scale for dense screens;
   - two-finger scroll on the touchpad;
   - battery and brightness (ACPI AML: a project of its own).

### What it costs in memory

There is no sharing between processes, so each program carries its own
fonts and code:
- three weights of Inter subset to Latin, about 0.3 MiB;
- JetBrains Mono for the terminal;
- skrifa and zeno, about 0.75 MiB;
- the toolkit, 0.2 to 0.4 MiB.

With the six programs of the launcher's pool, that is a few MiB of the
laptop's 4 GiB. Read-only memory shared between processes would remove
the copies; it is a kernel change, left for when it is measured to
matter.

## The owner's choices (2026-11-05)

1. **Menus:** a global menu bar at the top, as in macOS, for the active
   program, with the clock and status.
2. **Text:** light vertical hinting with skrifa, sharper than macOS at
   1x, at about 0.7 MiB more per program.
3. **Theme:** light by default; dark is chosen in settings.
4. **Fonts:** Inter and JetBrains Mono.

## Consequences

- The 0.18 desktop keeps working through stage 0. The protocol keeps the
  old events, and every app moves to the toolkit in stage 2.
- New crate families enter the build: skrifa (with read-fonts and
  font-types), zeno, ttf-parser for the OpenType layout tables, tiny-skia,
  and their dependencies. Each is pinned and on the licence
  list. The audit of each stage covers them.
- Two font files enter the repository under OFL 1.1, with their notices,
  as Terminus did.

## Stage 0: results so far

### Write-combining

Measured on the reference laptop, full flip at 800x600: 43.6 ms uncached,
0.86 ms write-combining. That figure predates the fence the audit asked for,
so it is measured again with the native mode.

### The native mode

AriBoot's choice works, but the reference laptop's firmware does not offer
the panel's mode. `diag` there says:

```
screen  800x600, the firmware's mode; the panel prefers 1366x768
modes   1024x768 800x600*
```

The EDID reads correctly; the GOP lists two 4:3 modes on a 16:9 panel. The
owner chose a driver built from Intel's manuals over any BIOS change, because
a BIOS setting helps one machine: ADR 0079, to come.

### The DMA quota

The quota bounds live buffers now: a buffer's charge goes back with its frames,
without a lock, whoever held it last. A closed buffer whose frames a mapping
keeps stays charged. `memory.dma_lifetime` checks it: 32 allocations and frees
within a quota of two pages.

An independent review of the refund found no high findings. It moved the frees
of `handle_close` and a failed `dma_alloc` out of the process table's lock, and
asked for a case with a charged buffer outliving its handle: step 5 of the self
test.

### The window protocol and key repeat

The wire format grew compatibly, so the protocol number stays and the 0.18 apps
run unchanged:
- `Create` carries what the window wants (`wants::POINTER`, `wants::KEY_CODES`).
  A program built before sends zero there and gets exactly the events it knows.
- With them: `Motion`, `Release`, `Enter`, `Leave`, `Wheel` and `KeyCode`. A button
  pressed on a window holds the pointer's events until it is released, wherever
  the pointer goes. Queued motion and wheel events merge, so a program that falls
  behind keeps its keys.
- `Present` can name up to 5 rectangles; none means all of it.
- `Resize` changes the content's buffer. `Configure`, the compositor asking a
  program to resize, comes in stage 1 with the frame that needs it.

Key repeat is in software: 500 ms, then every 33 ms, the same on every keyboard.
A USB keyboard sends a key once, and a PS/2 one repeats by itself, at its own
rate; both are now ignored in favour of this. The console does not use it yet.

`gui.window_events` checks all of this through QEMU's mouse and keyboard, with new
runner options `drag` and `hold_key`.

An independent review of the protocol found no high findings, but found that
`gui.window_events` passed without its last stage (incident 0059). All fixed:
- `Enter`, `Leave`, `Motion` and `Wheel` are about the content, not the frame.
- A PS/2 keyboard repeats a held key as a new press. Its driver now says it is a
  repeat, so a held Shift floods nothing and Caps does not toggle.
- The driver's queue gives up motion before keys: a lost release would repeat a
  key for ever. The compositor's queues keep releases and focus over motion.
- While a button pressed on a window is held, every button goes to that window,
  and a program hears the release of what it saw pressed.
- A driver's lost releases also clear the buttons, the grab and the escape chord.
- A change of focus tells the old window its keys went up, and the new one never
  hears the release of a key pressed elsewhere.
- A held dead key does not repeat; a resize keeps the close box on screen; damage
  on a window showing the same buffer at another width repaints all of it.

Known limit: if `Resize`'s answer is lost in a timeout, the compositor may hold the
new buffer while the program draws on the old one, until its next resize.

### Still open in stage 0

The `ui.*` figures of `perf` measure what stage 1 brings (glyphs, shapes, layout),
so they come with it.

### Audit of the first three items

An independent read-only review found no high findings. All of these are
fixed:

- **M1.** AriBoot compared whole `ModeInfo` values, including fields UEFI
  leaves undefined, and could fail to find the current mode. It compares
  size, stride and format.
- **L2.** `IA32_PAT` was written bare. It now follows the SDM's procedure
  (caches off and flushed, TLB flushed) and checks CPUID. Without PAT, the
  framebuffer stays uncached and the log says so.
- **L3.** The kernel's physical map aliased the write-combining framebuffer
  as write-back. The range now has the same type there (`Mapper::map_linear`),
  on 4 KiB pages inside its 2 MiB windows.
- **L4.** The display's own BAR aliased it as uncached. A BAR holding the
  framebuffer is now write-combining, and `map_foreign` refuses a second
  mapping of a range with another type.
- **L5.** `diag` claimed "is not offered" after a failed set, and "no EDID"
  for an invalid one. Both are worded for what is known.
- **L6.** `BootInfo::video` read the field before checking `size`.
- **L7.** A chosen mode past the 32 listed had no `*`. It takes the last slot.
- **L8.** A malformed `video=` was ignored silently. AriBoot logs it and `diag`
  reports it (`gui.video_malformed`, and `gui.video_refused` for a size not
  offered).
- **L9.** Stride 0 or under the width, and absurd sizes, could count as usable.
  An interlaced first timing could count as a panel size.
- **L10.** No fence followed a flip, so the last bursts could wait in the CPU's
  buffers, and the measurement ended early. `flip` ends with `sfence`.
- **L11.** `SCREEN_MAX_BYTES` and `DMA_MAX_LEN` are now tied by a compile-time
  check. A misplaced doc comment went home.
- **`Bitmask`** (this stage's own item): a firmware-left mask mode was drawn as
  BGR. The compositor now encodes through the masks (`PixelMasks`, host-tested),
  and refuses a screen of under 32 bits per pixel instead of writing past it.
  The kernel console scales channels wider than 8 bits too.

**Not tested in QEMU:** the path without PAT, and a `Bitmask` screen, since OVMF
offers none and scenarios cannot change the CPU model. Host tests cover the masks.

## Stage 1: results so far

### The frame and handling windows

The compositor draws the frame of the look above: a 28 px title bar with its three
circles on the left and the title centred, 10 px corners with smoothed edges, and the
shadow from one table, blurred once at build time and used in slices. Colours and the
shadow's strength are a `Theme`, not constants.

The owner handles windows without the program's help:
- the title bar moves one;
- the three circles close, minimize and zoom;
- a title bar dropped at the screen's edge snaps: sides to halves, corners to quarters,
  the top to all of it;
- Alt+Tab brings the next one forward, in the order they had when Alt went down, even a
  minimized one.

A program that asks (`wants::RESIZE`) can be resized by a band 6 px outside its frame,
and zoomed or snapped to another size: it gets `Configure`, paints a buffer of that size
and answers with `Resize`, and the window changes only then, so it is never shown cut.
While the owner drags, only the latest size counts. The 0.18 apps do not ask yet, so they
keep their size until they move to the toolkit (stage 2).

`gui.window_manager` checks all of it, in QEMU.

`Present` now takes effect where a window is when the frame is made, not where it was at
that moment. Before, a window moved away and back within one frame kept its old pixels:
the host test of damage against full composition found it, once frames could move.

An independent review found no high findings. All fixed:
- A size asked of a busy program could be applied after the owner had gone back. A place
  now remembers its size, and only that answer takes it there.
- Minimizing told the window nothing, and lost its keys' releases.
- A window that opened without the focus could block Alt+Tab and being raised by a click.
- A resize could show a black buffer: `Window::resize` now paints first.
- A window closed during Alt+Tab, a click on a title bar near the top, and an empty
  window list no longer misbehave; a minimized window Tab goes past is minimized again.
- Sizes asked of a program stay within what it may answer; a hidden window's Present costs
  nothing; a queue keeps `Configure`; the shadow skips what the frame covers and fills
  the corners' notches.

**Known limit:** a minimized window has no way back without a keyboard until the dock
(stage 3). The compositor's handling logic has scenarios but no host tests: it lives in the
service, and moving it into `libs/gfx` is for when it grows.

### Text

`libs/text` draws Inter 4.1 and JetBrains Mono NL 2.304, subset to Latin (49, 50 and
24 KB; `libs/text/fonts/`, SIL OFL 1.1), with skrifa's outlines and light hinting.
Title bars are Inter semibold 13 and the terminal is JetBrains Mono 13. Apps move to it
with the toolkit (stage 2).

- **Coverage is ours,** not zeno's. zeno keeps 24 KB of working data on the stack even when
  given the heap, in a program with 64 KB. Ours is the signed-area method of font-rs and
  fontdue: about 100 lines, nothing on the stack. A host test checks it against zeno on every
  glyph of the three fonts at four sizes: mean difference 0.44 of 255, worst 24.
- **Spacing** is the font's advances and its GPOS kerning, on ttf-parser. A host test checks
  line widths against harfrust, which is not shipped. Ligatures and the rest of OpenType wait
  until something needs them.
- **Stack:** read-fonts keeps the autohinter's 16 KB scratch on the stack, with no way to give
  it the heap. The compositor has an exception of 48 KB of path for this (`cargo xtask
  stack-check`), used only while a glyph not yet cached is rendered. The terminal does the
  same work, but that check measures initrd programs, not packages.
- **Cost:** the stripped compositor is 598 KB. A screen of mixed text fits the cache in 16 KB.
- **Not done yet:** blending is in sRGB with a slight lift of the coverage instead of the ADR's
  gamma-correct blend, and 4 pixels per SSE2 instruction. To be measured before it grows.

## Stage 2: results so far

### The toolkit

`libs/ui` holds retained widgets in stacks, with keyboard focus, in about 1900 lines and no
`dyn`. A program builds its `Ui` once, gives it the window's events, reads back `Action`s and
presents what `paint` answers. The launcher, the settings, the files and the editor run on it,
and all four resize: their fixed coordinates are gone. The old `aricode_gfx::widgets` went
with them.

- **Controls:** labels and paragraphs, buttons, one-line fields and multi-line areas with a
  selection, lists with a second column, scroll views with a thumb that drags, and the Tab
  order, with a focus ring and a default button.
- **Damage:** a change marks its area, and painting walks the whole tree clipped to the marks.
  No widget has a second way of drawing itself, so a partial paint is the full paint's pixels by
  construction. A host test checks that after every kind of event, and it found that rounded
  corners blended twice when nothing cleaned below them.
- **Tokens:** a widget names a role, never a value. Space, type and colour are one `Theme` value
  a program can change or replace, which is the owner's rule against hardcoding. Light and dark
  both exist; choosing one in settings is still to come.
- **Long text:** an area lays out only the lines that show, so a key costs the same in a 64 KiB
  file. Lines are not wrapped: a long one scrolls sideways with the caret.
- **The quota:** a resize holds two buffers for a moment, so the apps ask for 8 MiB, which holds
  a 1366x768 window going to any smaller one. It is a limit, not what they use: a buffer as
  large as the screen costs 4.1 MiB only while it is that large. A refused size leaves the
  window as it was, and says so where a program can.

**Changed for the owner:** the editor opens plain text only. The old one kept bytes that were
not UTF-8, shown as `?`; on Strings they would not come back as they were, so such a file is
now refused and left alone (`is not plain text: not opened`), as is one with control characters.
CRLF and tabs stay.

**Tab now walks every control,** as on other systems, instead of going between a list and a
field. Shift+Tab goes back, and Esc returns to the list or the name.

The terminal stays on its text grid, and keeps its text across sizes (`Console::resize_keeping`).

### The clipboard

The ADR planned a service with its own capability. It became two requests of the window session
the compositor already serves, because that needs no new authority and closes more:

- A window copies only while it has the focus.
- A window reads only with the focus, and once for each Ctrl+V the owner pressed in it. What the
  owner copied may be a password: no program can watch the clipboard from behind, or take it
  unasked while it has the focus. The first refusal of each window is logged.
- 64 KiB of UTF-8 at most, kept outside the compositor's heap, in pages that cost nothing until
  something is copied. Text coming in is copied once and checked before it replaces what was there.

The price: only the keyboard opens it. A Paste menu item will need its own way to show the owner
asked, and the terminal has no copy and paste yet, since Ctrl+C and Ctrl+V are the shell's.

### Light and dark

Settings chooses, and init keeps it in the file `appearance`. The compositor changes the frames
and tells each window that asked, at once, and a new window learns it in the answer to `Create`,
so its first picture is already right. `usb.desktop_appearance` checks it across a reboot.

### The audit of this stage

The clipboard's rules held. What it found was in the programs, and it was worth having:

- **High, holding a key answered questions:** holding Delete in the files armed and then deleted,
  and the refresh selected the next file, so it emptied `home/` at about 15 files a second. Holding
  Enter on "Leave the desktop" left. The old programs had this too. The toolkit now knows a repeat
  from a press: a repeat edits and moves, and opens, presses, confirms and shortcuts nothing.
- **High, typing on a focused Open:** two spaces discarded unsaved text. A two-press question now
  ends at anything else the owner does (`Ui::inputs`), not only at some of it. Two spaces in a row
  on that button still answer it, and modal dialogs will end that.
- **Medium:** Ctrl+X took the text before the clipboard had it (now it waits); a long line stopped
  drawing at 160 glyphs (the limit is gone, and lines past 32767 px measure right); a status line
  moved a view the owner had scrolled, without painting it.
- **Low:** a CRLF is one line end for the caret; a held Ctrl+V reads once; refusals are logged per
  reason; a paste that does not fit says so.

**Still to do in this stage:** menus and dialogs. `ui.*` figures in `perf` are not measured yet.

Next: those, then the shell (stage 3).
