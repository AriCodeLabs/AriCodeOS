// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Pista: a kart racing game, drawn by the CPU (`aricode_pista`): the window and
//! the keyboard around it. Q switches between native resolution and fast 2x scaling.
//!
//! Arrows/WASD drive, Space drifts, P pauses, R recovers, Q changes quality.
//! Enter resumes/restarts, Esc leaves.
//!
//! `arg` N > 0 drives itself for N frames as fast as it can, says how long each
//! part took, and ends (the `gui.pista` scenario, and measuring a machine).
#![no_std]
#![no_main]

use aricode_pista::hud::{self, Overlay};
use aricode_pista::raster::Target;
use aricode_pista::render::{draw, View};
use aricode_pista::world::{Input, Phase, World, RATE};
use aricode_rt::sys::window::{wants, Event, KeyState, Window};
use aricode_rt::sys::{self, abi, kind, permissions, Handle};

const WIDTH: usize = 960;
const HEIGHT: usize = 540;
const FRAME_US: u64 = 1_000_000 / RATE as u64;

// evdev key codes.
const KEY_ESC: u16 = 1;
const KEY_P: u16 = 25;
const KEY_Q: u16 = 16;
const KEY_R: u16 = 19;
const KEY_W: u16 = 17;
const KEY_A: u16 = 30;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_ENTER: u16 = 28;
const KEY_SPACE: u16 = 57;
const KEY_UP: u16 = 103;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_DOWN: u16 = 108;

/// Large buffers live in mapped memory: the ELF's static user region is only
/// 4 MiB, independently of the application's manifest memory allowance.
struct Buffers {
    address: u64,
}
impl Buffers {
    fn new(hw: Handle) -> Option<Self> {
        let dma = sys::dma_alloc(hw, (WIDTH * HEIGHT * 8) as u64).ok()?;
        let address = sys::mmio_map(dma);
        // A successful mapping retains the allocation until it is unmapped.
        let _ = sys::handle_close(dma);
        Some(Self {
            address: address.ok()?,
        })
    }
    fn slices(&mut self) -> (&mut [u32], &mut [u32]) {
        // SAFETY: the mapping owns WIDTH*HEIGHT*8 bytes, is page aligned, and
        // remains alive for this exclusive borrow. The two slices are disjoint.
        let all = unsafe {
            core::slice::from_raw_parts_mut(self.address as *mut u32, WIDTH * HEIGHT * 2)
        };
        all.split_at_mut(WIDTH * HEIGHT)
    }
}
impl Drop for Buffers {
    fn drop(&mut self) {
        let _ = sys::mmio_unmap(self.address);
    }
}

fn now_us(system: Handle) -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME_US, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(v, &mut b));
}

/// Native output or nearest-neighbour 2x; both modes share the full-size HUD.
fn present_picture(picture: &[u32], window: &mut Window, world: &World, overlay: &Overlay) {
    let canvas = window.canvas();
    let target = &mut *canvas.pixels;
    if overlay.high_quality {
        target[..WIDTH * HEIGHT].copy_from_slice(&picture[..WIDTH * HEIGHT]);
    } else {
        for (row, line) in picture[..WIDTH * HEIGHT / 4]
            .as_chunks::<{ WIDTH / 2 }>()
            .0
            .iter()
            .enumerate()
        {
            let (first, second) =
                target[row * 2 * WIDTH..(row * 2 + 2) * WIDTH].split_at_mut(WIDTH);
            for (pair, &px) in first.as_chunks_mut::<2>().0.iter_mut().zip(line) {
                pair.fill(px);
            }
            second.copy_from_slice(first);
        }
    }
    hud::draw(
        &mut Target {
            pixels: target,
            depth: &mut [],
            width: WIDTH,
            height: HEIGHT,
        },
        world,
        overlay,
    );
}

fn render_picture(
    world: &World,
    view: &mut View,
    picture: &mut [u32],
    depth: &mut [u32],
    high_quality: bool,
) {
    let scale = if high_quality { 1 } else { 2 };
    let (width, height) = (WIDTH / scale, HEIGHT / scale);
    draw(
        world,
        view,
        &mut Target {
            pixels: &mut picture[..width * height],
            depth: &mut depth[..width * height],
            width,
            height,
        },
    );
}

/// `frames` frames with nobody at the wheel, as fast as they can go.
fn measure(
    mut window: Window,
    system: Handle,
    picture: &mut [u32],
    depth: &mut [u32],
    frames: u64,
) -> u64 {
    let mut world = World::new();
    let mut view = View::default();
    let (mut sim, mut paint, mut blit, mut show) = (0, 0, 0, 0);
    let start = now_us(system);
    for _ in 0..frames {
        let a = now_us(system);
        let input = world.autopilot(0);
        world.advance(input);
        let b = now_us(system);
        render_picture(&world, &mut view, picture, depth, true);
        let c = now_us(system);
        present_picture(
            picture,
            &mut window,
            &world,
            &Overlay {
                high_quality: true,
                ..Overlay::default()
            },
        );
        let d = now_us(system);
        if window.present().is_err() {
            return 3;
        }
        let e = now_us(system);
        sim += b - a;
        paint += c - b;
        blit += d - c;
        show += e - d;
    }
    let all = now_us(system) - start;
    sys::print(b"pista: ");
    num(frames);
    sys::print(b" frames: world ");
    num(sim / frames);
    sys::print(b" us, 3D ");
    num(paint / frames);
    sys::print(b" us, doubling and text ");
    num(blit / frames);
    sys::print(b" us, present ");
    num(show / frames);
    sys::print(b" us; ");
    num(frames * 1_000_000 / all.max(1));
    sys::print(b" frames a second\n");
    let _ = window.close();
    0
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
        sys::print(b"pista: started without a compositor session or a clock\n");
        return 1;
    };
    let (w, h) = (WIDTH as u32, HEIGHT as u32);
    let Ok(mut window) = Window::create_wanting(session, hw, w, h, "Pista", wants::KEY_CODES)
    else {
        sys::print(b"pista: could not create the window\n");
        return 2;
    };
    let Some(mut buffers) = Buffers::new(hw) else {
        sys::print(b"pista: could not allocate graphics buffers\n");
        let _ = window.close();
        return 4;
    };
    let (picture, depth) = buffers.slices();
    if arg > 0 {
        return measure(window, system, picture, depth, arg);
    }
    let mut world = World::new();
    let mut view = View::default();
    let mut input = Input::default();
    let mut overlay = Overlay {
        high_quality: true,
        ..Overlay::default()
    };
    let mut next = now_us(system);
    let (mut frames, mut since, mut fps) = (0u32, next, 0u32);
    loop {
        // The time until the next frame goes in waiting for keys.
        loop {
            let left = next.saturating_sub(now_us(system));
            let wait = (left / 1000).clamp(1, 16) as u32;
            match window.poll(wait) {
                Ok(Some(Event::Close)) => {
                    let _ = window.close();
                    return 0;
                }
                Ok(Some(Event::Focus(false))) => {
                    input = Input::default();
                    overlay.paused = world.phase != Phase::Finished;
                }
                Ok(Some(Event::KeyCode { code, state, .. })) => {
                    let down = state != KeyState::Released;
                    let pressed = state == KeyState::Pressed;
                    match code {
                        KEY_UP | KEY_W => input.throttle = down,
                        KEY_DOWN | KEY_S => input.brake = down,
                        KEY_LEFT | KEY_A => input.left = down,
                        KEY_RIGHT | KEY_D => input.right = down,
                        KEY_SPACE => input.drift = down,
                        KEY_ESC if down => {
                            let _ = window.close();
                            return 0;
                        }
                        KEY_P if pressed && world.phase != Phase::Finished => {
                            overlay.paused = !overlay.paused;
                            input = Input::default();
                            next = now_us(system);
                        }
                        KEY_Q if pressed => overlay.high_quality = !overlay.high_quality,
                        KEY_R if pressed && world.phase == Phase::Racing => {
                            world.recover();
                            view = View::default();
                            input = Input::default();
                            overlay.paused = false;
                            next = now_us(system);
                        }
                        KEY_ENTER if pressed => {
                            if world.phase == Phase::Finished {
                                world = World::new();
                                view = View::default();
                            }
                            overlay.paused = false;
                            input = Input::default();
                            next = now_us(system);
                        }
                        _ => {}
                    }
                }
                Ok(_) => {}
                Err(_) => return 3,
            }
            if now_us(system) >= next {
                break;
            }
        }
        // The world keeps its own time: a slow frame does not slow the race.
        let t = now_us(system);
        let mut steps = 0;
        while next <= t && steps < 4 {
            if !overlay.paused {
                world.advance(input);
            }
            next += FRAME_US;
            steps += 1;
        }
        if next <= t {
            next = t + FRAME_US;
        }
        let mut paused_view = view;
        let camera = if overlay.paused {
            &mut paused_view
        } else {
            &mut view
        };
        render_picture(&world, camera, picture, depth, overlay.high_quality);
        overlay.fps = fps;
        present_picture(picture, &mut window, &world, &overlay);
        if window.present().is_err() {
            return 3;
        }
        frames += 1;
        if t - since >= 1_000_000 {
            fps = frames;
            frames = 0;
            since = t;
        }
    }
}

aricode_rt::entry!(main);
