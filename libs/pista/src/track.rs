// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The circuit: a closed curve through a few points, cut into segments.

use crate::fixed::{f, length, mul, ratio, F, ONE};

pub const SEGMENTS: usize = 480;
/// Half the road's width.
pub const HALF_WIDTH: F = f(7);

/// A point on the centre line and the direction to its right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Segment {
    pub x: F,
    pub y: F,
    pub z: F,
    pub right_x: F,
    pub right_z: F,
}

/// The circuit's points in metres: x, height, z.
const POINTS: [(i32, i32, i32); 16] = [
    (0, 0, -72),
    (0, 0, 24),
    (12, 2, 66),
    (54, 8, 78),
    (96, 12, 60),
    (102, 12, 18),
    (72, 8, -6),
    (66, 4, -42),
    (102, 2, -66),
    (132, 0, -36),
    (156, 2, 18),
    (174, 6, 66),
    (198, 8, 24),
    (192, 4, -78),
    (120, 0, -114),
    (36, 0, -114),
];

/// Catmull-Rom between `p1` and `p2`, at `t` of the way.
fn catmull(p0: i32, p1: i32, p2: i32, p3: i32, t: F) -> F {
    let (a, b, c, d) = (f(p0) as i64, f(p1) as i64, f(p2) as i64, f(p3) as i64);
    let t = t as i64;
    let t2 = (t * t) >> 16;
    let t3 = (t2 * t) >> 16;
    let v = 2 * b
        + (((c - a) * t) >> 16)
        + (((2 * a - 5 * b + 4 * c - d) * t2) >> 16)
        + (((3 * b - a - 3 * c + d) * t3) >> 16);
    (v / 2) as F
}

pub struct Track {
    pub segments: [Segment; SEGMENTS],
}

impl Track {
    pub fn new() -> Self {
        let mut segments = [Segment::default(); SEGMENTS];
        let n = POINTS.len();
        for (i, s) in segments.iter_mut().enumerate() {
            let along = i * n;
            let span = along / SEGMENTS;
            let t = ratio((along % SEGMENTS) as i32, SEGMENTS as i32);
            let at = |k: usize| POINTS[(span + k) % n];
            let (p0, p1, p2, p3) = (at(n - 1), at(0), at(1), at(2));
            s.x = catmull(p0.0, p1.0, p2.0, p3.0, t);
            s.y = catmull(p0.1, p1.1, p2.1, p3.1, t);
            s.z = catmull(p0.2, p1.2, p2.2, p3.2, t);
        }
        // Forward, by the neighbours; right of forward in the plane.
        for i in 0..SEGMENTS {
            let (a, b) = (
                segments[(i + SEGMENTS - 1) % SEGMENTS],
                segments[(i + 1) % SEGMENTS],
            );
            let (dx, dz) = (b.x - a.x, b.z - a.z);
            let len = length(dx, dz).max(1);
            segments[i].right_x = ((dz as i64 * ONE as i64) / len as i64) as F;
            segments[i].right_z = (-(dx as i64) * ONE as i64 / len as i64) as F;
        }
        Self { segments }
    }

    pub fn at(&self, i: usize) -> &Segment {
        &self.segments[i % SEGMENTS]
    }

    /// The segment nearest to (x, z), looking around `from` only: the circuit
    /// passes near itself, and a kart moves few segments per frame.
    pub fn nearest(&self, from: usize, x: F, z: F) -> usize {
        let mut best = (i64::MAX, from);
        for k in 0..24 {
            let i = (from + SEGMENTS + k - 6) % SEGMENTS;
            let s = &self.segments[i];
            let (dx, dz) = ((x - s.x) as i64, (z - s.z) as i64);
            let d = dx * dx + dz * dz;
            if d < best.0 {
                best = (d, i);
            }
        }
        best.1
    }

    /// How far right of the centre line segment `i` puts (x, z).
    pub fn lateral(&self, i: usize, x: F, z: F) -> F {
        let s = self.at(i);
        mul(x - s.x, s.right_x) + mul(z - s.z, s.right_z)
    }
}

impl Default for Track {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::length as len;

    #[test]
    fn the_circuit_closes_without_a_jump() {
        let t = Track::new();
        for i in 0..SEGMENTS {
            let (a, b) = (t.at(i), t.at(i + 1));
            let step = len(b.x - a.x, b.z - a.z);
            assert!(step > f(1) / 8 && step < f(8), "segment {i}: {step}");
            assert!((b.y - a.y).abs() < f(1), "segment {i} climbs too fast");
        }
    }

    #[test]
    fn right_vectors_are_unit_and_square_to_the_road() {
        let t = Track::new();
        for i in 0..SEGMENTS {
            let s = t.at(i);
            assert!((len(s.right_x, s.right_z) - ONE).abs() < 8, "{i}");
            let (a, b) = (t.at(i + SEGMENTS - 1), t.at(i + 1));
            let along = mul(b.x - a.x, s.right_x) + mul(b.z - a.z, s.right_z);
            assert!(along.abs() < ONE / 8, "{i}: {along}");
        }
    }

    /// Two stretches of road never touch: nothing draws over the wrong one, and
    /// `nearest` cannot jump to another part of the circuit.
    #[test]
    fn distant_stretches_stay_apart() {
        let t = Track::new();
        for i in (0..SEGMENTS).step_by(3) {
            for j in (i + 60..SEGMENTS).step_by(3) {
                if i + SEGMENTS - j < 60 {
                    continue;
                }
                let (a, b) = (t.at(i), t.at(j));
                let d = len((a.x - b.x) >> 4, (a.z - b.z) >> 4) << 4;
                assert!(d > 2 * HALF_WIDTH + f(6), "{i} and {j}: {}", d >> 16);
            }
        }
    }

    #[test]
    fn the_nearest_segment_follows_along_and_ignores_the_rest() {
        let t = Track::new();
        for i in 0..SEGMENTS {
            let s = t.at(i);
            assert_eq!(t.nearest(i, s.x, s.z), i);
            assert_eq!(t.lateral(i, s.x, s.z), 0);
            let (x, z) = (s.x + 3 * s.right_x, s.z + 3 * s.right_z);
            assert!((t.lateral(i, x, z) - f(3)).abs() < ONE / 32);
        }
    }
}
