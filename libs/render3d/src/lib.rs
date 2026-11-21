// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! 3D drawn by the CPU (ADR 0080): what the graphics benchmark draws today, and the CPU
//! backend that the GPU path will be measured against, scene for scene.
//!
//! A mesh is transformed, clipped against the near plane, and its triangles filled with
//! edge functions and the top-left rule, so two triangles sharing an edge shade each pixel
//! once. Attributes are interpolated with perspective. Depth is 16 bits. Shading is flat,
//! per vertex (Gouraud) or per pixel (Phong), with an optional texture filtered nearest,
//! bilinear or through mipmaps. Nothing here allocates while drawing.

#![no_std]

extern crate alloc;

pub mod effects;
pub mod math;
pub mod mesh;
pub mod raster;
pub mod shadow;
pub mod texture;

pub use math::{Mat4, Vec3};
pub use mesh::{Mesh, Vertex};
pub use raster::{draw, draw_shadowed, Camera, Light, Material, Shading, Stats, Target};
pub use shadow::ShadowMap;
pub use texture::{Filter, Texture};
