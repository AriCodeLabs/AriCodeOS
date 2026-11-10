// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Pista: a kart racing game of our own, to show what the system does with a
//! CPU and a framebuffer, with optional Gen7 compute finishing. Geometry and
//! physics use integers; the finishing kernel has a matching CPU reference.
//! `desktop/pista` owns the window, keyboard and GPU capability lifecycle.

#![no_std]

pub mod effects;
pub mod fixed;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod hud;
pub mod raster;
pub mod render;
pub mod track;
pub mod world;
