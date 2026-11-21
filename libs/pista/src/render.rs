// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The picture: a chase camera, perspective-correct terrain textures and a
//! reciprocal depth buffer shared by the track, scenery and vehicles.

use crate::fixed::{cos, f, isqrt, mul, sin, F, ONE};
use crate::raster::Target;
use crate::track::{Segment, HALF_WIDTH, SEGMENTS};
use crate::world::{is_turbo, Kart, World, KARTS};

/// Scenery remains visible in every direction, including when turned around.
const FAR: F = f(260);
const NEAR: F = ONE / 2;
/// The striped grass beside the road: past this, plain ground. Wider bands fold
/// over themselves inside tight corners.
const GRASS_WIDTH: F = f(24);
const CURB: F = f(1);
const CAMERA_BACK: F = f(10);
const CAMERA_UP: F = f(3) + ONE / 2;

const HAZE: u32 = 0xE7BD9B;
const SKY_TOP: u32 = 0x345E8B;
const SKY_LOW: u32 = 0xF7C9A6;
const GRASS: [u32; 2] = [0x407B5C, 0x427C5B];
const ROAD: [u32; 2] = [0x424B57, 0x444D59];
const CURBS: [u32; 2] = [0xE96148, 0xF7EACD];
const LINE: u32 = 0xE8E8E8;
const TURBO: [u32; 2] = [0x48DDE1, 0xB7FFFF];
const TRUNK: u32 = 0x6B4A2B;
const LEAVES: [u32; 2] = [0x246C5C, 0x357E62];
/// The karts' colours.
pub const PAINT: [u32; KARTS] = [0xFF654D, 0x39C6E8, 0xFFC857, 0xBD87F5];

/// What is kept from frame to frame: the camera swings after the kart.
#[derive(Debug, Clone, Copy, Default)]
pub struct View {
    heading: u16,
    ready: bool,
    speed: F,
    recovery: u32,
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
        self.surface(points, color, solid, 0);
    }

    fn surface(&mut self, points: &[[F; 3]], color: u32, solid: bool, material: u8) {
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
        let mut previous = cut[1];
        for e in &cut[2..n] {
            let next = self.camera.screen(*e);
            if solid {
                let (ax, ay) = ((last.0 - first.0) >> 8, (last.1 - first.1) >> 8);
                let (bx, by) = ((next.0 - first.0) >> 8, (next.1 - first.1) >> 8);
                if ax * by - ay * bx <= 0 {
                    last = next;
                    previous = *e;
                    continue;
                }
            }
            let vertex = |e: Eye, screen: (i64, i64)| {
                let depth = (1i64 << 40) / e.z as i64;
                let x = self.camera.x + mul(e.x, self.camera.cos) + mul(e.z, self.camera.sin);
                let z = self.camera.z - mul(e.x, self.camera.sin) + mul(e.z, self.camera.cos);
                [
                    screen.0,
                    screen.1,
                    depth,
                    x as i64 * depth / ONE as i64,
                    z as i64 * depth / ONE as i64,
                ]
            };
            self.target.surface(
                [
                    vertex(cut[0], first),
                    vertex(previous, last),
                    vertex(*e, next),
                ],
                color,
                material,
            );
            previous = *e;
            last = next;
        }
    }

    /// Smooth normals, directional sunlight, a clear-coat highlight and sky
    /// reflections. Lighting is evaluated per vertex and interpolated in perspective.
    fn lit(
        &mut self,
        points: [[F; 3]; 3],
        normals: [[F; 3]; 3],
        color: u32,
        fog: u32,
        gloss: bool,
    ) {
        #[derive(Clone, Copy)]
        struct V {
            eye: Eye,
            rgb: [F; 3],
        }
        let vertex = |i: usize| {
            let at = points[i];
            let normal = normals[i];
            let light = [-28000, 50000, -32000];
            let dot = |a: [F; 3], b: [F; 3]| mul(a[0], b[0]) + mul(a[1], b[1]) + mul(a[2], b[2]);
            let norm = |v: [F; 3]| {
                let len =
                    isqrt(v.iter().map(|&x| x as i64 * x as i64).sum::<i64>() as u64).max(1) as i64;
                v.map(|x| (x as i64 * ONE as i64 / len) as F)
            };
            let view = norm([
                self.camera.x - at[0],
                self.camera.y - at[1],
                self.camera.z - at[2],
            ]);
            let half = norm([light[0] + view[0], light[1] + view[1], light[2] + view[2]]);
            let diffuse = dot(normal, light).clamp(0, ONE);
            let mut spec = dot(normal, half).clamp(0, ONE);
            for _ in 0..4 {
                spec = mul(spec, spec);
            }
            let rim = (ONE - dot(normal, view).clamp(0, ONE)).min(ONE);
            let rim = if gloss { mul(mul(rim, rim), rim) } else { 0 };
            let intensity = ONE / 3 + diffuse * 2 / 3;
            let rgb = core::array::from_fn(|c| {
                let shift = 16 - c * 8;
                let base = ((color >> shift) & 255) as i32;
                let sky = [35, 65, 85][c];
                let highlight = if gloss { mul(spec, f(190)) >> 16 } else { 0 };
                let value = (((base as i64 * intensity as i64) >> 16) as i32
                    + highlight
                    + (mul(rim, f(sky)) >> 16))
                    .clamp(0, 255);
                let haze = ((HAZE >> shift) & 255) as i32;
                ((value * (256 - fog as i32) + haze * fog as i32) / 256) * ONE
            });
            V {
                eye: self.camera.eye(at),
                rgb,
            }
        };
        let input = [vertex(0), vertex(1), vertex(2)];
        let mut cut = [input[0]; 5];
        let mut count = 0;
        for i in 0..3 {
            let a = input[i];
            let b = input[(i + 1) % 3];
            if a.eye.z >= NEAR {
                cut[count] = a;
                count += 1;
            }
            if (a.eye.z >= NEAR) != (b.eye.z >= NEAR) {
                let t = ((NEAR - a.eye.z) as i64 * ONE as i64 / (b.eye.z - a.eye.z) as i64) as F;
                let lerp = |x: F, y: F| x + mul(y - x, t);
                cut[count] = V {
                    eye: Eye {
                        x: lerp(a.eye.x, b.eye.x),
                        y: lerp(a.eye.y, b.eye.y),
                        z: NEAR,
                    },
                    rgb: core::array::from_fn(|c| lerp(a.rgb[c], b.rgb[c])),
                };
                count += 1;
            }
        }
        if count < 3 {
            return;
        }
        let screen = |v: V| {
            let (x, y) = self.camera.screen(v.eye);
            let z = (1i64 << 40) / v.eye.z as i64;
            [
                x,
                y,
                z,
                v.rgb[0] as i64 * z / ONE as i64,
                v.rgb[1] as i64 * z / ONE as i64,
                v.rgb[2] as i64 * z / ONE as i64,
            ]
        };
        for i in 1..count - 1 {
            self.target
                .lit([screen(cut[0]), screen(cut[i]), screen(cut[i + 1])]);
        }
    }

    fn shell(
        &mut self,
        at: [F; 3],
        radius: [F; 3],
        heading: u16,
        color: u32,
        fog: u32,
        helmet: bool,
    ) {
        let close = self.camera.eye(at).z < f(35);
        let (rings, sides) = if close { (10, 20) } else { (6, 12) };
        let rotate = |v: [F; 3]| {
            [
                mul(v[0], cos(heading)) + mul(v[2], sin(heading)),
                v[1],
                -mul(v[0], sin(heading)) + mul(v[2], cos(heading)),
            ]
        };
        for ring in 0..rings {
            for side in 0..sides {
                let a = (ring * 32768 / rings) as u16;
                let b = ((ring + 1) * 32768 / rings) as u16;
                let c = (side * 65536 / sides) as u16;
                let d = ((side + 1) * 65536 / sides) as u16;
                let vertex = |lat, lon| {
                    let unit = [mul(sin(lat), cos(lon)), cos(lat), mul(sin(lat), sin(lon))];
                    let normal = unit.map(|v| v);
                    let normal = core::array::from_fn(|j| {
                        ((normal[j] as i64 * ONE as i64) / radius[j].max(1) as i64) as F
                    });
                    let len = isqrt(normal.iter().map(|&x| x as i64 * x as i64).sum::<i64>() as u64)
                        .max(1) as i64;
                    let normal = rotate(normal.map(|x| (x as i64 * ONE as i64 / len) as F));
                    let offset = rotate(core::array::from_fn(|j| mul(radius[j], unit[j])));
                    (core::array::from_fn(|j| at[j] + offset[j]), normal)
                };
                let v = [vertex(a, c), vertex(a, d), vertex(b, d), vertex(b, c)];
                let normal = core::array::from_fn::<F, 3, _>(|j| {
                    (v[0].1[j] + v[1].1[j] + v[2].1[j] + v[3].1[j]) / 4
                });
                let toward = [
                    self.camera.x - v[0].0[0],
                    self.camera.y - v[0].0[1],
                    self.camera.z - v[0].0[2],
                ];
                if normal
                    .iter()
                    .zip(toward)
                    .map(|(&a, b)| a as i64 * b as i64)
                    .sum::<i64>()
                    <= 0
                {
                    continue;
                }
                let visor =
                    helmet && (rings * 3 / 10..rings * 6 / 10).contains(&ring) && side < sides / 2;
                let stripe = helmet && (side == sides / 4 || side == sides * 3 / 4);
                let paint = if visor {
                    0x163D58
                } else if stripe {
                    0xE85436
                } else {
                    color
                };
                for tri in [[0, 1, 2], [0, 2, 3]] {
                    self.lit(tri.map(|i| v[i].0), tri.map(|i| v[i].1), paint, fog, true);
                }
            }
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
        let t = ONE / 4;
        // Warm afternoon light casts a tapered shadow away from the sun.
        self.polygon(
            &[
                [at[0] - ONE, at[1] + ONE / 30, at[2]],
                [at[0] + ONE, at[1] + ONE / 30, at[2]],
                [at[0] + tall / 3, at[1] + ONE / 30, at[2] + tall / 2],
                [at[0] + tall / 6, at[1] + ONE / 30, at[2] + tall / 2],
            ],
            mix(0x3B6E58, HAZE, fog),
            false,
        );
        self.block(at, 0, [-t, t, 0, tall / 2, -t, t], TRUNK, fog);
        for tier in 0..3 {
            let base = tall / 5 + tier * tall / 5;
            let radius = tall / 3 - tier * tall / 14;
            let top = [at[0], at[1] + base + tall / 2, at[2]];
            for face in 0..6 {
                let a = (face * 65536 / 6) as u16;
                let b = ((face + 1) * 65536 / 6) as u16;
                let point = |angle: u16| {
                    let r = radius
                        + mul(
                            radius / 6,
                            sin(angle.wrapping_mul(5).wrapping_add((at[0] >> 8) as u16)),
                        );
                    [
                        at[0] + mul(r, sin(angle)),
                        at[1] + base + mul(ONE / 3, sin(angle.wrapping_mul(3))),
                        at[2] + mul(r, cos(angle)),
                    ]
                };
                let c = mix(leaves, 0xD3DB94, (face * 13 + tier * 9) as u32);
                self.polygon(&[top, point(a), point(b)], mix(c, HAZE, fog), false);
            }
        }
    }

    /// A low-poly ellipsoid: rounded tyres, helmet and rocks, with fixed light.
    fn gem(&mut self, at: [F; 3], radius: [F; 3], color: u32, fog: u32) {
        for ring in 0..4 {
            let lat0 = (ring * 32768 / 4) as u16;
            let lat1 = ((ring + 1) * 32768 / 4) as u16;
            for side in 0..8 {
                let a = (side * 8192) as u16;
                let b = a.wrapping_add(8192);
                let point = |lat, lon| {
                    [
                        at[0] + mul(radius[0], mul(sin(lat), cos(lon))),
                        at[1] + mul(radius[1], cos(lat)),
                        at[2] + mul(radius[2], mul(sin(lat), sin(lon))),
                    ]
                };
                let light = (mul(sin(a), sin(lat0)) / 1024 + 80).clamp(12, 145) as u32;
                let c = mix(mix(color, 0x101C2A, light), HAZE, fog);
                self.polygon(
                    &[
                        point(lat0, a),
                        point(lat0, b),
                        point(lat1, b),
                        point(lat1, a),
                    ],
                    c,
                    false,
                );
            }
        }
    }

    fn kart(&mut self, k: &Kart, color: u32, fog: u32, tick: u32) {
        let h = k.heading;
        let t = ONE / 10;
        let at = [k.x, k.y, k.z];
        let point = |x: F, y: F, z: F| {
            [
                k.x + mul(x, cos(h)) + mul(z, sin(h)),
                k.y + y,
                k.z - mul(x, sin(h)) + mul(z, cos(h)),
            ]
        };
        // Soft concentric contact shadows, blended onto the receiving terrain.
        for (radius, darken) in [(16, 228), (13, 210), (10, 202)] {
            for segment in 0..16 {
                let a = (segment * 4096) as u16;
                let b = a.wrapping_add(4096);
                let rim = |angle| {
                    point(
                        mul(radius * t, sin(angle)),
                        t,
                        mul(radius * t * 3 / 2, cos(angle)),
                    )
                };
                self.surface(&[point(0, t, 0), rim(a), rim(b)], darken, false, 4);
            }
        }
        if k.boost > 0 {
            let reach = f(2) + (sin((tick * 4900) as u16) + ONE) / 3;
            for x in [-4 * t, 4 * t] {
                self.polygon(
                    &[
                        point(x - 3 * t, 3 * t, -12 * t),
                        point(x + 3 * t, 3 * t, -12 * t),
                        point(x, 4 * t, -reach),
                    ],
                    0x29CDE5,
                    false,
                );
                self.polygon(
                    &[
                        point(x - 2 * t, 4 * t, -12 * t),
                        point(x + 2 * t, 4 * t, -12 * t),
                        point(x, 4 * t, -reach * 3 / 4),
                    ],
                    0xEDFFFF,
                    false,
                );
            }
        }
        for x in [-9 * t, 9 * t] {
            for z in [-8 * t, 8 * t] {
                let at = point(x, 4 * t, z);
                // Cylindrical tyres rotate with the wheel's steering angle.
                let heading = h.wrapping_add(if z > 0 {
                    (k.steer / 20) as i16 as u16
                } else {
                    0
                });
                let wheel_point = |side: F, angle| {
                    let y = mul(4 * t, sin(angle));
                    let z = mul(4 * t, cos(angle));
                    [
                        at[0] + mul(side, cos(heading)) + mul(z, sin(heading)),
                        at[1] + y,
                        at[2] - mul(side, sin(heading)) + mul(z, cos(heading)),
                    ]
                };
                for n in 0..16 {
                    let a = (n * 4096) as u16;
                    let b = a.wrapping_add(4096);
                    self.polygon(
                        &[
                            wheel_point(-2 * t, a),
                            wheel_point(2 * t, a),
                            wheel_point(2 * t, b),
                            wheel_point(-2 * t, b),
                        ],
                        mix(if n % 2 == 0 { 0x182431 } else { 0x253442 }, HAZE, fog),
                        false,
                    );
                    for side in [-2 * t, 2 * t] {
                        let center = wheel_point(side, 0);
                        let center = [
                            center[0] - mul(4 * t, sin(heading)),
                            at[1],
                            center[2] - mul(4 * t, cos(heading)),
                        ];
                        self.polygon(
                            &[center, wheel_point(side, a), wheel_point(side, b)],
                            mix(0x18242D, HAZE, fog),
                            false,
                        );
                    }
                }
                for side in [-1, 1] {
                    let hub = [
                        at[0] + mul(side * 21 * t / 10, cos(heading)),
                        at[1],
                        at[2] - mul(side * 21 * t / 10, sin(heading)),
                    ];
                    self.shell(hub, [t / 4, 2 * t, 2 * t], heading, 0xB2C8D6, fog, false);
                }
            }
        }
        // Sculpted bodywork instead of boxes: clear-coated pods and nose.
        self.shell(
            point(0, 5 * t, 0),
            [8 * t, 3 * t, 15 * t],
            h,
            color,
            fog,
            false,
        );
        for side in [-1, 1] {
            self.shell(
                point(side * 8 * t, 5 * t, 0),
                [3 * t, 3 * t, 10 * t],
                h,
                color,
                fog,
                false,
            );
        }
        self.shell(
            point(0, 6 * t, 8 * t),
            [4 * t, 2 * t, 9 * t],
            h,
            0xF6E7BC,
            fog,
            false,
        );
        self.shell(
            point(0, 8 * t, -3 * t),
            [4 * t, 4 * t, 5 * t],
            h,
            0x142632,
            fog,
            false,
        );
        self.shell(
            point(0, 11 * t, -3 * t),
            [3 * t, 4 * t, 3 * t],
            h,
            color,
            fog,
            false,
        );
        self.shell(
            point(0, 16 * t, -3 * t),
            [4 * t, 4 * t, 4 * t],
            h,
            0xEDE9DE,
            fog,
            true,
        );
        for x in [-5 * t, 5 * t] {
            self.block(
                at,
                h,
                [x - t / 2, x + t / 2, 5 * t, 10 * t, -11 * t, -10 * t],
                0x263748,
                fog,
            );
        }
        self.block(
            at,
            h,
            [-9 * t, 9 * t, 10 * t, 11 * t, -14 * t, -10 * t],
            color,
            fog,
        );
        self.block(
            at,
            h,
            [-6 * t, 6 * t, 7 * t, 8 * t, -13 * t, -12 * t],
            0xFFB592,
            fog,
        );
        if k.drifting {
            let spark = if k.drift_charge >= 90 {
                0xFFB947
            } else if k.drift_charge >= 40 {
                0x64E6FF
            } else {
                0xC7D3CE
            };
            for side in [-1, 1] {
                for n in 0..5 {
                    let seed = (tick.wrapping_mul(13) + n * 37 + (side + 1) as u32 * 17) % 61;
                    let x = side * (10 * t + seed as i32 * t / 18);
                    let z = -10 * t - n as i32 * 4 * t;
                    let y = t + seed as i32 * t / 40;
                    self.polygon(
                        &[
                            point(x, y, z),
                            point(x + side * 2 * t, y + t, z - t),
                            point(x, y + 2 * t, z - 3 * t),
                        ],
                        spark,
                        false,
                    );
                }
            }
        }
    }
}

/// The sky, with its gradient, a line of far hills that slide as the camera
/// turns, and the ground as far as the road's end.
fn backdrop(target: &mut Target, horizon: usize, heading: u16) {
    let w = target.width as i64;
    let h = target.height as i64;
    for y in 0..target.height {
        let color = if y < horizon {
            mix(SKY_TOP, SKY_LOW, (y * 256 / horizon.max(1)) as u32)
        } else {
            mix(
                GRASS[1],
                HAZE,
                150u32.saturating_sub(((y - horizon) * 700 / target.height.max(1)) as u32),
            )
        };
        target.fill_rows(y, y + 1, color);
    }
    // Distant sun and soft halo, anchored in the sky as the camera turns.
    let sun_x = w / 2 + ((8000u16.wrapping_sub(heading) as i16 as i64) * w / 19000);
    let sun_y = horizon as i64 * 2 / 5;
    let radius = (h / 20).max(1);
    for y in (sun_y - radius * 3).max(0)..(sun_y + radius * 3).min(horizon as i64) {
        for x in (sun_x - radius * 3).max(0)..(sun_x + radius * 3).min(w) {
            let d = (x - sun_x) * (x - sun_x) + (y - sun_y) * (y - sun_y);
            if d < radius * radius * 9 {
                let i = (y * w + x) as usize;
                let glow = if d < radius * radius {
                    256
                } else {
                    ((radius * radius * 9 - d) * 65 / (radius * radius * 8)) as u32
                };
                target.pixels[i] = mix(target.pixels[i], 0xFFF3CD, glow);
            }
        }
    }
    // Soft cumulus clouds sampled in sky space; smoothly interpolate the
    // noise so silhouettes stay stable when the camera turns.
    let noise = |x: i64, y: i64, period: i64| {
        let (ix, iy) = (x >> 16, y >> 16);
        let smooth = |t: i64| (((t * t) >> 16) * (3 * 65536 - 2 * t)) >> 16;
        let (tx, ty) = (smooth(x & 65535), smooth(y & 65535));
        let hash = |xx: i64, yy: i64| {
            let a = (xx.rem_euclid(period) as u32).wrapping_mul(374761393)
                ^ (yy as u32).wrapping_mul(668265263);
            ((a ^ (a >> 13)).wrapping_mul(1274126177) >> 16) as i64
        };
        let a = hash(ix, iy);
        let b = hash(ix + 1, iy);
        let c = hash(ix, iy + 1);
        let d = hash(ix + 1, iy + 1);
        let top = a + (((b - a) * tx) >> 16);
        let bottom = c + (((d - c) * tx) >> 16);
        top + (((bottom - top) * ty) >> 16)
    };
    let cell = (w / 480).max(1) as usize;
    for y in (0..horizon).step_by(cell) {
        for x in (0..target.width).step_by(cell) {
            let angle = heading.wrapping_add(((x as i64 - w / 2) * 21000 / w.max(1)) as u16) as i64;
            let yy = y as i64 * 4 * 65536 / (horizon as i64).max(1);
            let n = (noise(angle * 12, yy, 12) * 4
                + noise(angle * 24, yy * 2, 24) * 2
                + noise(angle * 72, yy * 6, 72))
                / 7;
            let opacity = ((n - 35500) * 256 / 15000).clamp(0, 220) as u32;
            if opacity == 0 {
                continue;
            }
            let shade = ((n - 35000) / 160).clamp(0, 120) as u32;
            let cloud = mix(0xB1B6C4, 0xFFF0DC, 100 + shade);
            for dy in 0..cell {
                if y + dy >= horizon {
                    break;
                }
                for dx in 0..cell {
                    if x + dx >= target.width {
                        break;
                    }
                    let i = (y + dy) * target.width + x + dx;
                    target.pixels[i] = mix(target.pixels[i], cloud, opacity);
                }
            }
        }
    }
    // Angular ridgelines at three distances. Snow follows altitude and slope;
    // the lit/shaded gradients give the mountains volume rather than flat bands.
    let ridge = |angle: u16, seed: u32| -> i64 {
        let noise = |scale: u32| {
            let p = angle as u32 * scale;
            let cell = p >> 16;
            let t = (p & 65535) as i64;
            let hash = |i: u32| {
                let v = i.wrapping_mul(374761393).wrapping_add(seed * 668265263);
                ((v ^ (v >> 13)) & 65535) as i64
            };
            let a = hash(cell % scale);
            let b = hash((cell + 1) % scale);
            a + (b - a) * t / 65536
        };
        (noise(13) * 6 + noise(29) * 3 + noise(71)) / 10
    };
    for layer in 0..3i64 {
        for x in 0..w {
            let a = heading.wrapping_add(((x - w / 2) * 21000 / w.max(1)) as u16);
            let peak = ridge(a, layer as u32 + 1);
            let next = ridge(a.wrapping_add(100), layer as u32 + 1);
            let tall = h * (10 - layer * 2) / 100 + peak * h * (30 - layer * 6) / (65536 * 100);
            let base = horizon as i64 + layer * h / 20;
            let top = base - tall;
            let slope = ((next - peak) / 150).clamp(-45, 45);
            let stone = [0x8D9DA8, 0x65848A, 0x366D66][layer as usize];
            for y in top.max(0)..base.min(h) {
                let down = y - top;
                let haze = (down * 100 / tall.max(1)).clamp(0, 100) as u32;
                let snow = layer == 0
                    && down < tall / 3 + (sin(a.wrapping_mul(23)) as i64 * h / 65536 / 100)
                    && tall > h / 5;
                let color = if snow {
                    mix(0xFFF0DF, 0x91A9BE, (50 + slope).max(0) as u32)
                } else {
                    mix(stone, 0x1D4259, (45 + slope).max(0) as u32)
                };
                target.pixels[(y * w + x) as usize] = mix(color, HAZE, haze);
            }
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

/// Terrain shoulders for segment `i`, slightly below the road surface.
fn grass(p: &mut Painter, world: &World, i: usize, fog: u32) {
    let (mut s, mut n) = (*world.track.at(i), *world.track.at(i + 1));
    s.y -= ONE / 6;
    n.y -= ONE / 6;
    let c = mix(GRASS[(i % SEGMENTS / 4) & 1], HAZE, fog);
    // Terrain shoulders stop at the asphalt. A full-width ground quad can
    // intersect the road at bends, creating green cracks across the surface.
    for (from, to) in [(-GRASS_WIDTH, -HALF_WIDTH), (HALF_WIDTH, GRASS_WIDTH)] {
        p.surface(&band(&s, &n, from, to), c, false, 2);
    }
}

/// The road of segment `i`.
fn road(p: &mut Painter, world: &World, i: usize, fog: u32) {
    let (mut s, mut n) = (*world.track.at(i), *world.track.at(i + 1));
    s.y += ONE / 24;
    n.y += ONE / 24;
    let index = i % SEGMENTS;
    let hue = |pair: [u32; 2], every: usize| mix(pair[(index / every) & 1], HAZE, fog);
    p.surface(
        &band(&s, &n, -HALF_WIDTH, HALF_WIDTH),
        hue(ROAD, 3),
        false,
        1,
    );
    let edge = HALF_WIDTH + CURB;
    p.polygon(&band(&s, &n, -edge, -HALF_WIDTH), hue(CURBS, 2), false);
    p.polygon(&band(&s, &n, HALF_WIDTH, edge), hue(CURBS, 2), false);
    // Lift paint slightly above the asphalt, avoiding coplanar depth noise.
    s.y += ONE / 16;
    n.y += ONE / 16;
    // Continuous edge paint gives the driver a readable racing surface.
    for side in [-1, 1] {
        let edge = side * (HALF_WIDTH - ONE / 3);
        p.polygon(
            &band(&s, &n, edge - ONE / 12, edge + ONE / 12),
            mix(0xF4DEAE, HAZE, fog),
            false,
        );
    }
    if is_turbo(index) {
        p.polygon(&band(&s, &n, -f(5), f(5)), mix(0x185B68, HAZE, fog), false);
        // Three bright arrows across each strip, facing the racing direction.
        for lane in [-3, 0, 3] {
            let mid = |x: F, weight: F| {
                let a = band(&s, &n, x, x)[0];
                let b = band(&s, &n, x, x)[3];
                [
                    a[0] + mul(b[0] - a[0], weight),
                    a[1] + mul(b[1] - a[1], weight) + ONE / 16,
                    a[2] + mul(b[2] - a[2], weight),
                ]
            };
            let x = f(lane);
            p.polygon(
                &[
                    mid(x - ONE, ONE / 6),
                    mid(x, ONE * 5 / 6),
                    mid(x + ONE, ONE / 6),
                ],
                hue(TURBO, 1),
                false,
            );
        }
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
    let point = |off: F, up: F| {
        [
            s.x + mul(off, s.right_x),
            s.y + up,
            s.z + mul(off, s.right_z),
        ]
    };
    if index.is_multiple_of(6) {
        let code = index.wrapping_mul(2654435761) >> 7;
        for side in [-1, 1] {
            let off = side * (HALF_WIDTH + f(7) + f((code % 17) as i32));
            let at = point(off, 0);
            if code.is_multiple_of(3) {
                p.block(
                    at,
                    0,
                    [-ONE / 4, ONE / 4, 0, f(5), -ONE / 4, ONE / 4],
                    TRUNK,
                    fog,
                );
                for (dx, dy, dz, r) in [(-2, 5, 0, 3), (2, 6, 1, 3), (0, 8, 0, 3)] {
                    p.gem(
                        [at[0] + f(dx), at[1] + f(dy), at[2] + f(dz)],
                        [f(r), f(r), f(r)],
                        if code & 4 == 0 { 0x668847 } else { 0xAD8F47 },
                        fog,
                    );
                }
            } else {
                p.tree(
                    at,
                    f(7) + f((code % 5) as i32),
                    LEAVES[(code >> 3) & 1],
                    fog,
                );
            }
        }
    }
    // Grandstands on the starting straight. Spectators and seats are geometry,
    // so they stay anchored and occlude behind the rails like everything else.
    if index == 10 || index == 28 {
        let at = point(-f(19), 0);
        let heading = crate::world::heading_of(-s.right_z, s.right_x);
        let local = |x: F, y: F, z: F| {
            [
                at[0] + mul(x, s.right_x) - mul(z, s.right_z),
                at[1] + y,
                at[2] + mul(x, s.right_z) + mul(z, s.right_x),
            ]
        };
        for tier in 0..3 {
            let x = -f(tier * 2);
            p.block(
                at,
                heading,
                [x - f(2), x, f(tier), f(tier + 1), -f(7), f(7)],
                0x33596B,
                fog,
            );
            for seat in 0..10 {
                let color = [0xF3C875, 0xDF7858, 0x71BEC0, 0xC5D7CB][(seat + tier) as usize % 4];
                p.block(
                    local(x - ONE, f(tier + 1), f(seat) - f(5)),
                    heading,
                    [-ONE / 4, ONE / 4, 0, ONE / 2, -ONE / 4, ONE / 4],
                    color,
                    fog,
                );
            }
        }
        for z in [-f(7), f(7)] {
            p.block(
                local(-f(5), 0, z),
                heading,
                [-ONE / 8, ONE / 8, 0, f(6), -ONE / 8, ONE / 8],
                0xD6C6A2,
                fog,
            );
        }
        p.polygon(
            &[
                local(-f(7), f(6), -f(8)),
                local(ONE, f(5), -f(8)),
                local(ONE, f(5), f(8)),
                local(-f(7), f(6), f(8)),
            ],
            mix(0xDC7957, HAZE, fog),
            false,
        );
    }
    if index == 165 || index == 310 || index == 420 {
        let at = point(f(21), 0);
        p.block(at, 0, [-f(4), f(4), 0, f(5), -f(3), f(3)], 0xD6B98D, fog);
        let q = |x: F, y: F, z: F| [at[0] + x, at[1] + y, at[2] + z];
        p.polygon(
            &[
                q(-f(5), f(5), -f(4)),
                q(0, f(8), -f(4)),
                q(0, f(8), f(4)),
                q(-f(5), f(5), f(4)),
            ],
            mix(0xA45545, HAZE, fog),
            false,
        );
        p.polygon(
            &[
                q(0, f(8), -f(4)),
                q(f(5), f(5), -f(4)),
                q(f(5), f(5), f(4)),
                q(0, f(8), f(4)),
            ],
            mix(0xD78660, HAZE, fog),
            false,
        );
        p.polygon(
            &[
                q(-f(4), f(5), -f(3)),
                q(0, f(8), -f(3)),
                q(f(4), f(5), -f(3)),
            ],
            mix(0xE0C69A, HAZE, fog),
            false,
        );
        for x in [-f(2), f(2)] {
            p.block(
                q(x, f(2), -f(3) - ONE / 20),
                0,
                [-ONE / 2, ONE / 2, 0, ONE, -ONE / 20, 0],
                0x294F60,
                fog,
            );
        }
        p.block(
            q(f(2), f(6), ONE),
            0,
            [-ONE / 2, ONE / 2, 0, f(3), -ONE / 2, ONE / 2],
            0x7A6C61,
            fog,
        );
    }
    // Pennants move in a deterministic breeze alongside the finish approach.
    if index.is_multiple_of(12) && !(42..=440).contains(&index) {
        let at = point(HALF_WIDTH + f(3), 0);
        p.block(
            at,
            0,
            [-ONE / 12, ONE / 12, 0, f(5), -ONE / 12, ONE / 12],
            0xD4C5A4,
            fog,
        );
        let flutter = sin((world.step * 260 + index as u32 * 1200) as u16) / 3;
        p.polygon(
            &[
                [at[0], at[1] + f(5), at[2]],
                [at[0] + f(2), at[1] + f(4), at[2] + flutter],
                [at[0], at[1] + f(3), at[2]],
            ],
            mix(PAINT[index / 12 % KARTS], HAZE, fog),
            false,
        );
    }
    // Guardrails follow the terrain, with small reflective posts.
    if index.is_multiple_of(3) {
        for side in [-1, 1] {
            let off = side * (HALF_WIDTH + f(2));
            let end = world.track.at(i + 3);
            let a = point(off, ONE / 2);
            let b = [
                end.x + mul(off, end.right_x),
                end.y + ONE / 2,
                end.z + mul(off, end.right_z),
            ];
            p.polygon(
                &[
                    a,
                    b,
                    [b[0], b[1] + ONE / 3, b[2]],
                    [a[0], a[1] + ONE / 3, a[2]],
                ],
                mix(0xCAD4C9, HAZE, fog),
                false,
            );
            p.block(
                point(off, 0),
                0,
                [-ONE / 10, ONE / 10, 0, ONE, -ONE / 10, ONE / 10],
                0x526D73,
                fog,
            );
            p.block(
                point(off, ONE),
                0,
                [-ONE / 8, ONE / 8, 0, ONE / 5, -ONE / 8, ONE / 8],
                0xFFBB61,
                fog,
            );
        }
    }
    // Trackside banners, with graphic arrows visible before the bends.
    if index % 40 == 20 {
        let off = -HALF_WIDTH - f(3);
        let a = point(off, f(2));
        p.block(
            point(off, 0),
            0,
            [-ONE / 8, ONE / 8, 0, f(4), -ONE / 8, ONE / 8],
            0x384D58,
            fog,
        );
        let q = |x: F, y: F| [a[0] + mul(x, s.right_x), a[1] + y, a[2] + mul(x, s.right_z)];
        p.polygon(
            &[q(-f(2), 0), q(f(2), 0), q(f(2), f(2)), q(-f(2), f(2))],
            mix(0x142F43, HAZE, fog),
            false,
        );
        for x in [-ONE, ONE] {
            p.polygon(
                &[
                    q(x - ONE / 2, ONE / 3),
                    q(x + ONE / 2, ONE),
                    q(x - ONE / 2, ONE * 5 / 3),
                ],
                mix(0xFFE2A2, HAZE, fog),
                false,
            );
        }
    }
    if index % 31 == 12 {
        let off = HALF_WIDTH + f(12);
        p.gem(point(off, ONE), [f(3), f(2), f(2)], 0x839799, fog);
    }
    if index == 0 {
        arch(p, &s, fog);
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
    for x in -9..9 {
        for y in 0..2 {
            let a = point(f(x), f(5 + y));
            let b = point(f(x + 1), f(6 + y));
            p.polygon(
                &[a, point(f(x + 1), f(5 + y)), b, point(f(x), f(6 + y))],
                mix(
                    if (x + y) & 1 == 0 { 0x183242 } else { 0xFFE9BD },
                    HAZE,
                    fog,
                ),
                false,
            );
        }
    }
}

/// Draws kart 0's view of `world` on `target`.
pub fn draw(world: &World, view: &mut View, target: &mut Target) {
    target.depth.fill(0);
    let you = &world.karts[0];
    if !view.ready || you.recovery > view.recovery {
        view.heading = you.heading;
        view.ready = true;
    }
    view.recovery = you.recovery;
    let lag = you.heading.wrapping_sub(view.heading) as i16 as i32;
    view.heading = view.heading.wrapping_add((lag / 5) as i16 as u16);
    view.speed += (you.speed - view.speed) / 8;
    let (s, c) = (sin(view.heading), cos(view.heading));
    let pullback = CAMERA_BACK + view.speed / 30;
    let (w, h) = (target.width as i64, target.height as i64);
    let horizon = h * 9 / 25;
    let camera = Camera {
        x: you.x - mul(pullback, s),
        y: you.y + CAMERA_UP,
        z: you.z - mul(pullback, c),
        sin: s,
        cos: c,
        focal: ((w * 86 / 100) << 16) - w * view.speed as i64 / 500,
        center: (w << 16) / 2,
        horizon: horizon << 16,
    };
    backdrop(target, horizon as usize, view.heading);
    let mut p = Painter { target, camera };
    let camera = p.camera;
    let segments = || {
        (0..SEGMENTS).filter_map(|i| {
            let s = world.track.at(i);
            let e = camera.eye([s.x, s.y, s.z]);
            // Include shoulders/trees near the edge of the frustum and behind the
            // near plane, where they can still cross into view after clipping.
            if e.z < -f(40) || e.z > FAR || e.x.abs() > e.z.max(0) + f(60) {
                return None;
            }
            let distance = crate::fixed::length(s.x - camera.x, s.z - camera.z);
            let fog = ((distance as i64 * distance as i64 * 210) / (FAR as i64 * FAR as i64))
                .clamp(0, 210) as u32;
            Some((i, fog))
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
    // Vehicles use the same depth buffer as the terrain, including behind us.
    for (kart, paint) in world.karts.iter().zip(PAINT) {
        let eye = p.camera.eye([kart.x, kart.y, kart.z]);
        if eye.z > 0 && eye.z < f(260) {
            let fog = ((eye.z as i64 * 210 / f(260) as i64).clamp(0, 210)) as u32;
            p.kart(kart, paint, fog, world.step);
        }
    }
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
        let mut depth = vec![0; pixels.len()];
        let mut target = Target {
            depth: &mut depth,
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
            px.iter().any(|c| (c >> 16) & 255 > 55
                && (c >> 16) & 255 < 80
                && (c & 255) > 70
                && (c & 255) < 105),
            "the road"
        );
        assert!(
            px.iter()
                .any(|c| ((c >> 16) & 255) > 210 && ((c >> 8) & 255) < 160 && (c & 255) < 140),
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
                let mut depth = vec![0; pixels.len()];
                let mut target = Target {
                    depth: &mut depth,
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
        let mut depth = vec![0; pixels.len()];
        let mut target = Target {
            depth: &mut depth,
            pixels: &mut pixels,
            width: 8,
            height: 4,
        };
        draw(&world, &mut View::default(), &mut target);
    }
}
