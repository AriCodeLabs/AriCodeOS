// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Shadows from a light far away: the scene's depth as the light sees it, drawn again
//! whenever the light or the scene moves, then asked for each pixel drawn whether
//! something nearer the light hides it. What a GPU does with a depth-only pass and a
//! comparison in the fragment program.

use crate::math::{Mat4, Vec3};
use crate::mesh::Mesh;
use crate::raster::{cover, edge, to_screen, Light};

/// Depth 1 in the map's integers.
const ONE: f32 = (1u32 << 24) as f32;
/// How much nearer the light an occluder must be, in the map's integers: only the
/// sides turned away from the light are drawn into the map, so a lit surface never
/// meets its own depth and the margin can stay this small.
const BIAS: u32 = 1 << 12;

pub struct ShadowMap<'a> {
    depth: &'a mut [u32],
    size: usize,
    view_proj: Mat4,
}

impl<'a> ShadowMap<'a> {
    /// A map of `size` x `size` held in `depth`, seeing from `light` all within
    /// `radius` of `centre`, empty. `None` when `depth` is too short or the box empty.
    pub fn new(
        depth: &'a mut [u32],
        size: usize,
        light: &Light,
        centre: Vec3,
        radius: f32,
    ) -> Option<Self> {
        let cells = size.checked_mul(size)?;
        if size == 0 || depth.len() < cells || !radius.is_finite() || radius <= 0.0 {
            return None;
        }
        let d = light.direction;
        let eye = centre + d * (2.0 * radius);
        // Any up will do that is not along the light.
        let up = if d.y.abs() > 0.9 {
            Vec3::new(1.0, 0.0, 0.0)
        } else {
            Vec3::new(0.0, 1.0, 0.0)
        };
        let view_proj = Mat4::orthographic(radius, radius, radius, 3.0 * radius)
            * Mat4::look_at(eye, centre, up);
        let depth = &mut depth[..cells];
        depth.fill(u32::MAX);
        Some(Self {
            depth,
            size,
            view_proj,
        })
    }

    /// Adds `mesh`, placed by `model`, to what hides the light; how many of its
    /// triangles reached the map.
    pub fn cast(&mut self, mesh: &Mesh, model: &Mat4) -> u64 {
        let m = self.view_proj * *model;
        let n = self.size;
        let depth = &mut *self.depth;
        let mut cast = 0;
        for tri in mesh.indices.as_chunks::<3>().0 {
            let mut s = [None; 3];
            for (c, &i) in s.iter_mut().zip(tri) {
                *c = mesh
                    .vertices
                    .get(i as usize)
                    .and_then(|v| to_screen(m.transform(v.pos, 1.0), n, n));
            }
            let [Some(a), Some(b), Some(c)] = s else {
                continue;
            };
            let area = edge((a.x, a.y), (b.x, b.y), (c.x, c.y));
            // Only the sides turned away from the light; walked the other way round,
            // so they are counter-clockwise for `cover` as a facing side would be.
            if area >= 0 {
                continue;
            }
            let p = [(a.x, a.y), (c.x, c.y), (b.x, b.y)];
            let z = [a.z, c.z, b.z];
            let inv = 1.0 / (-area) as f32;
            let reached = cover(p, n, n, n, |at, e| {
                let l = (e[0] as f32 * z[0] + e[1] as f32 * z[1] + e[2] as f32 * z[2]) * inv;
                let d = (l.clamp(0.0, 1.0) * ONE) as u32;
                if d < depth[at] {
                    depth[at] = d;
                }
            });
            cast += u64::from(reached.is_some());
        }
        cast
    }

    /// Whether something drawn into the map lies between `world` and the light.
    pub fn hides(&self, world: Vec3) -> bool {
        let [x, y, z, _] = self.view_proj.transform(world, 1.0);
        let n = self.size as f32;
        let (sx, sy) = ((x * 0.5 + 0.5) * n, (0.5 - y * 0.5) * n);
        if !((0.0..n).contains(&sx) && (0.0..n).contains(&sy)) {
            return false;
        }
        let at = sy as usize * self.size + sx as usize;
        let d = (z.clamp(0.0, 1.0) * ONE) as u32;
        d > self.depth[at].saturating_add(BIAS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn light(towards: Vec3) -> Light {
        Light {
            direction: towards.normalized(),
            ambient: 0.2,
        }
    }

    /// A map holding a cube of side 2 lifted to span 1 to 3 above the ground (y = 0).
    fn with_cube<'a>(depth: &'a mut [u32], light: &Light) -> ShadowMap<'a> {
        let mut m = ShadowMap::new(depth, 256, light, Vec3::default(), 6.0).unwrap();
        let cube = Mesh::cube().unwrap();
        assert!(m.cast(&cube, &Mat4::translate(Vec3::new(0.0, 2.0, 0.0))) > 0);
        m
    }

    #[test]
    fn a_light_above_hides_the_ground_under_the_cube_and_not_the_cube() {
        let mut depth = vec![0u32; 256 * 256];
        let m = with_cube(&mut depth, &light(Vec3::new(0.0, 1.0, 0.0)));
        assert!(m.hides(Vec3::new(0.0, 0.0, 0.0)));
        assert!(m.hides(Vec3::new(0.8, 0.0, -0.8)));
        assert!(!m.hides(Vec3::new(2.5, 0.0, 0.0)));
        // Its own top faces the light: no acne on it.
        for (x, z) in [(0.0, 0.0), (0.5, 0.5), (-0.9, 0.7)] {
            assert!(!m.hides(Vec3::new(x, 3.0, z)), "top at {x} {z}");
        }
    }

    #[test]
    fn a_light_from_the_side_moves_the_shadow_away_from_it() {
        // Light up and to +x: the shadow falls toward -x.
        let mut depth = vec![0u32; 256 * 256];
        let m = with_cube(&mut depth, &light(Vec3::new(1.0, 1.0, 0.0)));
        assert!(m.hides(Vec3::new(-2.5, 0.0, 0.0)));
        assert!(!m.hides(Vec3::new(2.5, 0.0, 0.0)));
        // The side facing the light is lit.
        assert!(!m.hides(Vec3::new(1.0, 2.0, 0.0)));
    }

    #[test]
    fn a_short_buffer_or_an_empty_box_is_refused() {
        let light = light(Vec3::new(0.0, 1.0, 0.0));
        let mut short = vec![0u32; 15];
        assert!(ShadowMap::new(&mut short, 4, &light, Vec3::default(), 1.0).is_none());
        let mut enough = vec![0u32; 16];
        assert!(ShadowMap::new(&mut enough, 4, &light, Vec3::default(), 0.0).is_none());
        assert!(ShadowMap::new(&mut enough, 4, &light, Vec3::default(), f32::NAN).is_none());
        assert!(ShadowMap::new(&mut enough, 0, &light, Vec3::default(), 1.0).is_none());
    }
}
