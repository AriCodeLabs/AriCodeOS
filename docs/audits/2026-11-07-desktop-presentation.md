# Desktop presentation audit — 2026-11-07

Base: `34d916b` (`develop`). Implementation: `feature/desktop-icons`.
Scope: shell rendering, launcher layout, shared list presentation, identity and
damage tracking. This is a source and QEMU review, not a hardware qualification
or an independent security audit.

## Findings and resolution

| Finding | Resolution |
| --- | --- |
| The dock and application list identify programs by text alone. | A shared, built-in icon family for Applications, Terminal, Files, Editor, Settings and Pista; generic artwork for unknown packages. |
| All dock entries look similar, with minimized state conveyed only by text colour. | Icon plus label and a differently shaped active/visible/minimized mark. Existing click behaviour and slot order remain. |
| Long dock labels are cut at the slot boundary. | UTF-8-safe fitting with an ellipsis; drawing remains inside each slot. |
| The launcher's 320-pixel window gives its three action buttons and long hint little room. | A 400x420 default window, a clear heading, two action rows and a shorter keyboard hint. |
| An unsuccessful search leaves a blank list and an enabled Open button. | An explicit empty result message and disabled Open; the selected row remains absent. |
| The file list has no document symbols, and a long name can draw into its size column. | Shared document icons; name drawing stops before the detail column and scrollbar. |
| Panel damage compares titles and hidden state but not the trusted program name. | Program identity is included in panel invalidation, with a regression test. |

## Boundaries checked

- The manifest format and window protocol do not change. New packages are not
  given keys that released systems reject.
- Icons in compositor-owned panels come from the program identity init supplies.
  A window titled `terminal` without that identity still gets generic artwork.
- The toolkit retains the original row values. `Terminal` on screen still opens
  the package `terminal`, and filtering keeps its original activation indices.
- Icon painting allocates nothing, parses no data and caps its size at 64 pixels.
  Clip boundaries and partial/full paint equivalence are tested at tiny, normal
  and oversized requests.
- Changes to selection, scrolling, filtering and appearance are exercised with
  partial/full pixel comparisons. Launcher row spacing survives appearance changes.
- The work area reserves the taller dock. Resize and snap scenario expectations
  are updated from 696 to 664 pixels of full-height content at 1280x800.

## Validation

- `cargo xtask check`: passed (format, Clippy, dependency/license checks and
  stack budgets; 146 binaries measured, none over budget).
- `cargo test -p aricode-gfx -p aricode-ui`: 89 passed, 2 ignored benchmark/dump
  helpers. This includes icon clipping, UTF-8 ellipsis, trusted identity, list
  activation, damage equivalence and long-name/detail-column regressions.
- `cargo xtask test --all --area gui`: all host tests and 19 QEMU scenarios passed,
  including dock minimize/restore, resize, zoom, snap and keyboard handling.
- The seven slower scenarios `usb.desktop_launcher`, `usb.desktop_search`,
  `usb.desktop_files`, `usb.desktop_appearance`, `usb.desktop_resize`,
  `usb.desktop_terminal` and `usb.desktop_menus` passed, with the search scenario
  corrected and rerun as described below. There are 26 distinct passing QEMU
  scenarios across these runs.
- Final `git diff --check` and formatting checks passed.

Screenshots from these runs were visually inspected in both themes. PPM captures
and PNG copies are under `build/scenarios/` (generated artifacts, not committed):

- `usb.desktop_search/applications-current.png`: launcher with four application icons.
- `usb.desktop_search/icons-current.png`: terminal and two dock entries.
- `usb.desktop_appearance/dark-current.png`: dark windows, lists and dock.
- `usb.desktop_files/dialog-current.png`: document icon and modal confirmation.
- `gui.window_dock/dock-current.png`: minimized generic application in its dock slot.

During scenario preparation, the runner rejected a proposed search sequence
because it exceeded its 16-key-code queue limit. The sequence was shortened to
one nonmatching character; the empty-result and successful-search checks remain. The first QEMU run then
rejected `ctrl-a` as an unknown key code in that new sequence; the runner uses
`ctrl+a` for a chord. The guest had not received the search-clearing input.
The test input was corrected, and only that scenario was rerun.

## Remaining work

- Package-defined artwork and display names require a staged manifest change.
- The dock represents open windows; it does not pin or launch closed applications.
- Network status remains text. Battery and brightness need platform support;
  they are not simulated with decorative indicators.
- Scaling, animation, fractional time zones and daylight saving rules remain
  outside this presentation change, as recorded in ADR 0078.
- Real hardware and high-density displays have not been exercised in this audit.
