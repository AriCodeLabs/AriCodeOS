// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Pista: a kart racing game, drawn by the CPU (`aricode_pista`): the window and
//! the keyboard around it. The picture is drawn at half size, then doubled.
//!
//! Arrows or WASD drive, Space slides, Enter restarts after the race, Esc leaves.
//!
//! `arg` N > 0 drives itself for N frames as fast as it can, says how long each
//! part took, and ends (the `gui.pista` scenario, and measuring a machine).
#![no_std]
#![no_main]

use aricode_gfx::rgb;
use aricode_pista::raster::Target;
use aricode_pista::render::{draw, View};
use aricode_pista::world::{Input, Phase, World, COUNTDOWN, LAPS, RATE};
use aricode_rt::sys::window::{wants, Event, KeyState, Window};
use aricode_rt::sys::{self, abi, kind, permissions, Handle};
use core::cell::UnsafeCell;

const WIDTH: usize = 480;
const HEIGHT: usize = 270;
const FRAME_US: u64 = 1_000_000 / RATE as u64;

// evdev key codes.
const KEY_ESC: u16 = 1;
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

/// What is too big for the stack, in `.bss`. The program has one thread.
struct Big<T>(UnsafeCell<T>);
// SAFETY: one thread; `main` takes the only reference, once.
unsafe impl<T> Sync for Big<T> {}
static PICTURE: Big<[u32; WIDTH * HEIGHT]> = Big(UnsafeCell::new([0; WIDTH * HEIGHT]));

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

/// The picture, doubled, on the window's buffer.
fn double(picture: &[u32], window: &mut Window) {
    let canvas = window.canvas();
    let target = &mut *canvas.pixels;
    let row_len = WIDTH * 2;
    for (row, line) in picture.as_chunks::<WIDTH>().0.iter().enumerate() {
        let (first, second) =
            target[row * 2 * row_len..(row * 2 + 2) * row_len].split_at_mut(row_len);
        for (pair, &px) in first.as_chunks_mut::<2>().0.iter_mut().zip(line) {
            *pair = [px, px];
        }
        second.copy_from_slice(first);
    }
}

/// Writes `v` zero-padded into the first `digits` bytes of `out`.
fn put(out: &mut [u8], v: u32, digits: usize) {
    let mut v = v;
    for b in out[..digits].iter_mut().rev() {
        *b = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// The words on the picture, on the window at its full size.
fn hud(window: &mut Window, world: &World, fps: u32) {
    let white = rgb(255, 255, 255);
    let shadow = rgb(0, 0, 0);
    let mut c = window.canvas();
    let mut say = |x: i32, y: i32, text: &[u8]| {
        let text = core::str::from_utf8(text).unwrap_or("");
        c.text(x + 1, y + 1, text, shadow, None);
        c.text(x, y, text, white, None);
    };
    let you = &world.karts[0];
    let mut line = *b"Lap 0/0   Place 0/4";
    put(&mut line[4..], (you.laps + 1).min(LAPS), 1);
    put(&mut line[6..], LAPS, 1);
    put(&mut line[16..], world.position() as u32, 1);
    say(16, 12, &line);
    let t = world.clock / RATE as u32;
    let mut line = *b"Time 00:00";
    put(&mut line[5..], t / 60, 2);
    put(&mut line[8..], t % 60, 2);
    say(16, 30, &line);
    let mut line = *b"000 km/h";
    put(&mut line, ((you.speed >> 16) * 36 / 10) as u32, 3);
    say(16, 510, &line);
    if you.boost > 0 {
        say(100, 510, b"TURBO");
    }
    let mut line = *b"000 fps";
    put(&mut line, fps.min(999), 3);
    say(880, 510, &line);
    match world.phase {
        Phase::Countdown => {
            let left = COUNTDOWN.saturating_sub(world.step) / RATE as u32 + 1;
            let mut line = *b"0";
            put(&mut line, left.min(9), 1);
            say(476, 150, &line);
            say(376, 180, b"Arrows drive, Space slides");
        }
        Phase::Finished => {
            let mut line = *b"Finished in place 0";
            put(&mut line[18..], world.position() as u32, 1);
            say(404, 150, &line);
            say(348, 180, b"Enter to race again, Esc to leave");
        }
        Phase::Racing => {}
    }
}

/// `frames` frames with nobody at the wheel, as fast as they can go.
fn measure(mut window: Window, system: Handle, picture: &mut [u32], frames: u64) -> u64 {
    let mut world = World::new();
    let mut view = View::default();
    let (mut sim, mut paint, mut blit, mut show) = (0, 0, 0, 0);
    let start = now_us(system);
    for _ in 0..frames {
        let a = now_us(system);
        let input = world.autopilot(0);
        world.advance(input);
        let b = now_us(system);
        let mut target = Target {
            pixels: &mut *picture,
            width: WIDTH,
            height: HEIGHT,
        };
        draw(&world, &mut view, &mut target);
        let c = now_us(system);
        double(picture, &mut window);
        hud(&mut window, &world, 0);
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
    let (w, h) = (WIDTH as u32 * 2, HEIGHT as u32 * 2);
    let Ok(mut window) = Window::create_wanting(session, hw, w, h, "Pista", wants::KEY_CODES)
    else {
        sys::print(b"pista: could not create the window\n");
        return 2;
    };
    // SAFETY: the only reference to it, in the program's one thread.
    let picture: &mut [u32] = unsafe { &mut *PICTURE.0.get() };
    if arg > 0 {
        return measure(window, system, picture, arg);
    }
    let mut world = World::new();
    let mut view = View::default();
    let mut input = Input::default();
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
                Ok(Some(Event::Focus(false))) => input = Input::default(),
                Ok(Some(Event::KeyCode { code, state, .. })) => {
                    let down = state != KeyState::Released;
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
                        KEY_ENTER if down && world.phase == Phase::Finished => {
                            world = World::new();
                            view = View::default();
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
            world.advance(input);
            next += FRAME_US;
            steps += 1;
        }
        if next <= t {
            next = t + FRAME_US;
        }
        let mut target = Target {
            pixels: &mut *picture,
            width: WIDTH,
            height: HEIGHT,
        };
        draw(&world, &mut view, &mut target);
        double(picture, &mut window);
        hud(&mut window, &world, fps);
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
