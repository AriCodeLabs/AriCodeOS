# Deeper desktop presentation audit — 2026-11-07

Reviewed change: `ddb6da2`, integrated at `2f08e14`. This follows
[the first presentation audit](2026-11-07-desktop-presentation.md).
Review and corrections are local to `fix/desktop-icons-audit`; this report does
not claim an independent review or hardware qualification.

## Findings

### M1 — a secondary column can displace the filename

`libs/ui/src/lib.rs`, the List paint branch, reserved all of a detail string's
width before drawing the name. Icons reduced the remaining space further. A
small window could show a size with only a sliver of the filename; at still
smaller widths the secondary text could reach the icon.

Confirmed by a failing host regression before the correction. The renderer now
omits the optional detail column if it cannot also keep useful space for the
name. This changes presentation only; `row()` still returns the original value.

### M2 — a snapped launcher hides its actions

The launcher's new heading, separate action rows and taller application rows
increased its natural height. At 800x600, a quarter-screen snap gives 400x218
pixels of content. With four applications, the new layout needs 301 pixels;
the previous layout needed 217. The root stack does not shrink its children,
so Open/Refresh were cut and Leave was outside the viewport.

Confirmed with the unmodified launcher in `usb.desktop_compact`: both input
stages ran, the compositor reported `400x218`, but clicking the place where
Leave should have been visible did not open the confirmation. The screenshot
shows the controls clipped. Shift+Tab had moved focus to the invisible Leave
button; testing Enter alone would have missed the defect.

The launcher now wraps its existing layout in the toolkit's scroll view. The
layout at its normal size is unchanged; small windows can scroll, and keyboard
focus reveals the control. The regression runs the actual installed launcher
and checks the pointer action after focus, not a duplicate layout fixture.

## Checks beyond the initial audit

- Traced package identity from init's session through `set_program`, panel text
  and icon selection. Window titles cannot select a built-in icon.
- Checked filtering against the stored list values, empty selection, disabled
  Open, held Enter, modal input routing and appearance changes. No additional
  launch-authority path or empty-result activation was found.
- Checked icon drawing loops: strokes use fixed, nonzero-length segments and
  size is capped at 64. No heap allocation, image decoding or package-controlled
  paths were introduced.
- Verified the actual Inter Semibold font's character map contains U+2026, so
  the compositor's ellipsis is supported by the shipped font.
- Reviewed scene invalidation for identity, focus, hiding, insertion and removal.
  Theme changes are explicitly repainted by the compositor.
- Added icon pixel tests for negative/offscreen positions, odd and nonsquare
  sizes, padded strides, the 64-pixel cap and two-dimensional tiled repaints.
- Added full/partial desktop comparisons through all built-in icon identities,
  minimize/restore, focus changes and removing/reusing a slot without changing
  the window count, in both themes. A quiet frame must repaint zero pixels.

No high-severity finding was identified in this scope. This is not a claim
that the whole desktop or its dependencies are free of defects.

## Validation

- `cargo test -p aricode-ui -p aricode-gfx`: 92 passed, 2 existing ignored
  benchmark/dump helpers. The narrow-column test failed before the correction.
- `cargo xtask check`: passed, including formatting, Clippy, dependency/licence
  checks and stack measurement of 146 binaries, none over budget.
- `cargo xtask test --only ...`: all host tests and 26 QEMU scenarios passed.
  The selection comprises every non-release GUI-area scenario (19), plus
  `usb.desktop_compact`, `usb.desktop_launcher`, `usb.desktop_search`,
  `usb.desktop_files`, `usb.desktop_appearance`, `usb.desktop_resize` and
  `usb.desktop_menus` (7). No scenario was dropped after a failure.
- `usb.desktop_compact` failed with the original launcher and passed after the
  scroll-view correction. In both runs the two input stages actually ran.
  The corrected serial order includes `window 1 is now 400x218` followed by
  `launcher: asks to leave the desktop`.
- Final whitespace and formatting checks passed.

The before/after QEMU captures were visually inspected. Generated evidence is in
`build/desktop-deep-audit/compact-before.png`, `compact-after.png`,
`compact-before.log` and `compact-after.log` (not committed). The corrected
capture shows the focused Leave control fully inside the short viewport.

The failing-before evidence and mechanism are also recorded in
[incident 0062](../incidents/0062-desktop-presentation-at-small-sizes.md).

## Limits

Both confirmed findings are corrected locally, with regressions. Changes have
not been committed, merged or deployed by this audit. Validation covers the
affected GUI paths, not the full release matrix, all possible window sizes or
real hardware. Manifest-provided artwork, scaling and platform features remain
outside this change as in ADR 0078.

## Visual follow-up: dock proportions

The owner pointed out that the dock was too large. This was a gap in both
reviews: interaction and rendering correctness had been checked, but the
proportions of the 96x64 entries had not been assessed with several applications.

The compact dock uses 44x40 entries, 28-pixel icons and names on hover. Its painted
height is 48 pixels instead of 72; eight entries occupy 388 pixels horizontally
at 800x600. The active/minimized state marks and click behaviour remain.

The hover bubble's old and new rectangles participate in scene damage. Tests
compare partial/full repaints when entering, crossing entries, changing a
hovered identity and leaving. Labels stay on-screen, and the empty frame still
requires no repaint. The work area gains 24 vertical pixels; the compact launcher
scenario now has 400x230 content, while its original failing-before evidence
above remains the 400x218 run.

Visual-follow-up validation passed: 93 graphics/toolkit tests, `cargo xtask check`
(146 binaries within their stack budgets), and 28 QEMU scenarios. The latter are
the previous 26 plus `usb.desktop_dock_compact` and `usb.desktop_terminal`, whose
maximized size also changes with the dock.

The screenshots were visually inspected:

- `build/scenarios/usb.desktop_dock_compact/five-apps.png`: five running apps in
  a 244x48 dock, down from the former 504x72 for five entries.
- `build/scenarios/gui.window_dock/dock-name.png`: the hovered name above the
  icon, with the same restore/minimize behaviour checked by the scenario.

These are generated artifacts. The changes remain local and uncommitted.
