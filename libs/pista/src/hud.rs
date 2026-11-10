// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Resolution-independent race overlay. Coordinates use a 480x270 design grid.
use crate::fixed::f;
use crate::raster::Target;
use crate::render::PAINT;
use crate::world::{Phase, World, COUNTDOWN, LAPS, RATE};

const WHITE: u32 = 0xFFF0D9;
const MUTED: u32 = 0xA7BBC6;
const INK: u32 = 0x102A3B;
const GOLD: u32 = 0xFFD087;
const CYAN: u32 = 0x64E6ED;

#[derive(Default)]
pub struct Overlay {
    pub paused: bool,
    pub fps: u32,
    pub high_quality: bool,
}

struct Ui<'a, 'b>(&'a mut Target<'b>);
impl Ui<'_, '_> {
    fn rect(&mut self, x: i64, y: i64, w: i64, h: i64, color: u32) {
        let sx = self.0.width as i64;
        let sy = self.0.height as i64;
        for row in y * sy / 270..(y + h) * sy / 270 {
            self.0.span(row, x * sx / 480, (x + w) * sx / 480, color);
        }
    }
    fn text(&mut self, x: i64, y: i64, text: &[u8], size: i64, color: u32) {
        for (i, &c) in text.iter().enumerate() {
            let rows = glyph(c);
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..5 {
                    if bits & (1 << (4 - col)) != 0 {
                        self.rect(
                            x + i as i64 * 6 * size + col * size,
                            y + row as i64 * size,
                            size,
                            size,
                            color,
                        );
                    }
                }
            }
        }
    }
    fn number(&mut self, x: i64, y: i64, n: u32, digits: usize, size: i64, color: u32) {
        let mut bytes = [b'0'; 8];
        let mut n = n;
        for b in bytes[..digits].iter_mut().rev() {
            *b += (n % 10) as u8;
            n /= 10;
        }
        self.text(x, y, &bytes[..digits], size, color);
    }
    fn time(&mut self, x: i64, y: i64, ticks: u32, color: u32) {
        let seconds = ticks / RATE as u32;
        self.number(x, y, seconds / 60, 2, 1, color);
        self.text(x + 12, y, b":", 1, color);
        self.number(x + 18, y, seconds % 60, 2, 1, color);
        self.text(x + 30, y, b".", 1, color);
        self.number(
            x + 36,
            y,
            (ticks % RATE as u32) * 100 / RATE as u32,
            2,
            1,
            color,
        );
    }
    fn map(&mut self, world: &World) {
        self.rect(386, 8, 86, 78, INK);
        self.rect(386, 8, 2, 78, CYAN);
        self.text(396, 14, b"ALPINE LOOP", 1, MUTED);
        let (mut x0, mut x1, mut z0, mut z1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for s in &world.track.segments {
            x0 = x0.min(s.x);
            x1 = x1.max(s.x);
            z0 = z0.min(s.z);
            z1 = z1.max(s.z);
        }
        let span = (x1 - x0).max(z1 - z0) as i64;
        let place = |x: i32, z: i32| {
            (
                402 + (x - x0) as i64 * 54 / span,
                25 + (z1 - z) as i64 * 54 / span,
            )
        };
        for s in world.track.segments.iter().step_by(2) {
            let (x, y) = place(s.x, s.z);
            self.rect(x, y, 2, 2, 0x78959F);
        }
        for (kart, color) in world.karts.iter().zip(PAINT).rev() {
            let (x, y) = place(kart.x, kart.z);
            self.rect(x - 2, y - 2, 5, 5, INK);
            self.rect(x - 1, y - 1, 3, 3, color);
        }
        let k = &world.karts[0];
        let (x, y) = place(k.x, k.z);
        self.rect(x, y, 1, 1, WHITE);
    }
}

pub fn draw(target: &mut Target, world: &World, overlay: &Overlay) {
    let mut ui = Ui(target);
    let k = &world.karts[0];
    ui.rect(8, 8, 106, 30, INK);
    ui.rect(8, 8, 3, 30, PAINT[0]);
    ui.text(17, 13, b"PISTA", 2, WHITE);
    ui.text(17, 30, b"ALPINE GP / 01", 1, GOLD);
    ui.rect(122, 8, 91, 30, INK);
    ui.text(130, 14, b"LAP", 1, MUTED);
    ui.number(156, 12, (k.laps + 1).min(LAPS), 1, 2, WHITE);
    ui.text(172, 18, b"/ 3", 1, MUTED);
    ui.time(130, 27, world.clock, WHITE);
    ui.map(world);

    // Position and pace, separated from the racing line.
    ui.rect(8, 222, 72, 40, INK);
    ui.number(16, 226, world.position() as u32, 1, 4, WHITE);
    ui.text(45, 230, b"/ 4", 1, GOLD);
    ui.text(45, 246, b"POS", 1, MUTED);
    ui.rect(360, 222, 112, 40, INK);
    ui.number(
        371,
        227,
        ((k.speed as i64 * 36 / f(10) as i64) as u32).min(999),
        3,
        3,
        WHITE,
    );
    ui.text(428, 239, b"KM/H", 1, MUTED);
    for bar in 0..20 {
        let lit = k.speed as i64 * 20 / f(46) as i64 > bar;
        ui.rect(
            370 + bar * 5,
            253,
            4,
            3,
            if lit {
                if k.boost > 0 {
                    CYAN
                } else {
                    GOLD
                }
            } else {
                0x294452
            },
        );
    }
    if world.phase == Phase::Racing {
        if k.drift_charge > 0 || k.boost > 0 {
            ui.rect(163, 239, 154, 23, INK);
            let (label, color, charge) = if k.boost > 0 {
                (b"TURBO".as_slice(), CYAN, k.boost.min(90) * 100 / 90)
            } else if k.drift_charge >= 90 {
                (b"SUPER DRIFT - RELEASE".as_slice(), GOLD, 100)
            } else if k.drift_charge >= 40 {
                (
                    b"DRIFT - RELEASE".as_slice(),
                    CYAN,
                    k.drift_charge * 100 / 90,
                )
            } else {
                (
                    b"CHARGING DRIFT".as_slice(),
                    WHITE,
                    k.drift_charge * 100 / 90,
                )
            };
            ui.text(173, 244, label, 1, color);
            ui.rect(173, 255, 134, 2, 0x294452);
            ui.rect(173, 255, 134 * charge as i64 / 100, 2, color);
        } else {
            ui.text(168, 253, b"SPACE DRIFT   P PAUSE", 1, WHITE);
        }
        if world.wrong_way() {
            ui.rect(179, 54, 122, 18, INK);
            ui.text(189, 60, b"WRONG WAY - R RESET", 1, GOLD);
        } else if k.recovery > 0 {
            ui.rect(197, 54, 86, 18, INK);
            ui.text(207, 60, b"BACK ON TRACK", 1, CYAN);
        } else if world.clock < 60 {
            ui.text(222, 70, b"GO!", 3, CYAN);
        }
        if k.best_lap > 0 {
            ui.rect(8, 44, 106, 15, INK);
            ui.text(15, 48, b"BEST", 1, MUTED);
            ui.time(48, 48, k.best_lap, GOLD);
        }
    }
    if world.phase == Phase::Countdown {
        let count = (COUNTDOWN.saturating_sub(world.step))
            .div_ceil(RATE as u32)
            .max(1);
        ui.rect(216, 66, 48, 48, INK);
        ui.number(229, 72, count, 1, 5, GOLD);
        for light in 0..3 {
            ui.rect(
                214 + light * 20,
                120,
                12,
                4,
                if light < (3 - count) as i64 {
                    CYAN
                } else {
                    0x59707C
                },
            );
        }
        ui.rect(110, 169, 260, 43, INK);
        ui.text(126, 177, b"WASD / ARROWS DRIVE    SPACE DRIFT", 1, WHITE);
        ui.text(126, 189, b"RELEASE A CHARGED DRIFT FOR TURBO", 1, CYAN);
        ui.text(126, 201, b"TIME THE THROTTLE JUST BEFORE GO", 1, GOLD);
    }
    if world.phase == Phase::Finished || overlay.paused {
        // Dim the scene while leaving the result panel completely opaque.
        for pixel in ui.0.pixels.iter_mut() {
            *pixel = (*pixel & 0xFEFEFE) >> 1;
        }
        ui.rect(105, 63, 270, 147, INK);
        ui.rect(105, 63, 270, 3, if overlay.paused { CYAN } else { GOLD });
        if overlay.paused {
            ui.text(174, 79, b"PAUSED", 3, WHITE);
            ui.text(129, 113, b"P / ENTER       RESUME", 1, WHITE);
            ui.text(129, 128, b"R               RETURN TO TRACK", 1, MUTED);
            ui.text(129, 143, b"Q               GRAPHICS QUALITY", 1, MUTED);
            ui.text(129, 158, b"ESC             EXIT", 1, MUTED);
            ui.text(129, 184, b"SPACE + TURN, THEN RELEASE: TURBO", 1, CYAN);
        } else {
            let label = if world.position() == 1 {
                b"VICTORY".as_slice()
            } else {
                b"RACE COMPLETE".as_slice()
            };
            ui.text(240 - label.len() as i64 * 6, 78, label, 2, WHITE);
            ui.number(130, 106, world.position() as u32, 1, 5, GOLD);
            ui.text(166, 131, b"/ 4", 1, MUTED);
            ui.text(216, 110, b"RACE TIME", 1, MUTED);
            ui.time(216, 122, k.finished.unwrap_or(world.clock), WHITE);
            ui.text(216, 141, b"BEST LAP", 1, MUTED);
            ui.time(216, 153, k.best_lap, GOLD);
            ui.text(137, 184, b"ENTER RACE AGAIN   ESC EXIT", 1, CYAN);
        }
    }
    if overlay.paused {
        ui.text(
            8,
            269 - 7,
            if overlay.high_quality {
                b"Q: HIGH 960X540"
            } else {
                b"Q: FAST 480X270"
            },
            1,
            MUTED,
        );
        ui.number(427, 269 - 7, overlay.fps.min(999), 3, 1, MUTED);
        ui.text(449, 269 - 7, b"FPS", 1, MUTED);
    }
}

fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'A' => [14, 17, 17, 31, 17, 17, 17],
        b'B' => [30, 17, 17, 30, 17, 17, 30],
        b'C' => [14, 17, 16, 16, 16, 17, 14],
        b'D' => [30, 17, 17, 17, 17, 17, 30],
        b'E' => [31, 16, 16, 30, 16, 16, 31],
        b'F' => [31, 16, 16, 30, 16, 16, 16],
        b'G' => [14, 17, 16, 23, 17, 17, 15],
        b'H' => [17, 17, 17, 31, 17, 17, 17],
        b'I' => [31, 4, 4, 4, 4, 4, 31],
        b'J' => [7, 2, 2, 2, 18, 18, 12],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        b'L' => [16, 16, 16, 16, 16, 16, 31],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'N' => [17, 25, 25, 21, 19, 19, 17],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'Q' => [14, 17, 17, 17, 21, 18, 13],
        b'R' => [30, 17, 17, 30, 20, 18, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'U' => [17, 17, 17, 17, 17, 17, 14],
        b'V' => [17, 17, 17, 17, 17, 10, 4],
        b'W' => [17, 17, 17, 21, 21, 21, 10],
        b'X' => [17, 17, 10, 4, 10, 17, 17],
        b'Y' => [17, 17, 10, 4, 4, 4, 4],
        b'Z' => [31, 1, 2, 4, 8, 16, 31],
        b'0' => [14, 17, 19, 21, 25, 17, 14],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        b'8' => [14, 17, 17, 14, 17, 17, 14],
        b'9' => [14, 17, 17, 15, 1, 1, 14],
        b'/' => [1, 1, 2, 4, 8, 16, 16],
        b':' => [0, 4, 4, 0, 4, 4, 0],
        b'.' => [0, 0, 0, 0, 0, 6, 6],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'+' => [0, 4, 4, 31, 4, 4, 0],
        b'!' => [4, 4, 4, 4, 4, 0, 4],
        _ => [0; 7],
    }
}
