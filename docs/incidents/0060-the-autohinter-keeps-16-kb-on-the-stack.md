# Incident 0060: The font autohinter keeps 16 KB on the stack

- Opened: 2026-11-06. Status: accepted with a budget exception, open while the cause is
  in another project.
- Found by: `cargo xtask stack-check`, when the compositor began drawing title bars in Inter
  (ADR 0078, stage 1).

## Evidence

The compositor's deepest path reached 68 KB of its 64 KB stack, statically:

```
15264 compositor::main
 4832 Typesetter::draw
24192 Typesetter::render                     zeno's working data
 5656 skrifa::outline::OutlineGlyph::draw_dyn
 1552 autohint fill
16432 read_fonts::mem::with_scratch_dyn::on_stack::<16384>
```

## What was done

- zeno's 24 KB could not be avoided even with its `Scratch`, so coverage is now ours
  (`libs/text/src/raster.rs`), checked against zeno on every glyph: mean difference 0.44 of 255.
- The compositor's window contents moved to the heap: 6 KB less.
- What remains is read-fonts 0.46's scratch: blocks up to 16 KB go on the stack, and skrifa's
  autohinter offers no way to give it memory. So the compositor has an exception of 48 KB of
  path, 44.4 KB measured, used only while a glyph not yet cached is rendered.

## To close it

A read-fonts or skrifa version that takes a caller's buffer for this, or a change proposed to
them. Then the exception goes.
