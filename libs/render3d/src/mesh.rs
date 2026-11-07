// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Meshes, and the shapes the benchmark draws, made by code.

use crate::math::Vec3;
use alloc::vec::Vec;
use core::f32::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex {
    pub pos: Vec3,
    pub normal: Vec3,
    pub uv: [f32; 2],
}

/// Triangles as indices into `vertices`, counter-clockwise from outside. Every shape is
/// `None` for a size it cannot have or when the heap has no room, so a program with a
/// fixed budget says so instead of stopping.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    fn reserved(vertices: usize, indices: usize) -> Option<Self> {
        let mut m = Self::default();
        m.vertices.try_reserve_exact(vertices).ok()?;
        m.indices.try_reserve_exact(indices).ok()?;
        Some(m)
    }

    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }

    /// A grid of `columns` x `rows` quads, each two triangles, over vertices made by
    /// `at(u, v)` for `u` and `v` from 0 to 1. What the round shapes and the terrain share.
    /// The triangles turn counter-clockwise around the direction of `du x dv`; `flip`
    /// turns them the other way, for a surface whose outside is the other side.
    fn grid(columns: u32, rows: u32, flip: bool, at: impl Fn(f32, f32) -> Vertex) -> Option<Self> {
        if columns == 0 || rows == 0 {
            return None;
        }
        let vertices = columns.checked_add(1)?.checked_mul(rows.checked_add(1)?)?;
        let indices = columns.checked_mul(rows)?.checked_mul(6)?;
        let mut m = Mesh::reserved(vertices as usize, indices as usize)?;
        for j in 0..=rows {
            for i in 0..=columns {
                m.vertices
                    .push(at(i as f32 / columns as f32, j as f32 / rows as f32));
            }
        }
        let stride = columns + 1;
        for j in 0..rows {
            for i in 0..columns {
                let a = j * stride + i;
                let (b, c, d) = (a + 1, a + stride, a + stride + 1);
                if flip {
                    m.indices.extend_from_slice(&[a, b, c, b, d, c]);
                } else {
                    m.indices.extend_from_slice(&[a, c, b, b, c, d]);
                }
            }
        }
        Some(m)
    }

    /// A ring, `sides` around its tube and `rings` around its centre.
    pub fn torus(major: f32, minor: f32, rings: u32, sides: u32) -> Option<Self> {
        Self::grid(rings, sides, false, |u, v| {
            let (a, b) = (u * TAU, v * TAU);
            let (sa, ca, sb, cb) = (libm::sinf(a), libm::cosf(a), libm::sinf(b), libm::cosf(b));
            let normal = Vec3::new(ca * cb, sb, sa * cb);
            let centre = Vec3::new(ca * major, 0.0, sa * major);
            Vertex {
                pos: centre + normal * minor,
                normal,
                uv: [u * 4.0, v],
            }
        })
    }

    /// A sphere of `stacks` from pole to pole and `slices` around.
    pub fn sphere(radius: f32, stacks: u32, slices: u32) -> Option<Self> {
        Self::grid(slices, stacks, true, |u, v| {
            let (a, b) = (u * TAU, v * PI);
            let normal = Vec3::new(
                libm::sinf(b) * libm::cosf(a),
                libm::cosf(b),
                libm::sinf(b) * libm::sinf(a),
            );
            Vertex {
                pos: normal * radius,
                normal,
                uv: [u * 2.0, v],
            }
        })
    }

    /// A cube of side 2 around the origin, each face with its own corners and texture.
    pub fn cube() -> Option<Self> {
        let mut m = Mesh::reserved(24, 36)?;
        let faces = [
            (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
            (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0)),
            (Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.0, 0.0, 0.0)),
            (Vec3::new(0.0, -1.0, 0.0), Vec3::new(-1.0, 0.0, 0.0)),
            (Vec3::new(0.0, 0.0, 1.0), Vec3::new(-1.0, 0.0, 0.0)),
            (Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 0.0)),
        ];
        for (normal, side) in faces {
            let up = normal.cross(side);
            let base = m.vertices.len() as u32;
            for (s, t) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                m.vertices.push(Vertex {
                    pos: normal + side * s + up * t,
                    normal,
                    uv: [(s + 1.0) / 2.0, (t + 1.0) / 2.0],
                });
            }
            m.indices
                .extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
        Some(m)
    }

    /// Hills of `cells` x `cells` quads over a square of `size`, heights from a sum of
    /// waves: many triangles, near and far, as a landscape has.
    pub fn terrain(size: f32, cells: u32, height: f32) -> Option<Self> {
        let h = |x: f32, z: f32| {
            height
                * (0.5 * libm::sinf(x * 0.31) * libm::cosf(z * 0.23)
                    + 0.3 * libm::sinf(x * 0.11 + z * 0.17)
                    + 0.2 * libm::cosf(z * 0.43 - x * 0.05))
        };
        let mut m = Self::grid(cells, cells, false, |u, v| {
            let (x, z) = ((u - 0.5) * size, (v - 0.5) * size);
            Vertex {
                pos: Vec3::new(x, h(x, z), z),
                normal: Vec3::default(),
                uv: [u * 8.0, v * 8.0],
            }
        })?;
        // Normals from the slope, as a surface's are.
        let e = size / cells as f32;
        for v in &mut m.vertices {
            let (x, z) = (v.pos.x, v.pos.z);
            v.normal = Vec3::new(
                h(x - e, z) - h(x + e, z),
                2.0 * e,
                h(x, z - e) - h(x, z + e),
            )
            .normalized();
        }
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_grid_sizes_fail_before_allocating() {
        for size in [0, u32::MAX] {
            assert!(Mesh::torus(1.0, 0.3, size, 16).is_none());
            assert!(Mesh::sphere(1.0, 16, size).is_none());
            assert!(Mesh::terrain(40.0, size, 3.0).is_none());
        }
    }

    #[test]
    fn shapes_have_the_triangles_and_unit_normals_they_should() {
        let t = Mesh::torus(1.0, 0.3, 32, 16).unwrap();
        assert_eq!(t.triangles(), 32 * 16 * 2);
        let s = Mesh::sphere(1.0, 20, 40).unwrap();
        assert_eq!(s.triangles(), 20 * 40 * 2);
        assert_eq!(Mesh::cube().unwrap().triangles(), 12);
        let g = Mesh::terrain(40.0, 64, 3.0).unwrap();
        assert_eq!(g.triangles(), 64 * 64 * 2);
        for m in [&t, &s, &g] {
            assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
            for v in &m.vertices {
                assert!((v.normal.length() - 1.0).abs() < 1e-3, "{:?}", v.normal);
            }
        }
    }

    /// Each triangle turns counter-clockwise around the normals its corners carry: the
    /// side lit is the side drawn.
    #[test]
    fn every_shape_winds_the_way_its_normals_face() {
        for (name, m) in [
            ("torus", Mesh::torus(1.0, 0.3, 32, 16).unwrap()),
            ("sphere", Mesh::sphere(1.0, 20, 40).unwrap()),
            ("terrain", Mesh::terrain(40.0, 32, 3.0).unwrap()),
            ("cube", Mesh::cube().unwrap()),
        ] {
            let mut agree = 0;
            for tri in m.indices.chunks(3) {
                let [a, b, c] = [0, 1, 2].map(|k| m.vertices[tri[k] as usize]);
                let n = (b.pos - a.pos).cross(c.pos - a.pos);
                // The sphere's triangles at its poles have no area to speak of, and their
                // turning is rounding noise.
                if n.length() < 1e-6 {
                    continue;
                }
                let carried = a.normal + b.normal + c.normal;
                assert!(
                    n.dot(carried) > 0.0,
                    "{name}: a triangle wound against its normals"
                );
                agree += 1;
            }
            assert!(agree > 0);
        }
    }

    #[test]
    fn the_cube_faces_outward() {
        let c = Mesh::cube().unwrap();
        for tri in c.indices.chunks(3) {
            let [a, b, d] = [0, 1, 2].map(|k| c.vertices[tri[k] as usize].pos);
            let n = (b - a).cross(d - a);
            let centre = (a + b + d) * (1.0 / 3.0);
            assert!(n.dot(centre) > 0.0, "counter-clockwise seen from outside");
        }
    }
}
