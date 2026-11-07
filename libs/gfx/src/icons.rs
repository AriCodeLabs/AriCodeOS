// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Built-in artwork, shared by the shell and toolkit. No package data is parsed.

use crate::{rgb, Canvas, Color, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Application,
    Launcher,
    Terminal,
    Files,
    Editor,
    Settings,
    Pista,
    Document,
}

impl Icon {
    /// Only exact package identifiers select artwork. A window title is not an identity.
    pub fn application(name: &str) -> Self {
        match name {
            "launcher" => Self::Launcher,
            "terminal" => Self::Terminal,
            "files" => Self::Files,
            "editor" => Self::Editor,
            "settings" => Self::Settings,
            "pista" => Self::Pista,
            _ => Self::Application,
        }
    }

    /// Built-in display names; unknown packages keep their complete identifier.
    pub fn application_name(name: &str) -> &str {
        match name {
            "launcher" => "Applications",
            "terminal" => "Terminal",
            "files" => "Files",
            "editor" => "Editor",
            "settings" => "Settings",
            "pista" => "Pista",
            _ => name,
        }
    }

    /// Original, analytic artwork on a 32-unit grid, at up to 64 pixels. All drawing
    /// respects both the caller's clip and the icon's box, including tiny sizes.
    pub fn draw(self, out: &mut Canvas, rect: Rect) {
        let size = rect.w.min(rect.h).min(64);
        if size == 0 {
            return;
        }
        let rect = Rect::new(rect.x, rect.y, size, size);
        let mut out = out.clipped(&rect);
        let base = match self {
            Self::Terminal => rgb(0x30, 0x40, 0x55),
            Self::Files => rgb(0x16, 0x72, 0xC4),
            Self::Editor | Self::Document => rgb(0x26, 0x79, 0x68),
            Self::Settings => rgb(0x64, 0x6C, 0x80),
            Self::Pista => rgb(0xB0, 0x4D, 0x29),
            Self::Launcher | Self::Application => rgb(0x65, 0x50, 0xB5),
        };
        out.rounded(&rect, (size / 4) as i32, base);
        let white = rgb(0xFF, 0xFF, 0xFF);
        let scale = size as f32 / 32.0;
        let box_at = |x: u32, y: u32, w: u32, h: u32| {
            Rect::new(
                rect.x + (x * size / 32) as i32,
                rect.y + (y * size / 32) as i32,
                (w * size / 32).max(1),
                (h * size / 32).max(1),
            )
        };
        match self {
            Self::Launcher => {
                for y in [8, 19] {
                    for x in [8, 19] {
                        out.rounded(&box_at(x, y, 6, 6), (size / 16) as i32, white);
                    }
                }
            }
            Self::Files => {
                out.rounded(&box_at(5, 8, 10, 7), (size / 16) as i32, white);
                out.rounded(&box_at(5, 12, 22, 13), (size / 16) as i32, white);
            }
            Self::Editor | Self::Document => {
                out.rounded(&box_at(8, 5, 16, 22), (size / 16) as i32, white);
                for y in [11, 16, 21] {
                    out.fill_rect(&box_at(11, y, if y == 21 { 6 } else { 10 }, 2), base);
                }
            }
            Self::Settings => {
                for (x, y) in [(9, 12), (16, 21), (23, 10)] {
                    out.rounded(&box_at(x, 7, 2, 19), (size / 32) as i32, white);
                    out.circle(&box_at(x - 2, y - 2, 6, 6), white);
                }
            }
            Self::Terminal => {
                for (a, b) in [
                    ((8., 10.), (14., 16.)),
                    ((14., 16.), (8., 22.)),
                    ((17., 22.), (24., 22.)),
                ] {
                    stroke(&mut out, rect, scale, a, b, white);
                }
            }
            Self::Pista => {
                for (a, b) in [
                    ((9., 25.), (13., 7.)),
                    ((23., 25.), (19., 7.)),
                    ((16., 24.), (16., 20.)),
                    ((16., 14.), (16., 10.)),
                ] {
                    stroke(&mut out, rect, scale, a, b, white);
                }
            }
            Self::Application => {
                out.rounded(&box_at(6, 7, 20, 18), (size / 16) as i32, white);
                out.fill_rect(&box_at(8, 13, 16, 10), base);
            }
        }
    }
}

/// A round-ended stroke, with four samples per axis. Work is bounded by 64x64;
/// no path interpreter, allocation, font glyph or external asset is needed.
fn stroke(out: &mut Canvas, rect: Rect, scale: f32, a: (f32, f32), b: (f32, f32), ink: Color) {
    let (ax, ay) = (a.0 * scale, a.1 * scale);
    let (dx, dy) = ((b.0 - a.0) * scale, (b.1 - a.1) * scale);
    let length = dx * dx + dy * dy;
    let radius = scale.max(0.5);
    let bounds = Rect::new(
        rect.x + (ax.min(ax + dx) - radius).max(0.) as i32,
        rect.y + (ay.min(ay + dy) - radius).max(0.) as i32,
        (dx.abs() + radius * 2. + 2.) as u32,
        (dy.abs() + radius * 2. + 2.) as u32,
    )
    .intersect(&out.clip());
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            let mut covered = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = (x - rect.x) as f32 + (sx as f32 + 0.5) / 4. - ax;
                    let py = (y - rect.y) as f32 + (sy as f32 + 0.5) / 4. - ay;
                    let t = ((px * dx + py * dy) / length).clamp(0., 1.);
                    let (ex, ey) = (px - t * dx, py - t * dy);
                    covered += u32::from(ex * ex + ey * ey <= radius * radius);
                }
            }
            out.blend_pixel(x, y, ink, covered * 16);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn every_icon_is_clipped_and_partial_paints_match() {
        for icon in [
            Icon::Application,
            Icon::Launcher,
            Icon::Terminal,
            Icon::Files,
            Icon::Editor,
            Icon::Settings,
            Icon::Pista,
            Icon::Document,
        ] {
            for size in [0, 1, 16, 20, 32, 64, 80] {
                let mut full = std::vec![0x123456; 80 * 80];
                let mut partial = full.clone();
                let r = Rect::new(3, 4, size, size);
                icon.draw(&mut Canvas::new(&mut full, 80, 80, 80).unwrap(), r);
                let mut c = Canvas::new(&mut partial, 80, 80, 80).unwrap();
                for y in (0..80).step_by(7) {
                    icon.draw(&mut c.clipped(&Rect::new(0, y, 80, 7)), r);
                }
                assert_eq!(full, partial, "{icon:?}, {size}");
                for y in 0..80 {
                    for x in 0..80 {
                        if !r.contains(x, y) {
                            assert_eq!(full[(y * 80 + x) as usize], 0x123456);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_packages_keep_their_names_and_generic_artwork() {
        for name in ["", "Terminal", "terminal-copy", "other_app"] {
            assert_eq!(Icon::application(name), Icon::Application);
            assert_eq!(Icon::application_name(name), name);
        }
    }
}
