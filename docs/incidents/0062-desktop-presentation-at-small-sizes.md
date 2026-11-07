# Incident 0062: desktop presentation at small sizes

- Date: 2026-11-07
- Environment: host graphics tests and QEMU/OVMF, desktop changes in `ddb6da2`,
  integrated into `develop` by `2f08e14`.
- Work branch: `fix/desktop-icons-audit`, from `stream/desktop` at `347a972`.

## Evidence and cause

The deeper review requested after the first presentation audit found that the
new optional list icon consumes part of the name column while the secondary
column always retains its full width. The old correction clipped the name at
that column's left edge, but did not reserve any minimum space for the name.

The regression `narrow_icon_lists_preserve_the_name_instead_of_squeezing_it_out`
uses a real-looking filename and a seven-digit size, both themes, and list widths
80, 120 and 160. Before the fix it fails with:

```text
a secondary size column displaced the name at 80px
```

The same failing regression initially stopped the QEMU runner's host-test phase;
no guest had started in that run. The column fix was then applied before testing
the independent compact-launcher case.

The launcher has more fixed-height controls than before but still places them in
an unscrolled root. Its list's minimum height is three rows when four applications
are offered. A quarter of an 800x600 screen gives it 400x218 pixels of content;
the layout requires more. The `usb.desktop_compact` reproduction checks that
Shift+Tab brings the Leave control into view and that a click at its visible
position actually opens the leave confirmation.

Before the launcher correction, QEMU reported both input stages sent (10 and 4
events), `compositor: window 1 is now 400x218`, and normal shutdown, but no
`launcher: asks to leave the desktop`. The scenario failed its ordered expectation.
`build/desktop-deep-audit/compact-before.png` shows Open/Refresh clipped at the
bottom, with Leave entirely outside the viewport. The new layout needs 301 pixels
with four applications; the pre-icon layout needed 217.

## Mechanism review

- Geometry: the list renderer must allocate space before drawing both columns;
  clipping alone cannot make an obscured name readable. A stack distributes spare
  space but does not shrink children below their measured sizes.
- Input: changing a row's displayed columns must not change its value or selected
  index. A focused button below the viewport still takes keys, so keyboard
  activation alone cannot demonstrate that the launcher is usable at small sizes.
- Rendering: both fixes must preserve partial/full repaint equivalence and the
  theme. The compact scenario must exercise the actual package and compositor,
  rather than a second copy of their layout.

## Resolution and validation

Secondary columns are omitted when they cannot leave room for the name (up to
the theme's field minimum). Row data and activation indices are unchanged.
The launcher wraps its existing root in a scroll view so its controls remain
reachable after a small snap and focus can bring them into view.
The compact-launcher reproduction and final validation are recorded in the
[deeper audit](../audits/2026-11-07-desktop-deep-audit.md).

**Resolved locally, integration pending:** the column regression passes, and the
same QEMU compact-launcher sequence now prints `launcher: asks to leave the
desktop` after the pointer click. All 92 graphics/toolkit tests and the selected
26 QEMU scenarios pass; `cargo xtask check` passes. No timeout increase or retry
was needed. The fix has not yet been committed, so there is no fix commit to cite.
