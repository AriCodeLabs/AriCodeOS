// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! bench3d: the graphics benchmark of ADR 0080. Sixteen scenes, of the kinds glmark2
//! has (geometry, textures, shading, 2D effects, a desktop, heavy scenes), drawn in its
//! window while it measures them, with the frame rate, the frame time and the
//! processor's temperature on screen.
//!
//! The method is the one used for the Linux reference on the same laptop (ADR 0080,
//! section 4b): each scene runs 5 s at 800x600, starts below 70 C, and stops at 90 C.
//! Each scene's figures go to the log in the ADR's format, for a machine to compare.
//!
//! Today the scenes are drawn by the CPU (`aricode_render3d`); the GPU path will draw the
//! same ones. `arg` N > 0 draws N frames of each, without waiting to cool, and ends: the
//! `gui.bench3d` scenario, where QEMU's figures say nothing about speed.
//!
//! Handles: its compositor session, `HARDWARE` and `SYSTEM`.
#![no_std]
#![no_main]

extern crate alloc;

mod materials;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_cmdline::{keys, CmdLine};
use aricode_gfx::{rgb, Canvas, Rect};
use aricode_render3d::effects::{self, convolve};
use aricode_render3d::{
    draw, draw_shadowed, Camera, Filter, Light, Mat4, Material, Mesh, Shading, ShadowMap, Stats,
    Target, Texture, Vec3,
};
use aricode_rt::sys::window::{wants, Event, KeyState, Window};
use aricode_rt::sys::{self, abi, kind, permissions, Handle};
use aricode_text::{Face, Typesetter, INTER_REGULAR, INTER_SEMIBOLD};

const W: usize = 800;
const H: usize = 600;
/// What each scene lasts, in microseconds.
const SCENE_US: u64 = 5_000_000;
/// A scene starts below this, and stops at that: the method of ADR 0080, 4b.
const COOL_C: u32 = 70;
const STOP_C: u32 = 90;
/// More heap, for the depth buffer, the meshes, the texture, the picture the 2D effects
/// filter and the frame being drawn: with the window, within the manifest's 8 MiB.
const MORE_HEAP: usize = 6144 * 1024;
/// The `memory` of bench3d.manifest, in KiB: what its DMA may take, the window included.
const QUOTA_KIB: usize = 8 * 1024;
/// The window's buffer, in whole pages, as the kernel charges it.
const WINDOW_KIB: usize = (W * H * 4).div_ceil(4096) * 4;
/// What the quota leaves after the window and the extra heap.
const SPARE_KIB: usize = QUOTA_KIB - WINDOW_KIB - MORE_HEAP / 1024;
// The budget is checked when bench3d is built, not when it runs out on a machine.
const _: () = assert!(WINDOW_KIB + MORE_HEAP / 1024 <= QUOTA_KIB);
const KEY_ESC: u16 = 1;

#[derive(Clone, Copy)]
enum Kind {
    /// A torus, one colour per triangle.
    Geometry,
    /// A finely divided sphere, lit at its corners.
    DenseGeometry,
    /// A turning cube, its texture filtered so.
    Texture(Filter),
    /// A torus, lit so.
    Shade(Shading),
    /// A rows of cubes going into the distance, with mipmaps.
    Corridor,
    /// A picture filtered by a square kernel of this side.
    Effect(usize),
    /// Windows with their shadows, moving, as the compositor draws them.
    Desktop,
    /// Hills flown over.
    Terrain,
    /// Many small lit spheres.
    Objects,
    /// Objects on the ground, their shadows cast by a light going round them at this
    /// many radians a second: the light's depth drawn again each frame, and each pixel
    /// asking it.
    Shadow(f32),
    /// A Julia set, its constant moving: a loop and a branch for every pixel.
    Fractal,
    /// Hills, objects and textures together.
    Everything,
}

const SCENES: [(&str, Kind); 16] = [
    ("geometry", Kind::Geometry),
    ("geometry-dense", Kind::DenseGeometry),
    ("texture-nearest", Kind::Texture(Filter::Nearest)),
    ("texture-linear", Kind::Texture(Filter::Linear)),
    ("texture-mipmap", Kind::Corridor),
    ("shading-gouraud", Kind::Shade(Shading::Gouraud)),
    ("shading-phong", Kind::Shade(Shading::Phong)),
    ("effect2d-3x3", Kind::Effect(3)),
    ("effect2d-5x5", Kind::Effect(5)),
    ("desktop", Kind::Desktop),
    ("terrain", Kind::Terrain),
    ("objects", Kind::Objects),
    ("shadow-slow", Kind::Shadow(0.6)),
    ("shadow-fast", Kind::Shadow(6.0)),
    ("fractal", Kind::Fractal),
    ("everything", Kind::Everything),
];
/// The side of the shadow scenes' map, held in the effects' picture meanwhile.
const SHADOW_MAP: usize = 512;
const _: () = assert!(SHADOW_MAP * SHADOW_MAP <= W * H);

struct Assets {
    torus: Mesh,
    sphere: Mesh,
    small_sphere: Mesh,
    cube: Mesh,
    terrain: Mesh,
    /// Flat ground for the shadows to fall on.
    ground: Mesh,
    textures: [Texture; 4],
    texture_us: u64,
    depth: Vec<u16>,
    /// A frame to filter, for the 2D effects, made when such a scene starts; the
    /// shadow scenes keep their map in it.
    picture: Vec<u32>,
}

fn say(text: &str) {
    sys::print(b"bench3d: ");
    sys::print(text.as_bytes());
    sys::print(b"\n");
}

fn now_us(system: Handle) -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME_US, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

/// The processor's temperature and where it slows itself, if this machine says.
fn temperature(system: Handle) -> Option<(u32, u32)> {
    let mut b = [0u8; 8];
    (sys::system_info(system, abi::sysinfo::TEMPERATURE, &mut b) == Ok(8)).then(|| {
        (
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        )
    })
}

/// The line that names a run, as ADR 0080's format asks: build and processors.
fn header(system: Handle) -> String {
    let mut args = [0u8; sys::CMDLINE_MAX];
    let line = sys::cmdline(system, &mut args).ok().map(CmdLine::new);
    let build = line.as_ref().map_or(String::from("unknown"), |l| {
        match (l.get(keys::BOOT_CHANNEL), l.get(keys::BOOT_VERSION)) {
            (Some("dev"), Some(v)) => {
                format!("dev-{v}-{}", l.get(keys::BOOT_COMMIT).unwrap_or("?"))
            }
            (_, Some(v)) => String::from(v),
            _ => String::from("esp"),
        }
    });
    let mut cpus = [0u8; 8];
    let n = match sys::system_info(system, abi::sysinfo::CPUS, &mut cpus) {
        Ok(8) => u64::from_le_bytes(cpus),
        _ => 0,
    };
    // `machine` waits for the firmware's model in `diag` (SMBIOS): until then, unknown.
    format!("# machine=unknown build={build} clock=tsc cpus={n} scene=bench3d size={W}x{H} renderer=cpu textures=v2")
}

fn camera(eye: Vec3, at: Vec3) -> Camera {
    let view = Mat4::look_at(eye, at, Vec3::new(0.0, 1.0, 0.0));
    Camera {
        view_proj: Mat4::perspective(1.0, W as f32 / H as f32, 0.3, 300.0) * view,
        eye,
    }
}

const LIGHT: Light = Light {
    direction: Vec3::new(-0.39, 0.77, -0.49),
    ambient: 0.2,
};

fn material(shading: Shading, color: [f32; 3]) -> Material<'static> {
    Material {
        shading,
        color,
        texture: None,
        shininess: 0.0,
    }
}

/// One frame of a scene at `t` seconds into it, onto `pixels`: the frame being drawn,
/// never the window's buffer, which the compositor may read at any moment.
fn frame(kind: Kind, t: f32, a: &mut Assets, pixels: &mut [u32]) -> Stats {
    const SKY: u32 = 0x182030;
    let mut stats = Stats::default();
    match kind {
        Kind::Effect(size) => {
            let mut kernel = [0.0; 25];
            for (i, value) in kernel[..size * size].iter_mut().enumerate() {
                let (x, y) = ((i % size) as f32, (i / size) as f32);
                let c = (size / 2) as f32;
                let d = (x - c) * (x - c) + (y - c) * (y - c);
                *value = libm::expf(-d / (size as f32));
            }
            convolve(&a.picture, pixels, (W, H, W), &kernel[..size * size], size);
            return stats;
        }
        Kind::Fractal => {
            // Round the spirals of the set near -0.77 + 0.19i, zooming in and out.
            let c = (
                -0.77 + 0.03 * libm::cosf(t * 0.6),
                0.19 + 0.03 * libm::sinf(t * 0.8),
            );
            let view = ((0.0, 0.0), 1.8 + 0.5 * libm::sinf(t * 0.5));
            let colours = effects::palette(t * 0.1);
            effects::julia(pixels, (W, H, W), c, view, 128, &colours);
            stats.pixels = (W * H) as u64;
            return stats;
        }
        Kind::Desktop => {
            let Some(mut c) = Canvas::new(pixels, W as u32, H as u32, W as u32) else {
                return stats;
            };
            c.fill(rgb(0x8E, 0xA9, 0xC4));
            for i in 0..4 {
                let k = i as f32;
                let x = 60.0 + k * 140.0 + 40.0 * libm::sinf(t * 1.3 + k);
                let y = 60.0 + k * 90.0 + 30.0 * libm::cosf(t * 1.1 + k);
                let frame = Rect::new(x as i32, y as i32, 320, 220);
                c.window_shadow(&frame, 77);
                c.round_rect(&frame, rgb(0xEC, 0xEC, 0xEC));
                let body = Rect::new(frame.x + 12, frame.y + 40, 296, 168);
                c.rounded(&body, 8, rgb(0xFF, 0xFF, 0xFF));
            }
            return stats;
        }
        _ => {}
    }
    let mut target = Target {
        color: pixels,
        depth: &mut a.depth,
        width: W,
        height: H,
        stride: W,
    };
    target.clear(SKY);
    let spin = Mat4::rotate_y(t * 0.9) * Mat4::rotate_x(t * 0.6);
    let near = camera(Vec3::new(0.0, 0.8, -4.2), Vec3::default());
    match kind {
        Kind::Geometry => {
            stats += draw(
                &mut target,
                &a.torus,
                &spin,
                &near,
                &LIGHT,
                &material(Shading::Flat, [0.9, 0.4, 0.2]),
            );
        }
        Kind::DenseGeometry => {
            stats += draw(
                &mut target,
                &a.sphere,
                &spin,
                &near,
                &LIGHT,
                &material(Shading::Gouraud, [0.3, 0.6, 0.9]),
            );
        }
        Kind::Texture(filter) => {
            let m = Material {
                texture: Some((&a.textures[0], filter)),
                ..material(Shading::Flat, [1.0, 1.0, 1.0])
            };
            stats += draw(
                &mut target,
                &a.cube,
                &(Mat4::scale(1.2) * spin),
                &near,
                &LIGHT,
                &m,
            );
        }
        Kind::Shade(shading) => {
            let m = Material {
                shininess: 32.0,
                ..material(shading, [0.9, 0.35, 0.2])
            };
            stats += draw(&mut target, &a.torus, &spin, &near, &LIGHT, &m);
        }
        Kind::Corridor => {
            let cam = camera(
                Vec3::new(0.0, 1.5, -6.0 + t * 2.0),
                Vec3::new(0.0, 0.5, 30.0 + t * 2.0),
            );
            for i in 0..12 {
                let m = Material {
                    texture: Some((&a.textures[i % 2], Filter::Mipmap)),
                    ..material(Shading::Flat, [1.0, 1.0, 1.0])
                };
                for side in [-2.5f32, 2.5] {
                    let at = Vec3::new(side, 0.5, i as f32 * 4.0);
                    stats += draw(
                        &mut target,
                        &a.cube,
                        &(Mat4::translate(at) * Mat4::rotate_y(t + i as f32)),
                        &cam,
                        &LIGHT,
                        &m,
                    );
                }
            }
        }
        Kind::Terrain | Kind::Everything => {
            let z = t * 4.0;
            let cam = camera(Vec3::new(0.0, 6.0, -30.0 + z), Vec3::new(0.0, 0.0, z));
            stats += draw(
                &mut target,
                &a.terrain,
                &Mat4::translate(Vec3::new(0.0, -2.0, 0.0)),
                &cam,
                &LIGHT,
                &Material {
                    texture: Some((&a.textures[2], Filter::Mipmap)),
                    ..material(Shading::Gouraud, [1.0, 1.0, 1.0])
                },
            );
            if let Kind::Everything = kind {
                let m = Material {
                    texture: Some((&a.textures[3], Filter::Mipmap)),
                    shininess: 24.0,
                    ..material(Shading::Phong, [1.0, 1.0, 1.0])
                };
                for i in 0..8 {
                    let at = Vec3::new(
                        (i as f32 - 3.5) * 3.0,
                        2.0 + libm::sinf(t + i as f32),
                        z - 10.0 + (i % 3) as f32 * 6.0,
                    );
                    stats += draw(
                        &mut target,
                        &a.small_sphere,
                        &(Mat4::translate(at) * Mat4::scale(1.4)),
                        &cam,
                        &LIGHT,
                        &m,
                    );
                }
            }
        }
        Kind::Objects => {
            let cam = camera(Vec3::new(0.0, 6.0, -14.0), Vec3::new(0.0, 0.0, 0.0));
            for i in 0..64 {
                let m = Material {
                    texture: Some((&a.textures[[0, 1, 3][i % 3]], Filter::Mipmap)),
                    shininess: if i % 3 == 2 { 32.0 } else { 4.0 },
                    ..material(Shading::Phong, [1.0, 1.0, 1.0])
                };
                let (x, z) = ((i % 8) as f32 - 3.5, (i / 8) as f32 - 3.5);
                let at = Vec3::new(x * 2.0, libm::sinf(t * 2.0 + x + z) * 0.5, z * 2.0);
                stats += draw(
                    &mut target,
                    &a.small_sphere,
                    &(Mat4::translate(at) * Mat4::scale(0.7)),
                    &cam,
                    &LIGHT,
                    &m,
                );
            }
        }
        Kind::Shadow(speed) => {
            let angle = t * speed;
            let light = Light {
                direction: Vec3::new(0.8 * libm::cosf(angle), 0.9, 0.8 * libm::sinf(angle))
                    .normalized(),
                ambient: 0.2,
            };
            let cam = camera(Vec3::new(0.0, 6.5, -11.0), Vec3::new(0.0, 0.8, 0.0));
            let torus = Mat4::translate(Vec3::new(0.0, 1.9, 0.0))
                * Mat4::rotate_y(t * 0.4)
                * Mat4::rotate_x(1.2);
            let cube = Mat4::translate(Vec3::new(3.2, 0.8, 1.0)) * Mat4::scale(0.8);
            let sphere = Mat4::translate(Vec3::new(-3.0, 1.2, -0.5)) * Mat4::scale(0.75);
            // It fits the picture (checked when built), so this never returns early.
            let Some(mut map) = ShadowMap::new(
                &mut a.picture,
                SHADOW_MAP,
                &light,
                Vec3::new(0.0, 1.0, 0.0),
                7.5,
            ) else {
                return stats;
            };
            for (mesh, model) in [(&a.torus, &torus), (&a.cube, &cube), (&a.sphere, &sphere)] {
                map.cast(mesh, model);
            }
            let textured = |i: usize, shading, shininess| Material {
                texture: Some((&a.textures[i], Filter::Mipmap)),
                shininess,
                ..material(shading, [1.0, 1.0, 1.0])
            };
            let copper = Material {
                shininess: 32.0,
                ..material(Shading::Phong, [0.85, 0.45, 0.25])
            };
            for (mesh, model, m) in [
                (
                    &a.ground,
                    &Mat4::IDENTITY,
                    textured(1, Shading::Gouraud, 0.0),
                ),
                (&a.torus, &torus, copper),
                (&a.cube, &cube, textured(0, Shading::Flat, 0.0)),
                (&a.sphere, &sphere, textured(3, Shading::Phong, 24.0)),
            ] {
                stats += draw_shadowed(&mut target, mesh, model, &cam, &light, &m, &map);
            }
        }
        Kind::Effect(_) | Kind::Desktop | Kind::Fractal => {}
    }
    stats
}

/// What a scene's run gave.
struct Outcome {
    frames: Vec<u32>,
    elapsed_us: u64,
    peak_c: Option<u32>,
    stopped_hot: bool,
    stats: Stats,
}

impl Outcome {
    fn fps(&self) -> u64 {
        self.frames.len() as u64 * 1_000_000 / self.elapsed_us.max(1)
    }

    /// The log line of ADR 0080's format, for this scene.
    fn line(&mut self, name: &str) -> String {
        self.frames.sort_unstable();
        let t = &self.frames;
        let at = |q: usize| {
            t.get((t.len() * q / 100).min(t.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        let n = t.len().max(1) as u64;
        format!(
            "bench3d.{name} us {} {} {} max={} n={} fps={} triangles={} pixels={} peak_c={}{}",
            at(50),
            at(95),
            at(99),
            t.last().copied().unwrap_or(0),
            t.len(),
            self.fps(),
            self.stats.triangles / n,
            self.stats.pixels / n,
            self.peak_c.map_or(String::from("-"), |c| format!("{c}")),
            if self.stopped_hot { " stopped=hot" } else { "" },
        )
    }
}

struct App {
    win: Window,
    system: Handle,
    text: Typesetter,
    /// The frame being drawn. The compositor reads the window's buffer whenever it
    /// makes a frame of the screen, so a frame drawn there shows half made: models cut
    /// and empty zones. Only a finished frame is copied there, by [`App::show`].
    back: Vec<u32>,
    samples: Vec<u32>,
}

/// The owner asked to stop: the close box or Esc.
struct Leave;

impl App {
    /// Looks at what the owner did, without waiting long.
    fn events(&mut self) -> Result<(), Leave> {
        loop {
            match self.win.poll(1) {
                Ok(Some(Event::Close)) => return Err(Leave),
                Ok(Some(Event::KeyCode {
                    code: KEY_ESC,
                    state: KeyState::Pressed,
                    ..
                })) => return Err(Leave),
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(_) => return Err(Leave),
            }
        }
    }

    /// The frame being drawn, to paint on.
    fn screen(&mut self) -> Canvas<'_> {
        Canvas::new(&mut self.back, W as u32, H as u32, W as u32).expect("frame buffer")
    }

    /// Copies the finished frame to the window in one pass and shows it.
    fn show(&mut self) {
        self.win.canvas().pixels.copy_from_slice(&self.back);
        let _ = self.win.present();
    }

    /// Words on the frame, on a dark band so they read over any scene.
    fn hud(&mut self, lines: &[String]) {
        let mut c =
            Canvas::new(&mut self.back, W as u32, H as u32, W as u32).expect("frame buffer");
        let band = Rect::new(12, 12, 360, 22 * lines.len() as u32 + 12);
        c.rounded(&band, 8, rgb(0x10, 0x14, 0x1c));
        for (i, l) in lines.iter().enumerate() {
            let face = if i == 0 {
                INTER_SEMIBOLD
            } else {
                INTER_REGULAR
            };
            let at = (24, 34 + 22 * i as i32);
            self.text
                .draw(&mut c, (Face(face), 14), l, at, rgb(0xF0, 0xF0, 0xF0));
        }
    }

    /// Waits for the processor to cool below `COOL_C`, saying so on screen.
    fn cool_down(&mut self, next: &str) -> Result<(), Leave> {
        loop {
            let Some((c, _)) = temperature(self.system) else {
                return Ok(());
            };
            if c < COOL_C {
                return Ok(());
            }
            self.screen().fill(rgb(0x18, 0x20, 0x30));
            self.hud(&[
                format!("Next: {next}"),
                format!("Cooling: {c} C, it starts below {COOL_C} C"),
            ]);
            self.show();
            for _ in 0..50 {
                self.events()?;
                sys::thread_sleep(10);
            }
        }
    }

    /// Runs one scene for its time, or `quick` frames.
    fn run(
        &mut self,
        name: &str,
        kind: Kind,
        a: &mut Assets,
        quick: u64,
    ) -> Result<Outcome, Leave> {
        let mut out = Outcome {
            frames: core::mem::take(&mut self.samples),
            elapsed_us: 0,
            peak_c: None,
            stopped_hot: false,
            stats: Stats::default(),
        };
        let start = now_us(self.system);
        let mut next_look = start;
        let mut current = String::new();
        loop {
            let f0 = now_us(self.system);
            let t = (f0 - start) as f32 / 1e6;
            let stats = frame(kind, t, a, &mut self.back);
            out.stats += stats;
            let n = out.frames.len() as u64;
            let elapsed = (f0 - start).max(1);
            let fps = n * 1_000_000 / elapsed;
            let ms = out.frames.last().map_or(0, |&us| us / 100);
            if f0 >= next_look {
                // The temperature is looked at every half second, not each frame.
                next_look = f0 + 500_000;
                if let Some((c, _)) = temperature(self.system) {
                    out.peak_c = Some(out.peak_c.map_or(c, |p| p.max(c)));
                    current = format!("{c} C");
                    if c >= STOP_C {
                        out.stopped_hot = true;
                    }
                }
                self.events()?;
            }
            self.hud(&[
                String::from(name),
                format!("{fps} fps, {}.{} ms a frame", ms / 10, ms % 10),
                format!("{} triangles, {} pixels", stats.triangles, stats.pixels),
                if current.is_empty() {
                    String::from("temperature: not readable")
                } else {
                    current.clone()
                },
            ]);
            self.show();
            let f1 = now_us(self.system);
            out.frames.push((f1 - f0).min(u32::MAX as u64) as u32);
            out.elapsed_us = f1 - start;
            let done = if quick > 0 {
                out.frames.len() as u64 >= quick
            } else {
                out.elapsed_us >= SCENE_US
            };
            if done || out.stopped_hot || out.frames.len() == out.frames.capacity() {
                return Ok(out);
            }
        }
    }
}

/// A buffer of `n` copies of `v`, or `None` when the heap has no room for it.
fn buffer<T: Copy>(n: usize, v: T) -> Option<Vec<T>> {
    let mut b = Vec::new();
    b.try_reserve_exact(n).ok()?;
    b.resize(n, v);
    Some(b)
}

/// The scenes' meshes and textures, and the frame to draw on.
fn assets(hw: Handle, system: Handle) -> Option<(Assets, Vec<u32>, Vec<u32>)> {
    let heap = sys::dma_alloc(hw, MORE_HEAP as u64)
        .ok()
        .and_then(|d| sys::mmio_map(d).ok())?;
    // SAFETY: a new mapping of whole pages, never unmapped, used by nothing else.
    unsafe { aricode_rt::extend_heap(heap as usize, MORE_HEAP) };
    // The large buffers first, so a heap too small says so instead of stopping.
    let back = buffer(W * H, 0u32)?;
    let depth = buffer(W * H, u16::MAX)?;
    let picture = buffer(W * H, 0u32)?;
    let texture_start = now_us(system);
    let textures = materials::make()?;
    let texture_us = now_us(system) - texture_start;
    let terrain = Mesh::terrain(120.0, 96, 4.0)?;
    let ground = Mesh::terrain(24.0, 16, 0.0)?;
    let a = Assets {
        torus: Mesh::torus(1.3, 0.5, 64, 32)?,
        sphere: Mesh::sphere(1.6, 64, 128)?,
        small_sphere: Mesh::sphere(0.6, 12, 24)?,
        cube: Mesh::cube()?,
        terrain,
        ground,
        textures,
        texture_us,
        depth,
        picture,
    };
    let mut samples = buffer(4096, 0u32)?;
    samples.clear();
    // Leave a contiguous 256 KiB for the bounded 96 KiB glyph cache, font
    // hinting/scratch and small HUD/results strings. Fail here, before any scene.
    let reserve = buffer(256 * 1024, 0u8)?;
    drop(reserve);
    Some((a, back, samples))
}

fn main(arg: u64) -> u64 {
    let (mut session, mut hw, mut system) = (None, None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            session = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        } else if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        }
    });
    let (Some(session), Some(hw), Some(system)) = (session, hw, system) else {
        say("started without a compositor session, HARDWARE or SYSTEM");
        return 1;
    };
    say("making its scenes");
    let startup = now_us(system);
    let Some((mut a, back, samples)) = assets(hw, system) else {
        say("no memory for its scenes");
        return 2;
    };
    let Ok(win) =
        Window::create_wanting(session, hw, W as u32, H as u32, "Bench3D", wants::KEY_CODES)
    else {
        say("could not create its window");
        return 3;
    };
    say("window open");
    let mut app = App {
        win,
        system,
        text: Typesetter::new(),
        back,
        samples,
    };
    say(&format!(
        "startup_us={} texture_us={} heap_free_bytes={} dma_free_kib={SPARE_KIB}",
        now_us(system) - startup,
        a.texture_us,
        aricode_rt::heap_stats().free
    ));
    let quick = arg;
    let head = header(system);
    sys::print(head.as_bytes());
    sys::print(b"\n");
    let mut results: Vec<(&str, u64, bool)> = Vec::new();
    for (name, kind) in SCENES {
        if quick == 0 && app.cool_down(name).is_err() {
            break;
        }
        if let Kind::Effect(_) = kind {
            // What the 2D effects filter: a frame of the heaviest scene, made outside
            // the measured frames, since the shadow scenes use the same memory.
            let mut picture = core::mem::take(&mut a.picture);
            frame(Kind::Everything, 1.0, &mut a, &mut picture);
            a.picture = picture;
        }
        let Ok(mut out) = app.run(name, kind, &mut a, quick) else {
            say("stopped by the owner");
            break;
        };
        let line = out.line(name);
        sys::print(line.as_bytes());
        sys::print(b"\n");
        results.push((name, out.fps(), out.stopped_hot));
        app.samples = core::mem::take(&mut out.frames);
        app.samples.clear();
        if quick > 0
            && matches!(
                name,
                "texture-linear" | "objects" | "shadow-fast" | "fractal" | "everything"
            )
        {
            // Outside the measured frames: let the compositor and QMP snapshot
            // observe this completed scene before the next one replaces it.
            sys::thread_sleep(500);
        }
    }
    say(&format!("{} scenes measured", results.len()));
    say(&format!(
        "heap_free_bytes={} after scenes",
        aricode_rt::heap_stats().free
    ));
    if quick > 0 {
        let _ = app.win.close();
        return 0;
    }
    // The table stays until the owner closes it.
    app.screen().fill(rgb(0x18, 0x20, 0x30));
    let mut lines = alloc::vec![String::from("bench3d: frames a second, drawn by the CPU")];
    for (name, fps, hot) in &results {
        lines.push(format!(
            "{name}: {fps}{}",
            if *hot { "  (stopped at 90 C)" } else { "" }
        ));
    }
    app.hud(&lines);
    app.show();
    while app.events().is_ok() {
        sys::thread_sleep(100);
    }
    let _ = app.win.close();
    0
}

aricode_rt::entry!(main);
