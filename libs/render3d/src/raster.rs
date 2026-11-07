// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Triangles onto a colour and depth buffer.
//!
//! Positions are snapped to sixteenths of a pixel and the edge functions are integers, so
//! the two triangles of a shared edge compute exactly opposite values there, and a tie
//! goes to exactly one of them: no gap and no pixel shaded twice.

use crate::math::{Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};
use crate::shadow::ShadowMap;
use crate::texture::{Filter, Texture};

/// Sub-pixel steps per pixel, per axis.
const SUB: i64 = 16;

/// Where a frame is drawn: `stride` pixels from one row to the next in both buffers.
pub struct Target<'a> {
    pub color: &'a mut [u32],
    pub depth: &'a mut [u16],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl Target<'_> {
    /// `None` if a buffer does not cover the rows.
    pub fn check(&self) -> Option<()> {
        let need = self.stride * self.height;
        (self.stride >= self.width && self.color.len() >= need && self.depth.len() >= need)
            .then_some(())
    }

    pub fn clear(&mut self, color: u32) {
        for y in 0..self.height {
            let row = y * self.stride;
            self.color[row..row + self.width].fill(color);
            self.depth[row..row + self.width].fill(u16::MAX);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shading {
    /// One colour per triangle, from its own normal.
    Flat,
    /// Light at the corners, blended across (Gouraud).
    Gouraud,
    /// Light at each pixel, from its interpolated normal (Phong), with a highlight.
    Phong,
}

#[derive(Clone, Copy)]
pub struct Material<'a> {
    pub shading: Shading,
    /// Its colour, each channel 0 to 1; a texture's is multiplied by it.
    pub color: [f32; 3],
    pub texture: Option<(&'a Texture, Filter)>,
    /// The highlight's sharpness; 0 for none.
    pub shininess: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Light {
    /// Toward the light, at length 1.
    pub direction: Vec3,
    /// What reaches a surface turned away, 0 to 1.
    pub ambient: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub view_proj: Mat4,
    pub eye: Vec3,
}

/// What a draw did: what the benchmark reports beside its frame time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub triangles: u64,
    /// Turned away, or wholly behind the camera.
    pub culled: u64,
    /// Pixels that passed the depth test and were written.
    pub pixels: u64,
}

impl core::ops::AddAssign for Stats {
    fn add_assign(&mut self, o: Self) {
        self.triangles += o.triangles;
        self.culled += o.culled;
        self.pixels += o.pixels;
    }
}

/// A vertex after the vertex stage.
#[derive(Debug, Clone, Copy, Default)]
struct Var {
    clip: [f32; 4],
    world: Vec3,
    normal: Vec3,
    uv: [f32; 2],
    /// Its lit colour, for Gouraud.
    lit: [f32; 3],
}

impl Var {
    fn lerp(a: &Var, b: &Var, t: f32) -> Var {
        let f = |x: f32, y: f32| x + (y - x) * t;
        let v = |x: Vec3, y: Vec3| x + (y - x) * t;
        Var {
            clip: core::array::from_fn(|i| f(a.clip[i], b.clip[i])),
            world: v(a.world, b.world),
            normal: v(a.normal, b.normal),
            uv: core::array::from_fn(|i| f(a.uv[i], b.uv[i])),
            lit: core::array::from_fn(|i| f(a.lit[i], b.lit[i])),
        }
    }
}

/// What light makes of `color` at a point with normal `n`, seen from `eye`.
fn shade(color: [f32; 3], n: Vec3, at: Vec3, eye: Vec3, light: &Light, shininess: f32) -> [f32; 3] {
    let diffuse = n.dot(light.direction).max(0.0);
    let k = light.ambient + (1.0 - light.ambient) * diffuse;
    let mut out = color.map(|c| c * k);
    if shininess > 0.0 && diffuse > 0.0 {
        let half = (light.direction + (eye - at).normalized()).normalized();
        let s = 0.5 * libm::powf(n.dot(half).max(0.0), shininess);
        out = out.map(|c| c + s);
    }
    out
}

/// Draws `mesh`, placed by `model`, seen by `camera`, onto `target`.
pub fn draw(
    target: &mut Target,
    mesh: &Mesh,
    model: &Mat4,
    camera: &Camera,
    light: &Light,
    material: &Material,
) -> Stats {
    draw_with(target, mesh, model, camera, light, material, None)
}

/// As [`draw`], with what `shadow` hides from `light` lit only by its ambient part.
pub fn draw_shadowed(
    target: &mut Target,
    mesh: &Mesh,
    model: &Mat4,
    camera: &Camera,
    light: &Light,
    material: &Material,
    shadow: &ShadowMap,
) -> Stats {
    draw_with(target, mesh, model, camera, light, material, Some(shadow))
}

fn draw_with(
    target: &mut Target,
    mesh: &Mesh,
    model: &Mat4,
    camera: &Camera,
    light: &Light,
    material: &Material,
    shadow: Option<&ShadowMap>,
) -> Stats {
    let mut stats = Stats::default();
    if target.check().is_none() {
        return stats;
    }
    let mvp = camera.view_proj * *model;
    let vertex = |v: &Vertex| {
        let world = model.point(v.pos);
        let normal = model.direction(v.normal).normalized();
        let lit = match material.shading {
            Shading::Gouraud => shade(
                material.color,
                normal,
                world,
                camera.eye,
                light,
                material.shininess,
            ),
            _ => material.color,
        };
        Var {
            clip: mvp.transform(v.pos, 1.0),
            world,
            normal,
            uv: v.uv,
            lit,
        }
    };
    for tri in mesh.indices.as_chunks::<3>().0 {
        stats.triangles += 1;
        let mut corners = [Var::default(); 3];
        for (c, &i) in corners.iter_mut().zip(tri) {
            match mesh.vertices.get(i as usize) {
                Some(v) => *c = vertex(v),
                None => return stats,
            }
        }
        let flat = (material.shading == Shading::Flat).then(|| {
            let [a, b, c] = corners.map(|v| v.world);
            let n = (b - a).cross(c - a).normalized();
            let centre = (a + b + c) * (1.0 / 3.0);
            shade(
                material.color,
                n,
                centre,
                camera.eye,
                light,
                material.shininess,
            )
        });
        // Clipped against the near plane (depth 0): at most four corners, as a fan.
        let mut poly = [Var::default(); 4];
        let mut n = 0;
        for i in 0..3 {
            let (a, b) = (&corners[i], &corners[(i + 1) % 3]);
            let (da, db) = (a.clip[2], b.clip[2]);
            if da >= 0.0 {
                poly[n] = *a;
                n += 1;
            }
            if (da >= 0.0) != (db >= 0.0) && n < 4 {
                poly[n] = Var::lerp(a, b, da / (da - db));
                n += 1;
            }
        }
        if n < 3 {
            stats.culled += 1;
            continue;
        }
        let mut drew = false;
        for k in 1..n - 1 {
            if let Some(pixels) = fill(
                target,
                [&poly[0], &poly[k], &poly[k + 1]],
                material,
                light,
                camera,
                flat,
                shadow,
            ) {
                stats.pixels += pixels;
                drew = true;
            }
        }
        if !drew {
            stats.culled += 1;
        }
    }
    stats
}

/// A corner on the screen: position in sixteenths, depth, and 1/w.
#[derive(Clone, Copy)]
pub(crate) struct Screen {
    pub(crate) x: i64,
    pub(crate) y: i64,
    pub(crate) z: f32,
    inv_w: f32,
}

pub(crate) fn to_screen(clip: [f32; 4], width: usize, height: usize) -> Option<Screen> {
    let w = clip[3];
    if w <= 1e-6 {
        return None;
    }
    let inv_w = 1.0 / w;
    let sx = (clip[0] * inv_w * 0.5 + 0.5) * width as f32;
    let sy = (0.5 - clip[1] * inv_w * 0.5) * height as f32;
    // Far beyond the screen: the edge functions would overflow nothing, but such a
    // triangle is not worth the bounding box.
    let limit = 1.0e6;
    if !(sx.abs() < limit && sy.abs() < limit) {
        return None;
    }
    Some(Screen {
        x: libm::roundf(sx * SUB as f32) as i64,
        y: libm::roundf(sy * SUB as f32) as i64,
        z: clip[2] * inv_w,
        inv_w,
    })
}

/// Where an edge from `a` to `b` stands at `p`: positive on one side, negative on the
/// other, zero on it, exactly the opposite for `b` to `a`.
pub(crate) fn edge(a: (i64, i64), b: (i64, i64), p: (i64, i64)) -> i64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Whether a pixel exactly on the edge from `a` to `b` belongs to this triangle. The
/// same edge walked the other way answers the opposite, so of two triangles sharing it,
/// one takes it.
fn owns(a: (i64, i64), b: (i64, i64)) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    dy > 0 || (dy == 0 && dx > 0)
}

/// Fills one triangle; how many pixels it wrote, or `None` if it faces away or is off
/// the screen.
fn fill(
    target: &mut Target,
    v: [&Var; 3],
    material: &Material,
    light: &Light,
    camera: &Camera,
    flat: Option<[f32; 3]>,
    shadow: Option<&ShadowMap>,
) -> Option<u64> {
    let s: [Screen; 3] = [
        to_screen(v[0].clip, target.width, target.height)?,
        to_screen(v[1].clip, target.width, target.height)?,
        to_screen(v[2].clip, target.width, target.height)?,
    ];
    let p = s.map(|c| (c.x, c.y));
    let area = edge(p[0], p[1], p[2]);
    // Counter-clockwise as seen is the side facing the camera; on a screen whose y goes
    // down that makes this area positive.
    if area <= 0 {
        return None;
    }
    // The mip level for the whole triangle, from its texels per pixel.
    let level = match material.texture {
        Some((tex, Filter::Mipmap)) => {
            let size = tex.size() as f32;
            let [a, b, c] = [v[0].uv, v[1].uv, v[2].uv];
            let texels =
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * size * size;
            let pixels = area as f32 / (SUB * SUB) as f32;
            let lod = 0.5 * libm::log2f((texels / pixels.max(1e-6)).max(1.0));
            libm::floorf(lod + 0.5) as usize
        }
        _ => 0,
    };
    let inv_area = 1.0 / area as f32;
    let mut written = 0;
    let (width, height, stride) = (target.width, target.height, target.stride);
    cover(p, width, height, stride, |at, [e0, e1, e2]| {
        let l = [
            e0 as f32 * inv_area,
            e1 as f32 * inv_area,
            e2 as f32 * inv_area,
        ];
        let z = l[0] * s[0].z + l[1] * s[1].z + l[2] * s[2].z;
        let depth = (z.clamp(0.0, 1.0) * 65534.0) as u16;
        if depth >= target.depth[at] {
            return;
        }
        // With perspective: each corner's share weighted by its 1/w.
        let wl = [l[0] * s[0].inv_w, l[1] * s[1].inv_w, l[2] * s[2].inv_w];
        let norm = 1.0 / (wl[0] + wl[1] + wl[2]);
        let mix3 = |f: fn(&Var) -> [f32; 3]| mix(v, wl, norm, f);
        let mut color = match (material.shading, flat) {
            (Shading::Flat, Some(c)) => c,
            (Shading::Phong, _) => {
                let n = mix3(|v| [v.normal.x, v.normal.y, v.normal.z]);
                let w = mix3(|v| [v.world.x, v.world.y, v.world.z]);
                shade(
                    material.color,
                    Vec3::new(n[0], n[1], n[2]).normalized(),
                    Vec3::new(w[0], w[1], w[2]),
                    camera.eye,
                    light,
                    material.shininess,
                )
            }
            _ => mix3(|v| v.lit),
        };
        if let Some(map) = shadow {
            let w = mix3(|v| [v.world.x, v.world.y, v.world.z]);
            if map.hides(Vec3::new(w[0], w[1], w[2])) {
                // Only what reaches a surface turned away: no direct light, no highlight.
                color = material.color.map(|c| c * light.ambient);
            }
        }
        if let Some((tex, filter)) = material.texture {
            let uv = mix3(|v| [v.uv[0], v.uv[1], 0.0]);
            let t = tex.sample(uv[0], uv[1], filter, level);
            color = core::array::from_fn(|i| color[i] * t[i]);
        }
        target.depth[at] = depth;
        target.color[at] = pack(color);
        written += 1;
    })?;
    Some(written)
}

/// Calls `pixel(at, edges)` for each pixel centre inside the triangle `p` (in
/// sixteenths, counter-clockwise on screen) within `width` x `height`, `at` its index
/// `stride` apart. The one rule both passes share: of two triangles with a common edge,
/// exactly one takes the pixels on it. `None` when no pixel of its box is on screen.
#[inline(always)]
pub(crate) fn cover(
    p: [(i64, i64); 3],
    width: usize,
    height: usize,
    stride: usize,
    mut pixel: impl FnMut(usize, [i64; 3]),
) -> Option<()> {
    let x0 = (p.iter().map(|q| q.0).min()? / SUB).max(0);
    let x1 = ((p.iter().map(|q| q.0).max()? + SUB - 1) / SUB).min(width as i64);
    let y0 = (p.iter().map(|q| q.1).min()? / SUB).max(0);
    let y1 = ((p.iter().map(|q| q.1).max()? + SUB - 1) / SUB).min(height as i64);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let owned = [owns(p[1], p[2]), owns(p[2], p[0]), owns(p[0], p[1])];
    let inside = |e: i64, k: usize| e > 0 || (e == 0 && owned[k]);
    // Each edge function is linear in the pixel: start it at the box's first pixel
    // centre and add its step per pixel and per row. Integers, so exactly what
    // `edge` gives there, without its multiplications.
    let sides = [(p[1], p[2]), (p[2], p[0]), (p[0], p[1])];
    let step_x = sides.map(|(a, b)| -(b.1 - a.1) * SUB);
    let step_y = sides.map(|(a, b)| (b.0 - a.0) * SUB);
    let first = (x0 * SUB + SUB / 2, y0 * SUB + SUB / 2);
    let mut row_e = sides.map(|(a, b)| edge(a, b, first));
    for y in y0..y1 {
        let row = y as usize * stride;
        let mut next = row_e;
        row_e = core::array::from_fn(|k| row_e[k] + step_y[k]);
        for x in x0..x1 {
            let e = next;
            next = core::array::from_fn(|k| next[k] + step_x[k]);
            if inside(e[0], 0) && inside(e[1], 1) && inside(e[2], 2) {
                pixel(row + x as usize, e);
            }
        }
    }
    Some(())
}

/// A corner value mixed by the pixel's perspective weights. Generic and inlined: a
/// call through `dyn` per pixel and attribute was most of a pixel's cost.
#[inline(always)]
fn mix(v: [&Var; 3], wl: [f32; 3], norm: f32, f: impl Fn(&Var) -> [f32; 3]) -> [f32; 3] {
    let (a, b, c) = (f(v[0]), f(v[1]), f(v[2]));
    core::array::from_fn(|i| (a[i] * wl[0] + b[i] * wl[1] + c[i] * wl[2]) * norm)
}

fn pack(c: [f32; 3]) -> u32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    b(c[0]) << 16 | b(c[1]) << 8 | b(c[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Mesh;
    use alloc::vec;

    const W: usize = 64;
    const H: usize = 48;

    fn camera() -> Camera {
        let eye = Vec3::new(0.0, 0.0, -4.0);
        let view = Mat4::look_at(eye, Vec3::default(), Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(1.0, W as f32 / H as f32, 0.5, 50.0);
        Camera {
            view_proj: proj * view,
            eye,
        }
    }

    fn light() -> Light {
        Light {
            direction: Vec3::new(0.0, 0.0, -1.0),
            ambient: 0.2,
        }
    }

    fn plain(shading: Shading) -> Material<'static> {
        Material {
            shading,
            color: [1.0, 0.5, 0.25],
            texture: None,
            shininess: 0.0,
        }
    }

    struct Buffers {
        color: Vec<u32>,
        depth: Vec<u16>,
    }

    impl Buffers {
        fn new() -> Self {
            Self {
                color: vec![0; W * H],
                depth: vec![u16::MAX; W * H],
            }
        }
        fn target(&mut self) -> Target<'_> {
            Target {
                color: &mut self.color,
                depth: &mut self.depth,
                width: W,
                height: H,
                stride: W,
            }
        }
    }

    use alloc::vec::Vec;

    /// A square facing the camera, split along a diagonal, so its two triangles share an
    /// edge that crosses pixel centres.
    fn square(half: f32) -> Mesh {
        let corner = |x: f32, y: f32| Vertex {
            pos: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, -1.0),
            uv: [(x / half + 1.0) / 2.0, (y / half + 1.0) / 2.0],
        };
        Mesh {
            vertices: vec![
                corner(-half, -half),
                corner(half, -half),
                corner(-half, half),
                corner(half, half),
            ],
            // Counter-clockwise as the camera at -z sees them: these face it.
            indices: vec![0, 2, 1, 1, 2, 3],
        }
    }

    #[test]
    fn a_shared_edge_shades_each_pixel_once_and_leaves_no_gap() {
        // An orthographic-like case is not needed: count against a lone fill of the
        // whole square drawn as one shape's outline, by painting each triangle in its
        // own colour and checking each covered pixel is one or the other, with the total
        // equal to the pixels written.
        let mut b = Buffers::new();
        let stats = draw(
            &mut b.target(),
            &square(1.0),
            &Mat4::IDENTITY,
            &camera(),
            &light(),
            &plain(Shading::Flat),
        );
        assert_eq!(stats.triangles, 2);
        let covered = b.color.iter().filter(|&&c| c != 0).count() as u64;
        assert_eq!(
            stats.pixels, covered,
            "a pixel written twice would count twice"
        );
        // No hole: every row between the first and last covered is a solid run.
        for y in 0..H {
            let row = &b.color[y * W..(y + 1) * W];
            let first = row.iter().position(|&c| c != 0);
            let last = row.iter().rposition(|&c| c != 0);
            if let (Some(f), Some(l)) = (first, last) {
                assert!(row[f..=l].iter().all(|&c| c != 0), "a gap in row {y}");
            }
        }
        assert!(covered > 100);
    }

    #[test]
    fn the_near_square_hides_the_far_one_whatever_the_order() {
        for near_first in [true, false] {
            let mut b = Buffers::new();
            let near = Mat4::translate(Vec3::new(0.0, 0.0, -1.0));
            let far = Mat4::translate(Vec3::new(0.0, 0.0, 1.0)) * Mat4::scale(3.0);
            let red = Material {
                color: [1.0, 0.0, 0.0],
                ..plain(Shading::Flat)
            };
            let blue = Material {
                color: [0.0, 0.0, 1.0],
                ..plain(Shading::Flat)
            };
            let order = if near_first {
                [(near, red), (far, blue)]
            } else {
                [(far, blue), (near, red)]
            };
            for (m, mat) in order {
                draw(&mut b.target(), &square(1.0), &m, &camera(), &light(), &mat);
            }
            let centre = b.color[H / 2 * W + W / 2];
            assert!(
                centre >> 16 > 0 && centre & 0xff == 0,
                "red in front: {centre:06x}"
            );
        }
    }

    #[test]
    fn a_shape_turned_away_or_behind_the_camera_draws_nothing() {
        let mut b = Buffers::new();
        let back = Mat4::rotate_y(core::f32::consts::PI);
        let s = draw(
            &mut b.target(),
            &square(1.0),
            &back,
            &camera(),
            &light(),
            &plain(Shading::Flat),
        );
        assert_eq!((s.pixels, s.culled), (0, 2));
        let behind = Mat4::translate(Vec3::new(0.0, 0.0, -10.0));
        let s = draw(
            &mut b.target(),
            &square(1.0),
            &behind,
            &camera(),
            &light(),
            &plain(Shading::Flat),
        );
        assert_eq!(s.pixels, 0);
        assert!(b.color.iter().all(|&c| c == 0));
    }

    #[test]
    fn a_triangle_through_the_camera_is_clipped_not_lost() {
        let mut b = Buffers::new();
        // A large floor under the camera, reaching behind it.
        let floor = Mat4::translate(Vec3::new(0.0, -1.0, 0.0))
            * Mat4::rotate_x(core::f32::consts::FRAC_PI_2);
        let s = draw(
            &mut b.target(),
            &square(20.0),
            &floor,
            &camera(),
            &light(),
            &plain(Shading::Gouraud),
        );
        assert!(s.pixels > 0);
        assert!(
            b.color[(H - 1) * W + W / 2] != 0,
            "the floor reaches the bottom of the screen"
        );
    }

    #[test]
    fn every_shading_and_filter_draws_a_lit_torus_inside_the_frame() {
        let tex = Texture::checker(64, 8, 0xffffff, 0x4060a0);
        let torus = Mesh::torus(1.2, 0.5, 24, 12).unwrap();
        let model = Mat4::rotate_x(0.6);
        let mut counts = Vec::new();
        for shading in [Shading::Flat, Shading::Gouraud, Shading::Phong] {
            for texture in [
                None,
                Some((&tex, Filter::Nearest)),
                Some((&tex, Filter::Linear)),
                Some((&tex, Filter::Mipmap)),
            ] {
                let mut b = Buffers::new();
                let m = Material {
                    shading,
                    color: [1.0, 1.0, 1.0],
                    texture,
                    shininess: 16.0,
                };
                let s = draw(&mut b.target(), &torus, &model, &camera(), &light(), &m);
                assert!(s.pixels > 300, "{shading:?} {:?}", texture.map(|t| t.1));
                counts.push(s.pixels);
            }
        }
        // The same shape covers the same pixels whatever its colour.
        assert!(counts.iter().all(|&c| c == counts[0]));
    }

    #[test]
    fn far_off_and_degenerate_triangles_are_harmless() {
        let mut b = Buffers::new();
        let mut m = square(1.0);
        m.vertices[1].pos = m.vertices[0].pos; // a line
        m.vertices.push(Vertex {
            pos: Vec3::new(1e9, -1e9, 3.0),
            ..Default::default()
        });
        m.indices.extend_from_slice(&[4, 2, 3]);
        m.indices.extend_from_slice(&[0, 1, 99]); // an index past the end ends the draw
        draw(
            &mut b.target(),
            &m,
            &Mat4::IDENTITY,
            &camera(),
            &light(),
            &plain(Shading::Phong),
        );
        // A target whose buffers are too small is refused, not overrun.
        let mut short = vec![0u32; 10];
        let mut depth = vec![0u16; 10];
        let mut t = Target {
            color: &mut short,
            depth: &mut depth,
            width: W,
            height: H,
            stride: W,
        };
        assert_eq!(
            draw(
                &mut t,
                &square(1.0),
                &Mat4::IDENTITY,
                &camera(),
                &light(),
                &plain(Shading::Flat)
            ),
            Stats::default()
        );
    }
}

/// To look at what is drawn: `R3D_DUMP=file.ppm cargo test -p aricode-render3d dump -- --ignored`.
#[cfg(test)]
mod dump {
    extern crate std;
    use super::*;
    use crate::mesh::Mesh;
    use alloc::vec;

    #[test]
    #[ignore]
    fn dump() {
        let path = std::env::var("R3D_DUMP").expect("R3D_DUMP");
        let (w, h) = (800usize, 600usize);
        let mut color = vec![0u32; w * h];
        let mut depth = vec![u16::MAX; w * h];
        let mut t = Target {
            color: &mut color,
            depth: &mut depth,
            width: w,
            height: h,
            stride: w,
        };
        t.clear(0x182030);
        let eye = Vec3::new(0.0, 4.0, -9.0);
        let view = Mat4::look_at(eye, Vec3::new(0.0, 0.5, 0.0), Vec3::new(0.0, 1.0, 0.0));
        let cam = Camera {
            view_proj: Mat4::perspective(1.0, w as f32 / h as f32, 0.3, 200.0) * view,
            eye,
        };
        let light = Light {
            direction: Vec3::new(-0.4, 0.8, -0.5).normalized(),
            ambient: 0.2,
        };
        let tex = crate::texture::Texture::checker(256, 8, 0xf0f0f0, 0x3070c0);
        let ground = Mesh::terrain(60.0, 96, 2.0).unwrap();
        draw(
            &mut t,
            &ground,
            &Mat4::translate(Vec3::new(0.0, -2.5, 10.0)),
            &cam,
            &light,
            &Material {
                shading: Shading::Gouraud,
                color: [0.45, 0.7, 0.35],
                texture: None,
                shininess: 0.0,
            },
        );
        let torus = Mesh::torus(1.3, 0.5, 48, 24).unwrap();
        draw(
            &mut t,
            &torus,
            &(Mat4::translate(Vec3::new(-2.2, 1.0, 0.0)) * Mat4::rotate_x(0.9)),
            &cam,
            &light,
            &Material {
                shading: Shading::Phong,
                color: [0.9, 0.35, 0.2],
                texture: None,
                shininess: 32.0,
            },
        );
        let cube = Mesh::cube().unwrap();
        draw(
            &mut t,
            &cube,
            &(Mat4::translate(Vec3::new(2.2, 1.0, 0.0))
                * Mat4::rotate_y(0.6)
                * Mat4::rotate_x(0.4)),
            &cam,
            &light,
            &Material {
                shading: Shading::Flat,
                color: [1.0, 1.0, 1.0],
                texture: Some((&tex, crate::texture::Filter::Mipmap)),
                shininess: 0.0,
            },
        );
        let sphere = Mesh::sphere(0.9, 24, 48).unwrap();
        draw(
            &mut t,
            &sphere,
            &Mat4::translate(Vec3::new(0.0, 2.6, 1.5)),
            &cam,
            &light,
            &Material {
                shading: Shading::Gouraud,
                color: [1.0, 1.0, 1.0],
                texture: Some((&tex, crate::texture::Filter::Linear)),
                shininess: 0.0,
            },
        );
        let mut out = std::format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in color.iter() {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(path, out).unwrap();
    }
}
