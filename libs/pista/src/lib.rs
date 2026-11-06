// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Pista: a kart racing game of our own, to show what the system does with a
//! CPU and a framebuffer. There is no GPU driver: every pixel comes from the
//! CPU. It works in integers, so the same race gives the same pictures on any
//! machine. Everything here is tested on the host; `desktop/pista` is the window
//! and the keyboard around it.

#![no_std]

pub mod fixed;
pub mod raster;
pub mod render;
pub mod track;
pub mod world;
