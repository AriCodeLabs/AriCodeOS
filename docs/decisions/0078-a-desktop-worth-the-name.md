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

### Audit

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
