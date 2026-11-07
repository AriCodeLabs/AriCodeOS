// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Vectors and matrices in `f32`, with `libm` for what `core` lacks in `no_std`.

use core::ops::{Add, Mul, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f32 {
        libm::sqrtf(self.dot(self))
    }

    /// The same direction at length 1, or zero for zero.
    pub fn normalized(self) -> Self {
        let l = self.length();
        if l > 0.0 {
            self * (1.0 / l)
        } else {
            self
        }
    }
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, k: f32) -> Self {
        Self::new(self.x * k, self.y * k, self.z * k)
    }
}

/// A 4x4 matrix, row-major, acting on column vectors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    pub fn translate(t: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][3] = t.x;
        m.0[1][3] = t.y;
        m.0[2][3] = t.z;
        m
    }

    pub fn scale(k: f32) -> Self {
        let mut m = Self::IDENTITY;
        for i in 0..3 {
            m.0[i][i] = k;
        }
        m
    }

    pub fn rotate_y(a: f32) -> Self {
        let (s, c) = (libm::sinf(a), libm::cosf(a));
        Self([
            [c, 0.0, s, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [-s, 0.0, c, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn rotate_x(a: f32) -> Self {
        let (s, c) = (libm::sinf(a), libm::cosf(a));
        Self([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, c, -s, 0.0],
            [0.0, s, c, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// A perspective projection: depth 0 at `near`, 1 at `far`, and `w` the distance.
    pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / libm::tanf(fov_y / 2.0);
        let range = far / (far - near);
        Self([
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, range, -near * range],
            [0.0, 0.0, 1.0, 0.0],
        ])
    }

    /// A camera at `eye` looking at `at`, with `up` roughly upward. The view looks
    /// along +z.
    pub fn look_at(eye: Vec3, at: Vec3, up: Vec3) -> Self {
        let f = (at - eye).normalized();
        let r = up.cross(f).normalized();
        let u = f.cross(r);
        Self([
            [r.x, r.y, r.z, -r.dot(eye)],
            [u.x, u.y, u.z, -u.dot(eye)],
            [f.x, f.y, f.z, -f.dot(eye)],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn transform(&self, v: Vec3, w: f32) -> [f32; 4] {
        let m = &self.0;
        let mut out = [0.0; 4];
        for (i, o) in out.iter_mut().enumerate() {
            *o = m[i][0] * v.x + m[i][1] * v.y + m[i][2] * v.z + m[i][3] * w;
        }
        out
    }

    pub fn point(&self, v: Vec3) -> Vec3 {
        let [x, y, z, _] = self.transform(v, 1.0);
        Vec3::new(x, y, z)
    }

    /// A direction, without the translation; for normals under rotation and uniform
    /// scale, which is all the meshes here get.
    pub fn direction(&self, v: Vec3) -> Vec3 {
        let [x, y, z, _] = self.transform(v, 0.0);
        Vec3::new(x, y, z)
    }
}

impl Mul for Mat4 {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        let mut out = [[0.0; 4]; 4];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.0[i][k] * o.0[k][j]).sum();
            }
        }
        Self(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_camera_looks_along_its_axis_and_projects_into_depth() {
        let eye = Vec3::new(0.0, 0.0, -5.0);
        let view = Mat4::look_at(eye, Vec3::default(), Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(view.point(Vec3::default()), Vec3::new(0.0, 0.0, 5.0));
        let proj = Mat4::perspective(1.0, 1.0, 1.0, 10.0);
        let near = proj.transform(Vec3::new(0.0, 0.0, 1.0), 1.0);
        let far = proj.transform(Vec3::new(0.0, 0.0, 10.0), 1.0);
        assert!((near[2] / near[3]).abs() < 1e-6);
        assert!((far[2] / far[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rotations_keep_lengths() {
        let v = Vec3::new(1.0, 2.0, 3.0);
        let r = (Mat4::rotate_y(0.7) * Mat4::rotate_x(-1.3)).direction(v);
        assert!((r.length() - v.length()).abs() < 1e-5);
    }
}
