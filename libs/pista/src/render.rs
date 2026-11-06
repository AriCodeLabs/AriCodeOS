// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The picture: a chase camera, and polygons painted from far to near, with
//! the road's order along the circuit standing in for a depth buffer.

use crate::fixed::{cos, f, mul, sin, F, ONE};
use crate::raster::Target;
use crate::track::{Segment, HALF_WIDTH, SEGMENTS};
use crate::world::{is_turbo, Kart, World, KARTS};

/// How many segments ahead are drawn.
const DRAW: usize = 110;
const NEAR: F = ONE / 2;
/// The striped grass beside the road: past this, plain ground. Wider bands fold
/// over themselves inside tight corners.
const GRASS_WIDTH: F = f(24);
const CURB: F = f(1);
const CAMERA_BACK: F = f(9);
const CAMERA_UP: F = f(3);

const HAZE: u32 = 0xBFD8EA;
const SKY_TOP: u32 = 0x3A7BD5;
const SKY_LOW: u32 = 0xBFD8EA;
const HILLS: u32 = 0x6F8FA8;
const GRASS: [u32; 2] = [0x3F9B3F, 0x389038];
const ROAD: [u32; 2] = [0x55585F, 0x4F5259];
const CURBS: [u32; 2] = [0xD8342B, 0xF2F2F2];
const LINE: u32 = 0xE8E8E8;
const TURBO: [u32; 2] = [0xFFB000, 0xFF6A00];
const TRUNK: u32 = 0x6B4A2B;
const LEAVES: [u32; 2] = [0x1F6B33, 0x2B8042];
/// The karts' colours.
pub const PAINT: [u32; KARTS] = [0xE5322D, 0x2D6BE5, 0xF2C200, 0x8A3FD1];

/// What is kept from frame to frame: the camera swings after the kart.
#[derive(Debug, Clone, Copy, Default)]
pub struct View {
    heading: u16,
    ready: bool,
}

#[derive(Clone, Copy)]
struct Camera {
    x: F,
    y: F,
    z: F,
    sin: F,
    cos: F,
    focal: i64,
    center: i64,
    horizon: i64,
}

/// A point in the camera's space.
#[derive(Clone, Copy)]
struct Eye {
    x: F,
    y: F,
    z: F,
}

impl Camera {
    fn eye(&self, p: [F; 3]) -> Eye {
        let (dx, dy, dz) = (p[0] - self.x, p[1] - self.y, p[2] - self.z);
        Eye {
            x: mul(dx, self.cos) - mul(dz, self.sin),
            y: dy,
            z: mul(dx, self.sin) + mul(dz, self.cos),
        }
    }

    /// On screen in pixels, Q16.16. `z` is at least `NEAR` here.
    fn screen(&self, e: Eye) -> (i64, i64) {
        let z = e.z as i64;
        (
            self.center + e.x as i64 * self.focal / z,
            self.horizon - e.y as i64 * self.focal / z,
        )
    }
}

/// Mixes `amount`/256 of `to` into `c`.
fn mix(c: u32, to: u32, amount: u32) -> u32 {
    let keep = 256 - amount;
    let rb = ((c & 0xFF00FF) * keep + (to & 0xFF00FF) * amount) >> 8;
    let g = ((c & 0xFF00) * keep + (to & 0xFF00) * amount) >> 8;
    (rb & 0xFF00FF) | (g & 0xFF00)
}

struct Painter<'a, 'b> {
    target: &'a mut Target<'b>,
    camera: Camera,
}

impl Painter<'_, '_> {
    /// A convex polygon of up to 4 points, cut at the near plane. `solid`: a
    /// face seen from behind is skipped.
    fn polygon(&mut self, points: &[[F; 3]], color: u32, solid: bool) {
        let mut cut = [Eye { x: 0, y: 0, z: 0 }; 8];
        let mut n = 0;
        for i in 0..points.len() {
            let a = self.camera.eye(points[i]);
            let b = self.camera.eye(points[(i + 1) % points.len()]);
            if a.z >= NEAR {
                cut[n] = a;
                n += 1;
            }
            if (a.z >= NEAR) != (b.z >= NEAR) {
                let t = (NEAR - a.z) as i64 * ONE as i64 / (b.z - a.z) as i64;
                let along = |p: F, q: F| p + (((q - p) as i64 * t) >> 16) as F;
                cut[n] = Eye {
                    x: along(a.x, b.x),
                    y: along(a.y, b.y),
                    z: NEAR,
                };
                n += 1;
            }
        }
        if n < 3 {
            return;
        }
        let first = self.camera.screen(cut[0]);
        let mut last = self.camera.screen(cut[1]);
        for e in &cut[2..n] {
            let next = self.camera.screen(*e);
            if solid {
                let (ax, ay) = ((last.0 - first.0) >> 8, (last.1 - first.1) >> 8);
                let (bx, by) = ((next.0 - first.0) >> 8, (next.1 - first.1) >> 8);
                if ax * by - ay * bx <= 0 {
                    last = next;
                    continue;
                }
            }
            self.target.triangle(first, last, next, color);
            last = next;
        }
    }

    /// A box in a frame placed at `at`, turned by `heading`: x right, y up,
    /// z forward, in metres, Q16.16. Its bottom is never seen.
    fn block(&mut self, at: [F; 3], heading: u16, size: [F; 6], color: u32, fog: u32) {
        let (s, c) = (sin(heading), cos(heading));
        let [x0, x1, y0, y1, z0, z1] = size;
        let corner = |x: F, y: F, z: F| {
            [
                at[0] + mul(x, c) + mul(z, s),
                at[1] + y,
                at[2] - mul(x, s) + mul(z, c),
            ]
        };
        let shade = |n: u32| mix(mix(color, 0, n), HAZE, fog);
        let (a, b, c2, d) = (
            corner(x0, y0, z0),
            corner(x1, y0, z0),
            corner(x1, y0, z1),
            corner(x0, y0, z1),
        );
        let (e, g, h, i) = (
            corner(x0, y1, z0),
            corner(x1, y1, z0),
            corner(x1, y1, z1),
            corner(x0, y1, z1),
        );
        self.polygon(&[e, g, b, a], shade(0), true);
        self.polygon(&[g, h, c2, b], shade(60), true);
        self.polygon(&[i, e, a, d], shade(60), true);
        self.polygon(&[h, i, d, c2], shade(30), true);
        self.polygon(&[i, h, g, e], mix(shade(0), 0xFFFFFF, 30), true);
    }

    fn tree(&mut self, at: [F; 3], tall: F, leaves: u32, fog: u32) {
        let t = f(1) / 3;
        self.block(at, 0, [-t, t, 0, f(2), -t, t], TRUNK, fog);
        let r = tall / 3;
        let top = [at[0], at[1] + tall, at[2]];
        let foot = |x: F, z: F| [at[0] + x, at[1] + f(2), at[2] + z];
        let (a, b, c, d) = (foot(-r, -r), foot(r, -r), foot(r, r), foot(-r, r));
        let paint = |n: u32| mix(mix(leaves, 0, n), HAZE, fog);
        self.polygon(&[top, b, a], paint(0), true);
        self.polygon(&[top, c, b], paint(50), true);
        self.polygon(&[top, a, d], paint(50), true);
        self.polygon(&[top, d, c], paint(25), true);
    }

    fn kart(&mut self, k: &Kart, color: u32, fog: u32) {
        let at = [k.x, k.y, k.z];
        // The body turns with where it points, and leans into nothing: a slide
        // shows as heading against course.
        let h = k.heading;
        let tenth = ONE / 10;
        let wheel = |x: F, z: F| {
            [
                x - 2 * tenth,
                x + 2 * tenth,
                0,
                4 * tenth,
                z - 3 * tenth,
                z + 3 * tenth,
            ]
        };
        let track = 8 * tenth;
        for size in [
            wheel(-track, -8 * tenth),
            wheel(track, -8 * tenth),
            wheel(-track, 8 * tenth),
            wheel(track, 8 * tenth),
        ] {
            self.block(at, h, size, 0x1A1A1A, fog);
        }
        self.block(
            at,
            h,
            [
                -6 * tenth,
                6 * tenth,
                2 * tenth,
                5 * tenth,
                -11 * tenth,
                12 * tenth,
            ],
            color,
            fog,
        );
        self.block(
            at,
            h,
            [
                -3 * tenth,
                3 * tenth,
                5 * tenth,
                9 * tenth,
                -5 * tenth,
                tenth,
            ],
            0x222831,
            fog,
        );
        self.block(
            at,
            h,
            [-2 * tenth, 2 * tenth, 9 * tenth, 12 * tenth, -3 * tenth, 0],
            0xF0D0A0,
            fog,
        );
        self.block(
            at,
            h,
            [
                -7 * tenth,
                7 * tenth,
                7 * tenth,
                8 * tenth,
                -13 * tenth,
                -9 * tenth,
            ],
            color,
            fog,
        );
    }
}

/// The sky, with its gradient, a line of far hills that slide as the camera
/// turns, and the ground as far as the road's end.
fn backdrop(target: &mut Target, horizon: usize, heading: u16) {
    for y in 0..horizon {
        let c = mix(SKY_TOP, SKY_LOW, (y * 256 / horizon.max(1)) as u32);
        target.fill_rows(y, y + 1, c);
    }
    let rows = target.height.saturating_sub(horizon).max(1);
    for y in horizon..target.height {
        let far = (y - horizon) * 256 / rows;
        // Nearer the horizon, hazier: quickly, as distance runs out at the top.
        let haze = 170u32.saturating_sub((far * 4) as u32);
        target.fill_rows(y, y + 1, mix(GRASS[1], HAZE, haze));
    }
    let (w, h) = (target.width, target.height as i64);
    for x in 0..w {
        let a = heading.wrapping_mul(2).wrapping_add((x * 136) as u16);
        let tall = 10 + ((mul(sin(a), f(7)) + mul(sin(a.wrapping_mul(3)), f(4))) >> 16) as i64;
        for y in (horizon as i64 - tall).max(0)..(horizon as i64).min(h) {
            target.pixels[y as usize * w + x] = HILLS;
        }
    }
}

/// The band between `from` and `to` across the road, from segment `s` to `n`.
fn band(s: &Segment, n: &Segment, from: F, to: F) -> [[F; 3]; 4] {
    [
        [s.x + mul(from, s.right_x), s.y, s.z + mul(from, s.right_z)],
        [s.x + mul(to, s.right_x), s.y, s.z + mul(to, s.right_z)],
        [n.x + mul(to, n.right_x), n.y, n.z + mul(to, n.right_z)],
        [n.x + mul(from, n.right_x), n.y, n.z + mul(from, n.right_z)],
    ]
}

/// The grass of segment `i`. All of it goes down before any road: inside a tight
/// corner, a nearer segment's grass reaches over a farther one's road.
fn grass(p: &mut Painter, world: &World, i: usize, fog: u32) {
    let (s, n) = (*world.track.at(i), *world.track.at(i + 1));
    let c = mix(GRASS[(i % SEGMENTS / 4) & 1], HAZE, fog);
    p.polygon(&band(&s, &n, -GRASS_WIDTH, GRASS_WIDTH), c, false);
}

/// The road of segment `i`.
fn road(p: &mut Painter, world: &World, i: usize, fog: u32) {
    let (s, n) = (*world.track.at(i), *world.track.at(i + 1));
    let index = i % SEGMENTS;
    let hue = |pair: [u32; 2], every: usize| mix(pair[(index / every) & 1], HAZE, fog);
    p.polygon(&band(&s, &n, -HALF_WIDTH, HALF_WIDTH), hue(ROAD, 3), false);
    let edge = HALF_WIDTH + CURB;
    p.polygon(&band(&s, &n, -edge, -HALF_WIDTH), hue(CURBS, 2), false);
    p.polygon(&band(&s, &n, HALF_WIDTH, edge), hue(CURBS, 2), false);
    if is_turbo(index) {
        p.polygon(&band(&s, &n, -f(4), f(4)), hue(TURBO, 1), false);
    } else if index % 8 < 3 {
        let line = mix(LINE, HAZE, fog);
        p.polygon(&band(&s, &n, -ONE / 5, ONE / 5), line, false);
    }
    // The start line: squares across the road.
    if index == 0 {
        for square in -7..7 {
            let from = f(square);
            let c = if square & 1 == 0 { 0xF4F4F4 } else { 0x111111 };
            p.polygon(&band(&s, &n, from, from + f(1)), mix(c, HAZE, fog), false);
        }
    }
}

/// What stands on segment `i`.
fn standing(p: &mut Painter, world: &World, i: usize, fog: u32) {
    let s = *world.track.at(i);
    let index = i % SEGMENTS;
    // Trees along the verges, from the segment's number: the same every lap.
    if index.is_multiple_of(5) {
        let code = index.wrapping_mul(2654435761) >> 7;
        let distance = HALF_WIDTH + f(5) + f((code % 22) as i32);
        let off = if code & 1 == 0 { distance } else { -distance };
        let at = [s.x + mul(off, s.right_x), s.y, s.z + mul(off, s.right_z)];
        let tall = f(7) + f((code % 5) as i32);
        p.tree(at, tall, LEAVES[(code >> 3) & 1], fog);
    }
    if index == 0 {
        arch(p, &s, fog);
    }
    for (kart, paint) in world.karts.iter().zip(PAINT) {
        if kart.segment == index {
            p.kart(kart, paint, fog);
        }
    }
}

/// The start and finish gantry.
fn arch(p: &mut Painter, s: &Segment, fog: u32) {
    let side = HALF_WIDTH + f(2);
    let point = |lateral: F, y: F| {
        [
            s.x + mul(lateral, s.right_x),
            s.y + y,
            s.z + mul(lateral, s.right_z),
        ]
    };
    for x in [-side, side] {
        let at = point(x, 0);
        let t = ONE / 2;
        p.block(at, 0, [-t, t, 0, f(7), -t, t], 0x30343A, fog);
    }
    p.polygon(
        &[
            point(-side, f(7)),
            point(side, f(7)),
            point(side, f(5)),
            point(-side, f(5)),
        ],
        mix(0xF4F4F4, HAZE, fog),
        false,
    );
}

/// A map of the circuit in a corner, with the karts on it.
fn map(target: &mut Target, world: &World) {
    const SIZE: i64 = 64;
    const MARGIN: i64 = 8;
    let (mut x0, mut x1, mut z0, mut z1) = (F::MAX, F::MIN, F::MAX, F::MIN);
    for s in &world.track.segments {
        x0 = x0.min(s.x);
        x1 = x1.max(s.x);
        z0 = z0.min(s.z);
        z1 = z1.max(s.z);
    }
    let span = ((x1 - x0).max(z1 - z0) as i64).max(1);
    let left = target.width as i64 - SIZE - MARGIN;
    let place = |x: F, z: F| {
        (
            left + (x - x0) as i64 * SIZE / span,
            MARGIN + (z1 - z) as i64 * SIZE / span,
        )
    };
    for s in world.track.segments.iter().step_by(3) {
        let (x, y) = place(s.x, s.z);
        target.span(y, x, x + 1, 0xFFFFFF);
    }
    for (k, paint) in world.karts.iter().zip(PAINT).rev() {
        let (x, y) = place(k.x, k.z);
        for row in y - 1..=y + 1 {
            target.span(row, x - 1, x + 2, paint);
        }
    }
}

/// Draws kart 0's view of `world` on `target`.
pub fn draw(world: &World, view: &mut View, target: &mut Target) {
    let you = &world.karts[0];
    if !view.ready {
        view.heading = you.heading;
        view.ready = true;
    }
    let lag = you.heading.wrapping_sub(view.heading) as i16 as i32;
    view.heading = view.heading.wrapping_add((lag / 5) as i16 as u16);
    let (s, c) = (sin(view.heading), cos(view.heading));
    let (w, h) = (target.width as i64, target.height as i64);
    let horizon = h * 9 / 25;
    let camera = Camera {
        x: you.x - mul(CAMERA_BACK, s),
        y: you.y + CAMERA_UP,
        z: you.z - mul(CAMERA_BACK, c),
        sin: s,
        cos: c,
        focal: (w * 9 / 10) << 16,
        center: (w << 16) / 2,
        horizon: horizon << 16,
    };
    backdrop(target, horizon as usize, view.heading);
    let mut p = Painter { target, camera };
    let segments = || {
        (0..DRAW).rev().map(|k| {
            let fog = (k * k * 200 / (DRAW * DRAW)) as u32;
            (you.segment + SEGMENTS + k - 8, fog)
        })
    };
    for (i, fog) in segments() {
        grass(&mut p, world, i, fog);
    }
    for (i, fog) in segments() {
        road(&mut p, world, i, fog);
    }
    for (i, fog) in segments() {
        standing(&mut p, world, i, fog);
    }
    map(target, world);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::world::{Input, Phase, COUNTDOWN};
    use std::vec;
    use std::vec::Vec;

    const W: usize = 480;
    const H: usize = 270;

    fn frame(world: &World, view: &mut View) -> Vec<u32> {
        let mut pixels = vec![0xDEAD; W * H];
        let mut target = Target {
            pixels: &mut pixels,
            width: W,
            height: H,
        };
        draw(world, view, &mut target);
        pixels
    }

    #[test]
    fn the_picture_has_sky_road_and_the_kart() {
        let world = World::new();
        let px = frame(&world, &mut View::default());
        assert!(!px.contains(&0xDEAD), "every pixel painted");
        // Top row: the sky's first colour. Under the camera: the road.
        assert_eq!(px[W / 4], SKY_TOP);
        assert!(
            ROAD.iter().any(|r| px.contains(&mix(*r, HAZE, 0))),
            "the road"
        );
        assert!(
            px.contains(&mix(mix(PAINT[0], 0, 0), HAZE, 0)),
            "the kart's paint"
        );
    }

    #[test]
    fn the_same_race_gives_the_same_pictures() {
        let run = || {
            let (mut w, mut v) = (World::new(), View::default());
            for _ in 0..COUNTDOWN + 60 {
                let input = w.autopilot(0);
                w.advance(input);
            }
            frame(&w, &mut v)
        };
        assert_eq!(run(), run());
    }

    /// A whole race, on the road and off it, drawn at every step: no vertex,
    /// however it falls, reaches outside the buffer or divides by zero.
    #[test]
    fn nothing_breaks_along_a_whole_lap() {
        let (mut w, mut v) = (World::new(), View::default());
        let mut pixels = vec![0; W * H];
        let mut n = 0;
        while w.karts[0].laps == 0 && n < 6000 {
            let mut input = w.autopilot(0);
            // Now and then it runs off, sliding.
            if n % 900 > 800 {
                input = Input {
                    throttle: true,
                    right: true,
                    drift: true,
                    ..Input::default()
                };
            }
            w.advance(input);
            n += 1;
            if n % 3 == 0 {
                let mut target = Target {
                    pixels: &mut pixels,
                    width: W,
                    height: H,
                };
                draw(&w, &mut v, &mut target);
            }
        }
        assert_ne!(w.phase, Phase::Countdown);
        assert_eq!(w.karts[0].laps, 1);
    }

    /// To look at the picture: `PISTA_DUMP=dir cargo test -p aricode-pista dump --
    /// --ignored` writes raw frames at three moments of a race.
    #[test]
    #[ignore]
    fn dump_frames() {
        let dir = std::env::var("PISTA_DUMP").expect("PISTA_DUMP");
        let (mut w, mut v) = (World::new(), View::default());
        for (n, step) in [1u32, COUNTDOWN + 500, COUNTDOWN + 1100]
            .into_iter()
            .enumerate()
        {
            while w.step < step {
                let input = w.autopilot(0);
                w.advance(input);
                // The camera follows frame by frame.
                frame(&w, &mut v);
            }
            let px = frame(&w, &mut v);
            let bytes: Vec<u8> = px
                .iter()
                .flat_map(|p| p.to_be_bytes()[1..].to_vec())
                .collect();
            std::fs::write(std::format!("{dir}/frame{n}.rgb"), bytes).unwrap();
        }
    }

    #[test]
    fn a_small_target_is_fine_too() {
        let world = World::new();
        let mut pixels = vec![0; 8 * 4];
        let mut target = Target {
            pixels: &mut pixels,
            width: 8,
            height: 4,
        };
        draw(&world, &mut View::default(), &mut target);
    }
}
