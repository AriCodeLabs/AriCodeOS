// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Text on a canvas (ADR 0078): Inter for the interface and JetBrains Mono for the
//! terminal, drawn with skrifa's outlines and light hinting, which keeps baselines
//! and x-heights on whole pixels, and our own coverage (`raster`). Spacing comes from the font's
//! own kerning. Each glyph is rendered once per size and quarter-pixel position, and
//! kept. Everything here is a parameter of the caller: fonts, sizes and colours.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use aricode_gfx::{Canvas, Color};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, HintingInstance, HintingOptions, OutlineGlyphCollection, OutlinePen,
    SmoothMode, Target,
};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};

pub mod raster;
use raster::Outline;

/// The fonts in the system, subset to Latin (`fonts/`, under the SIL OFL 1.1).
pub const INTER_REGULAR: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
pub const INTER_SEMIBOLD: &[u8] = include_bytes!("../fonts/Inter-SemiBold.ttf");
pub const JETBRAINS_MONO: &[u8] = include_bytes!("../fonts/JetBrainsMonoNL-Regular.ttf");

/// Quarter-pixel positions: enough that spacing stays even, few enough to cache.
const SUBPIXELS: i32 = 4;
/// What the cache may hold, in bytes of coverage, before it starts over. The title
/// bars of a desktop and a screen of text fit well inside this.
const CACHE_BYTES: usize = 96 * 1024;
/// No glyph is rendered larger than this, whatever a size asks for.
const GLYPH_LIMIT: u32 = 256;

/// A font, by the bytes of its file.
#[derive(Clone, Copy)]
pub struct Face(pub &'static [u8]);

/// One font at one size. `size` in pixels.
struct Sized {
    face: usize,
    size: u32,
    hinting: Option<HintingInstance>,
}

struct Glyph {
    face: usize,
    size: u32,
    id: u32,
    sub: u8,
    /// Where its top-left pixel is, from the pen, and its size.
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// The lightest pass at gamma: coverage is lifted a little, since blending in sRGB
/// thins dark text on light. Slight stem darkening, in the ADR's words.
fn lift(c: u8) -> u8 {
    let c = c as u32;
    (c + (((255 - c) * c * 48) >> 16)) as u8
}

/// The outline in our rasterizer, y going down.
struct Pen(Outline);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, -y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to(cx, -cy, x, -y);
    }
    fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) {
        self.0.curve_to((a, -b), (c, -d), x, -y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

/// What a line of text measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Pixels from the baseline up and down, rounded out.
    pub ascent: i32,
    pub descent: i32,
}

/// Draws text, and keeps what it rendered.
#[derive(Default)]
pub struct Typesetter {
    sizes: Vec<Sized>,
    glyphs: Vec<Glyph>,
    bytes: usize,
}

fn hinting_options() -> HintingOptions {
    HintingOptions {
        engine: Engine::Auto(None),
        target: Target::Smooth {
            mode: SmoothMode::Light,
            symmetric_rendering: true,
            preserve_linear_metrics: true,
        },
    }
}

impl Typesetter {
    pub const fn new() -> Self {
        Self {
            sizes: Vec::new(),
            glyphs: Vec::new(),
            bytes: 0,
        }
    }

    pub fn metrics(&self, face: Face, size: u32) -> Metrics {
        let Ok(font) = FontRef::new(face.0) else {
            return Metrics {
                ascent: size as i32,
                descent: 0,
            };
        };
        let m = font.metrics(Size::new(size as f32), LocationRef::default());
        Metrics {
            ascent: libm_ceil(m.ascent),
            descent: libm_ceil(-m.descent),
        }
    }

    /// The width of `text` in pixels, with its kerning.
    pub fn width(&self, face: Face, size: u32, text: &str) -> i32 {
        let mut x = 0;
        layout(face, size, text, |_, at| x = at);
        ((x + 0xFFFF) >> 16) as i32
    }

    /// Walks `text` once: `at` gets each character's byte offset and where it starts in
    /// pixels, rounded, then `text.len()` and where the line ends. What a caret or a
    /// wrapped line needs, without measuring every prefix.
    pub fn offsets(&self, face: Face, size: u32, text: &str, mut at: impl FnMut(usize, i32)) {
        let mut chars = text.char_indices();
        layout(face, size, text, |glyph, x| {
            let offset = match glyph {
                Some(_) => chars.next().map_or(text.len(), |(i, _)| i),
                None => text.len(),
            };
            at(offset, ((x + 0x8000) >> 16) as i32);
        });
    }

    /// Draws `text` with its baseline at `at.1`, starting at `at.0`, in `color`, within
    /// `canvas`'s clip.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        (face, size): (Face, u32),
        text: &str,
        (x, y): (i32, i32),
        color: Color,
    ) {
        layout(face, size, text, |glyph, at| {
            let Some(id) = glyph else { return };
            // Q16.16 from the start of the line: its whole pixels and its quarter.
            let quarter = (at + (1 << 13)) >> 14;
            let sub = (quarter.rem_euclid(SUBPIXELS as i64)) as u8;
            let px = x + quarter.div_euclid(SUBPIXELS as i64) as i32;
            let Some(g) = self.glyph(face, size, id, sub) else {
                return;
            };
            canvas.blend_mask(
                px + g.left,
                y + g.top,
                g.width,
                g.height,
                &g.coverage,
                color,
            );
        });
    }

    fn glyph(&mut self, face: Face, size: u32, id: u32, sub: u8) -> Option<&Glyph> {
        let key = |g: &Glyph| {
            g.face == face.0.as_ptr() as usize && g.size == size && g.id == id && g.sub == sub
        };
        if let Some(i) = self.glyphs.iter().position(key) {
            return self.glyphs.get(i);
        }
        let glyph = self.render(face, size, id, sub)?;
        if self.bytes + glyph.coverage.len() > CACHE_BYTES {
            self.glyphs.clear();
            self.bytes = 0;
        }
        self.bytes += glyph.coverage.len();
        self.glyphs.push(glyph);
        self.glyphs.last()
    }

    #[inline(never)]
    fn render(&mut self, face: Face, size: u32, id: u32, sub: u8) -> Option<Glyph> {
        let font = FontRef::new(face.0).ok()?;
        let outlines = font.outline_glyphs();
        let outline = outlines.get(GlyphId::new(id))?;
        let hinting = self.hinting(&outlines, face, size);
        let settings = match hinting {
            Some(h) => DrawSettings::hinted(h, false),
            None => DrawSettings::unhinted(Size::new(size as f32), LocationRef::default()),
        };
        let mut pen = Pen(Outline::new());
        outline.draw(settings, &mut pen).ok()?;
        let shift = sub as f32 / SUBPIXELS as f32;
        let (mut coverage, left, top, width, height) = pen.0.coverage(shift, GLYPH_LIMIT);
        for c in &mut coverage {
            *c = lift(*c);
        }
        Some(Glyph {
            face: face.0.as_ptr() as usize,
            size,
            id,
            sub,
            left,
            top,
            width,
            height,
            coverage,
        })
    }

    fn hinting(
        &mut self,
        outlines: &OutlineGlyphCollection,
        face: Face,
        size: u32,
    ) -> Option<&HintingInstance> {
        let at = self
            .sizes
            .iter()
            .position(|s| s.face == face.0.as_ptr() as usize && s.size == size);
        let at = match at {
            Some(i) => i,
            None => {
                let hinting = HintingInstance::new(
                    outlines,
                    Size::new(size as f32),
                    LocationRef::default(),
                    hinting_options(),
                )
                .ok();
                self.sizes.push(Sized {
                    face: face.0.as_ptr() as usize,
                    size,
                    hinting,
                });
                self.sizes.len() - 1
            }
        };
        self.sizes[at].hinting.as_ref()
    }
}

fn libm_ceil(v: f32) -> i32 {
    let t = v as i32;
    if (t as f32) < v {
        t + 1
    } else {
        t
    }
}

/// Walks `text`: `place` gets each glyph with its position in Q16.16 pixels, then
/// `None` and the line's end.
fn layout(face: Face, size: u32, text: &str, mut place: impl FnMut(Option<u32>, i64)) {
    let Ok(font) = FontRef::new(face.0) else {
        return;
    };
    let parsed = ttf_parser::Face::parse(face.0, 0).ok();
    let charmap = font.charmap();
    let advances = font.hmtx().ok();
    let units = font.head().map_or(1000, |h| h.units_per_em()) as i64;
    let scale = |v: i64| -> i64 { (v * ((size as i64) << 16)) / units };
    let mut x: i64 = 0;
    let mut before: Option<u32> = None;
    for c in text.chars() {
        let id = charmap.map(c).map_or(0, |g| g.to_u32());
        if let (Some(a), Some(f)) = (before, parsed.as_ref()) {
            x += scale(kerning(f, a as u16, id as u16) as i64);
        }
        place(Some(id), x);
        let advance = advances
            .as_ref()
            .map_or(0, |h| h.advance(skrifa::GlyphId::new(id)).unwrap_or(0));
        x += scale(advance as i64);
        before = Some(id);
    }
    place(None, x);
}

/// What the font's `kern` feature moves the second glyph of a pair by, in font units.
fn kerning(face: &ttf_parser::Face, a: u16, b: u16) -> i32 {
    let Some(gpos) = face.tables().gpos else {
        return 0;
    };
    let (a, b) = (ttf_parser::GlyphId(a), ttf_parser::GlyphId(b));
    let kern = ttf_parser::Tag::from_bytes(b"kern");
    for feature in gpos.features {
        if feature.tag != kern {
            continue;
        }
        for index in feature.lookup_indices {
            let Some(lookup) = gpos.lookups.get(index) else {
                continue;
            };
            for sub in lookup.subtables.into_iter::<PositioningSubtable>() {
                let PositioningSubtable::Pair(pair) = sub else {
                    continue;
                };
                if pair.coverage().get(a).is_none() {
                    continue;
                }
                match pair {
                    PairAdjustment::Format1 { coverage, sets } => {
                        let found = coverage
                            .get(a)
                            .and_then(|i| sets.get(i))
                            .and_then(|set| set.get(b));
                        if let Some((first, _)) = found {
                            return first.x_advance as i32;
                        }
                    }
                    PairAdjustment::Format2 {
                        classes, matrix, ..
                    } => {
                        let classes = (classes.0.get(a), classes.1.get(b));
                        if let Some((first, _)) = matrix.get(classes) {
                            return first.x_advance as i32;
                        }
                    }
                }
            }
        }
        return 0;
    }
    0
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use aricode_gfx::Rect;
    use std::vec;

    const INTER: Face = Face(INTER_REGULAR);

    fn dark_pixels(px: &[u32]) -> usize {
        px.iter().filter(|&&p| p != 0xFFFFFF).count()
    }

    #[test]
    fn every_font_has_its_letters_and_accents() {
        for face in [INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO] {
            let font = FontRef::new(face).unwrap();
            for c in "Aaz09ñÑáéíóúü¿¡€çÇ".chars() {
                assert!(font.charmap().map(c).is_some(), "{c}");
            }
        }
    }

    #[test]
    fn text_draws_inside_its_clip_only() {
        let mut t = Typesetter::new();
        let (w, h) = (200usize, 40usize);
        let mut px = vec![0xFFFFFFu32; w * h];
        {
            let mut c = Canvas::new(&mut px, w as u32, h as u32, w as u32).unwrap();
            let mut clipped = c.clipped(&Rect::new(10, 0, 50, 40));
            t.draw(&mut clipped, (INTER, 13), "Hamburgefonstiv ñ", (0, 20), 0);
        }
        assert!(dark_pixels(&px) > 50);
        for y in 0..h {
            for x in 0..w {
                if !(10..60).contains(&x) {
                    assert_eq!(px[y * w + x], 0xFFFFFF, "({x},{y})");
                }
            }
        }
    }

    #[test]
    fn nothing_breaks_at_the_edges_or_with_odd_text() {
        let mut t = Typesetter::new();
        let mut px = vec![0xFFFFFFu32; 64 * 16];
        let mut c = Canvas::new(&mut px, 64, 16, 64).unwrap();
        for (x, y) in [(-500, 8), (500, 8), (0, -500), (0, 500), (60, 15)] {
            t.draw(&mut c, (INTER, 13), "Text\u{1F600}\u{0}\t~", (x, y), 0);
        }
        t.draw(&mut c, (INTER, 13), "", (0, 8), 0);
        assert_eq!(t.width(INTER, 13, ""), 0);
        // A line longer than Q16.16 holds in an i32 still has its width.
        let long = "m".repeat(4000);
        assert!(t.width(INTER, 13, &long) > 32768);
        t.draw(&mut c, (Face(b"not a font"), 13), "abc", (0, 8), 0);
    }

    #[test]
    fn the_baseline_is_on_a_whole_pixel() {
        // A row of flat-bottomed letters ends sharply: light hinting at work.
        let mut t = Typesetter::new();
        let (w, h) = (200usize, 30usize);
        let mut px = vec![0xFFFFFFu32; w * h];
        let mut c = Canvas::new(&mut px, w as u32, h as u32, w as u32).unwrap();
        t.draw(&mut c, (INTER, 13), "HHHHHHHH", (2, 20), 0);
        let row = |y: usize| {
            px[y * w..(y + 1) * w]
                .iter()
                .filter(|&&p| p != 0xFFFFFF)
                .count()
        };
        assert!(row(19) > 0, "the last row above the baseline");
        assert_eq!(row(20), 0, "and nothing below it");
    }

    #[test]
    fn kerning_comes_from_the_font() {
        let t = Typesetter::new();
        // Inter tucks these pairs together.
        let apart = t.width(INTER, 100, "A") + t.width(INTER, 100, "V");
        assert!(t.width(INTER, 100, "AV") < apart);
    }

    /// Our rasterizer against zeno's: every glyph of every font, at the sizes we use,
    /// within a little of each other.
    #[test]
    fn coverage_matches_zeno() {
        use zeno::{Command, Mask, Origin, Vector};
        struct Commands(std::vec::Vec<Command>);
        impl OutlinePen for Commands {
            fn move_to(&mut self, x: f32, y: f32) {
                self.0.push(Command::MoveTo(Vector::new(x, -y)));
            }
            fn line_to(&mut self, x: f32, y: f32) {
                self.0.push(Command::LineTo(Vector::new(x, -y)));
            }
            fn quad_to(&mut self, a: f32, b: f32, x: f32, y: f32) {
                self.0
                    .push(Command::QuadTo(Vector::new(a, -b), Vector::new(x, -y)));
            }
            fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) {
                let (p, q, r) = (Vector::new(a, -b), Vector::new(c, -d), Vector::new(x, -y));
                self.0.push(Command::CurveTo(p, q, r));
            }
            fn close(&mut self) {
                self.0.push(Command::Close);
            }
        }
        let (mut worst, mut total, mut pixels) = (0i32, 0i64, 0i64);
        for data in [INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO] {
            let font = FontRef::new(data).unwrap();
            let outlines = font.outline_glyphs();
            for size in [11.0, 13.0, 17.0, 26.0] {
                for id in 0..font.maxp().unwrap().num_glyphs() as u32 {
                    let Some(glyph) = outlines.get(GlyphId::new(id)) else {
                        continue;
                    };
                    let settings =
                        || DrawSettings::unhinted(Size::new(size), LocationRef::default());
                    let mut ours = Pen(Outline::new());
                    glyph.draw(settings(), &mut ours).unwrap();
                    let (mask, left, top, w, h) = ours.0.coverage(0.0, 256);
                    let mut theirs = Commands(vec![]);
                    glyph.draw(settings(), &mut theirs).unwrap();
                    let (other, p) = Mask::new(theirs.0.as_slice())
                        .origin(Origin::TopLeft)
                        .render();
                    let at = |m: &[u8], area: (i32, i32, u32, u32), x: i32, y: i32| -> i32 {
                        let (cx, cy) = (x - area.0, y - area.1);
                        if cx < 0 || cy < 0 || cx >= area.2 as i32 || cy >= area.3 as i32 {
                            return 0;
                        }
                        m[cy as usize * area.2 as usize + cx as usize] as i32
                    };
                    let a = (left, top, w, h);
                    let b = (p.left, p.top, p.width, p.height);
                    for y in top.min(p.top)..(top + h as i32).max(p.top + p.height as i32) {
                        for x in left.min(p.left)..(left + w as i32).max(p.left + p.width as i32) {
                            let d = (at(&mask, a, x, y) - at(&other, b, x, y)).abs();
                            worst = worst.max(d);
                            total += d as i64;
                            pixels += 1;
                        }
                    }
                }
            }
        }
        let mean = total as f64 / pixels as f64;
        std::eprintln!("against zeno: worst {worst} of 255, mean {mean:.3}");
        assert!(mean < 1.5, "mean {mean}");
        assert!(worst <= 48, "worst {worst}");
    }

    /// The same advances as the reference shaper, glyph for glyph.
    #[test]
    fn spacing_matches_harfrust() {
        use harfrust::{FontRef as HFont, ShaperData, UnicodeBuffer};
        for (data, text) in [
            (
                INTER_REGULAR,
                "Hamburgefonstiv AVATAR To Ty. Español: año, niño, ¿qué?",
            ),
            (INTER_SEMIBOLD, "The window in use — Finder, Terminal"),
            (JETBRAINS_MONO, "fn main() { let x = 1; } // AV To"),
        ] {
            let font = HFont::new(data).unwrap();
            let shaper_data = ShaperData::new(&font);
            let shaper = shaper_data.shaper(&font).build();
            let mut buf = UnicodeBuffer::new();
            buf.push_str(text);
            buf.guess_segment_properties();
            let features = [
                harfrust::Feature::new(harfrust::Tag::new(b"liga"), 0, ..),
                harfrust::Feature::new(harfrust::Tag::new(b"calt"), 0, ..),
            ];
            let out = shaper.shape(buf, &features);
            let units = FontRef::new(data).unwrap().head().unwrap().units_per_em() as i64;
            let reference: i64 = out
                .glyph_positions()
                .iter()
                .map(|p| p.x_advance as i64)
                .sum();
            let ours = {
                let mut end = 0i64;
                layout(Face(data), 1000, text, |_, at| end = at as i64);
                // Q16.16 pixels at 1000 px a line, back to font units.
                end * units / 1000 / 65536
            };
            assert!(
                (ours - reference).abs() <= text.len() as i64,
                "{text}: {ours} and {reference}"
            );
        }
    }

    /// To look at it: `TEXT_DUMP=file cargo test -p aricode-text dump -- --ignored`.
    #[test]
    #[ignore]
    fn dump_sample() {
        let path = std::env::var("TEXT_DUMP").expect("TEXT_DUMP");
        let (w, h) = (620usize, 230usize);
        let mut px = vec![0xFFFFFFu32; w * h];
        let mut c = Canvas::new(&mut px, w as u32, h as u32, w as u32).unwrap();
        let mut t = Typesetter::new();
        let (bold, mono) = (Face(INTER_SEMIBOLD), Face(JETBRAINS_MONO));
        let lines: [(Face, u32, &str, i32, u32); 8] = [
            (INTER, 26, "Pista: una carrera", 34, 0),
            (
                bold,
                13,
                "The window in use  Título de la ventana",
                60,
                0x262626,
            ),
            (
                INTER,
                13,
                "Body 13/16. El veloz murciélago hindú comía feliz cardillo y kiwi.",
                82,
                0x262626,
            ),
            (
                INTER,
                12,
                "Callout 12/15: La cigüeña tocaba el saxofón detrás del palenque de paja.",
                102,
                0x262626,
            ),
            (
                INTER,
                11,
                "Subheadline 11/14: AV To Ty. WAVE Yo. 0123456789 ñÑ ¿? ¡! €",
                120,
                0x808080,
            ),
            (
                mono,
                13,
                "fn main() { let año = 2026; } // JetBrains Mono 13",
                146,
                0x262626,
            ),
            (
                mono,
                13,
                "ari> diag | grep temperature      {}[]()<>=-_+*",
                164,
                0x1E6EF4,
            ),
            (
                INTER,
                17,
                "Title 2 17/22 — Ventanas nítidas, al píxel",
                194,
                0,
            ),
        ];
        for (face, size, text, y, color) in lines {
            t.draw(&mut c, (face, size), text, (12, y), color);
        }
        std::eprintln!("cache {} glyphs, {} bytes", t.glyphs.len(), t.bytes);
        let bytes: alloc::vec::Vec<u8> = px
            .iter()
            .flat_map(|p| p.to_be_bytes()[1..].to_vec())
            .collect();
        std::fs::write(path, bytes).unwrap();
    }
}
