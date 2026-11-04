# ADR 0076: Text beyond ASCII: the Spanish keyboard as Linux types it

- **Date:** 2026-11-05
- **Status:** Accepted. The owner, testing 0.18 on the reference laptop
  (2026-11-04), found that ñ, ç and the accents gave nothing. Their
  instruction: look at how Linux does it and do the same, instead of
  guessing.

## Context

Until now every layer stopped at printable ASCII:
- **The keyboard.** `aricode_input::Keyboard` with `kbd=es` gave nothing
  for ñ, ç, º, ¡, ¿, ·, €, ¬ or the accents. It had no dead keys, and some
  AltGr keys were missing.
- **The console font.** It held the ASCII glyphs only, and the console
  showed `?` for anything else.
- **The text widgets.** `TextField` and `TextArea` stored one byte per
  character and refused anything else.
- **The decoder.** `term::KeyDecoder` gave `Key::Char(u8)`, a single byte.
- **The shell's line editor.** It moved one byte at a time.

### What Linux does

The facts come from what Linux installs on the build host. Nothing is
copied from it:
- **The layout.** xkeyboard-config's `symbols/es` builds on
  `latin(type4)` and `latin`. It gives four levels per key: plain, Shift,
  AltGr and Shift+AltGr. Among them:
  - the dead keys `` ` `` and `^` right of P, and `´` and `¨` right of ñ;
  - the dead `~` on AltGr+ñ, and the dead `¸` on AltGr+¡;
  - `{` and `}` both on AltGr+´ and AltGr+ç, and on AltGr+7 and AltGr+0;
  - `€` on AltGr+e, `¬` on AltGr+6, `½` on AltGr+5, `æ` on AltGr+a, and so
    on.
- **The dead keys.** `locale/en_US.UTF-8/Compose` combines a dead key with
  the next key: `´` and `a` give `á`, a dead key and a space give `'`,
  `` ` ``, `^`, `"` or `~`, and the same dead key twice gives the accent.
  The Linux console (`kbd`) behaves the same, and before a key that does
  not combine it writes the accent, then the key.
- **The font.** The console font `Lat15-Terminus16`, which ours is already
  taken from, carries Latin-1, the euro sign and more, with a Unicode
  table.

The laptop sends the codes Linux expects for these keys. A diagnostic
build that recorded each key in the event ring showed ç = 43, ´ = 40 and
AltGr = 100, the evdev codes `symbols/es` names.

## Decision

1. **The keyboard types UTF-8.**
   - The Spanish layout is a table of four levels per key
     (`libs/input/src/es.rs`), written in our own code from the facts
     above.
   - The dead keys ``` ` ^ ´ ¨ ~ ¸ ``` combine with the next key into a
     Latin-1 letter, as the `Compose` file does. They give `' ` ^ " ~ ¸`
     with a space, and the accent itself when pressed twice. Before a key
     they do not combine with they give the accent, then the key, as the
     Linux console does. An editing key, Ctrl, or a lost release ends a
     dead key unused.
   - The other dead keys of the layout, on the fourth level (caron, breve,
     ogonek, ...), give nothing.
   - `KeyBytes` holds two characters of UTF-8: an accent that did not
     combine and a three-byte character such as €.
   - `tools/check-keymap.py` compares the table with Linux's, all four
     levels of every key and the dead-key rules that give Latin-1. Run on
     this host, it reports no difference.
2. **What is shown: Latin-1 and the euro sign.**
   - The font has a glyph for each cell code: printable ASCII and Latin-1
     are their own value, and the euro sign is 0x80, a C1 control never
     shown (`tools/gen-font.py`, from the font's Unicode table).
   - The console's cells stay one byte each.
   - Anything else is shown as `?`, but is kept as typed.
3. **Characters, not bytes.**
   - `Key::Char` carries a `char`. `KeyDecoder` gathers its UTF-8 and drops
     an invalid or cut character, as well as controls.
   - `TextField`, `TextArea` and the shell's line editor store UTF-8. The
     caret moves, deletes and cuts one character at a time. A character
     takes one cell, so the console's backspaces still move the cursor
     right.
   - Bytes of a file that are not UTF-8 are kept and shown as `?`.
4. **File names stay ASCII.** AriFS names, and `valid_plain_name`, are
   unchanged. The editor and the file manager now say so in their message
   ("no accents or ñ").

## Consequences

- **Tests.** Host tests cover the table, the dead keys, the decoder, the
  widgets and the line editor. Three QEMU scenarios are new:
  - `console.ps2_es_accents`: the kernel's PS/2 console receives `ñé€`;
  - `usb.desktop_editor_es`: the editor, on a PS/2 machine, saves
    `ñá€'`;
  - `shell.utf8_text`: a line with ñ, accents and € is edited, saved and
    shown back.
- **Over SSH**, the host's terminal sends UTF-8 already. The line editor
  now moves and deletes by character there too.
- **The US layout** does not change.
- **Not done.** Scripts beyond Latin-1, and right-to-left or combining
  characters, would need a larger font and wider cells.
