#!/usr/bin/env python3
"""Regenera libs/fb-console/src/font.rs a partir de un PSF1 8x16 (Terminus)."""
import gzip, sys
src = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/consolefonts/Lat15-Terminus16.psf.gz"
d = gzip.open(src).read()
assert d[:2] == b"\x36\x04" and d[3] == 16, "se espera PSF1 con glifos de 16 filas"
glyphs = d[4:4 + 256 * 16]
lines = ["//! Fuente de consola 8x16, glifos ASCII 32..=126.\n//!\n",
         "//! Extraída de `Lat15-Terminus16.psf` (Terminus Font, (c) 2010 Dimitar Toshkov\n",
         "//! Zhekov), licencia SIL Open Font License 1.1: ver `FONT-LICENSE-OFL.txt`.\n",
         "//! Generado por `tools/gen-font.py`; no editar a mano.\n\n",
         "pub const GLYPH_WIDTH: usize = 8;\npub const GLYPH_HEIGHT: usize = 16;\n",
         "pub const FIRST_CHAR: u8 = 32;\npub const LAST_CHAR: u8 = 126;\n\n",
         "/// Una fila por byte, bit 7 = píxel izquierdo.\n",
         "pub const GLYPHS: [[u8; GLYPH_HEIGHT]; (LAST_CHAR - FIRST_CHAR + 1) as usize] = [\n"]
for c in range(32, 127):
    rows = ", ".join("0x%02X" % b for b in glyphs[c * 16:(c + 1) * 16])
    name = "espacio" if c == 32 else ("backslash" if c == 0x5C else chr(c))
    lines.append("    [%s], // %s\n" % (rows, name))
lines.append("];\n")
open("libs/fb-console/src/font.rs", "w").write("".join(lines))
