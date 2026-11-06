// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Compositor (Phase 26): owner of the framebuffer in ring 3. Handles: 0x10004 =
//! service endpoint, 0x10005 = framebuffer (MMIO), 0x10006 = SYSTEM,
//! 0x10007 = HARDWARE (back buffer), 0x10008.. = input services
//! (`arg` = how many). Composes into a back buffer and flushes it to the screen,
//! only the damaged areas and at most once per `FRAME_MS`; routes keyboard
//! and mouse to the focused window.
#![no_std]
#![no_main]

use aricode_cmdline::{keys, CmdLine};
use aricode_gfx::{
    resized, snap_at, snap_content, Canvas, Damage, Desktop, Hit, PixelMasks, Rect, Scene, Window,
};
use aricode_input::{
    EscapeChord, KeyRepeat, Keyboard, Layout, BTN_LEFT, EV_KEY, EV_REL, REL_WHEEL, REL_X, REL_Y,
};
use aricode_proto::input::Status as InputStatus;
use aricode_proto::input::MAX_EVENTS;
use aricode_proto::window::{mods, wants, Event, KeyState, Op, Reply, Request, Status, MAX_SIDE};
use aricode_rt::sys::input::{DriverInstance, InputClient, InputError};
use aricode_rt::sys::{self, abi, Handle};
use aricode_rt::{fmt_hex, fmt_u64};

const SERVICE: Handle = Handle(0x0001_0004);
const FRAMEBUFFER: Handle = Handle(0x0001_0005);
const SYSTEM: Handle = Handle(0x0001_0006);
const HW: Handle = Handle(0x0001_0007);
const FIRST_INPUT: u32 = 0x0001_0008;
/// Pause before reading again from a driver that died holding the read
/// (ADR 0030): its supervisor relaunches it on the same endpoint.
const RETRY_MS: u64 = 50;
/// Input services: up to four USB or virtio ones and the PS/2 one (ADR 0075).
const MAX_INPUTS: usize = 5;
const MAX_WINDOWS: usize = aricode_gfx::MAX_WINDOWS;
/// evdev codes the desktop itself answers to.
const KEY_TAB: u16 = 15;
const KEY_LEFTALT: u16 = 56;
/// A title bar moved less than this was clicked, not dragged: it does not snap.
const MIN_DRAG: i32 = 4;
const QUEUE: usize = 32;
const _: () = assert!(aricode_input::mods::CAPS == aricode_proto::window::mods::CAPS);
/// Client sessions (ADR 0052): one per program the broker launches with
/// `window`.
const MAX_SESSIONS: usize = 8;
/// Windows one session may have open: a client cannot fill the table
/// (audit of ADR 0052, M2).
const MAX_WINDOWS_PER_SESSION: usize = 2;
/// The longest a `Poll` waits: a reply handle is not held for days.
const MAX_POLL_MS: u32 = 60_000;
/// How often every session is asked whether its client is still there.
const SWEEP_MS: u64 = 100;
/// What arrives on the main endpoint (init): only `Attach` and `Detach`.
const MAIN: usize = 0;
/// Shortest interval between two frames (about 60 per second): changes
/// that arrive sooner are coalesced into the next frame, so an event waits
/// at most one frame.
const FRAME_MS: u64 = 16;
/// How long a request to close may wait untaken before the next click on
/// the close box closes the window for its program (ADR 0077).
const FORCE_CLOSE_MS: u64 = 2000;
/// An input event handled later than this after its driver received it
/// is late: the pointer trails the hand (incident 0058). Each is counted
/// and recorded in the kernel event ring as `gui input late`, at most once
/// a second, so `diag` shows it.
const LATE_MS: u64 = 50;

fn say(msg: &[u8]) {
    sys::print(b"compositor: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(fmt_u64(v, &mut b));
}

fn now(system: Handle) -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

/// Screen: description, framebuffer mapping and back buffer in RAM.
struct Screen {
    info: abi::sysinfo::Framebuffer,
    pixels: Pixels,
    fb: *mut u32,
    back: &'static mut [u32],
}

/// How a back buffer pixel (`0x00RRGGBB`) is written to the screen.
#[derive(Clone, Copy)]
enum Pixels {
    /// Blue in the low byte: as it is.
    Bgr,
    Rgb,
    Masks(PixelMasks),
}

impl Screen {
    fn open() -> Result<Self, u64> {
        let mut b = [0u8; abi::sysinfo::Framebuffer::LEN];
        let n = sys::system_info(SYSTEM, abi::sysinfo::FRAMEBUFFER, &mut b).map_err(|_| 10u64)?;
        let info = abi::sysinfo::Framebuffer::decode(&b[..n]).ok_or(11u64)?;
        let fb = sys::mmio_map(FRAMEBUFFER).map_err(|_| 12u64)? as *mut u32;
        let pixels = info.stride as u64 * info.height as u64;
        // A screen of less than 32 bits per pixel (its masks may say 16)
        // is smaller than this; `flip` writes 32 bits.
        if pixels * 4 > info.size {
            return Err(15);
        }
        let format = match info.format {
            abi::sysinfo::FB_FORMAT_RGB => Pixels::Rgb,
            abi::sysinfo::FB_FORMAT_BGR => Pixels::Bgr,
            _ => Pixels::Masks(
                PixelMasks::new(info.red_mask, info.green_mask, info.blue_mask).ok_or(16u64)?,
            ),
        };
        let dma = sys::dma_alloc(HW, pixels * 4).map_err(|_| 13u64)?;
        let virt = sys::mmio_map(dma).map_err(|_| 14u64)?;
        // SAFETY: our own DMA buffer of `pixels` u32, mapped RW; lives until exit.
        let back = unsafe { core::slice::from_raw_parts_mut(virt as *mut u32, pixels as usize) };
        Ok(Self {
            info,
            pixels: format,
            fb,
            back,
        })
    }

    fn canvas(&mut self) -> Canvas<'_> {
        Canvas::new(
            self.back,
            self.info.width,
            self.info.height,
            self.info.stride,
        )
        .expect("back buffer")
    }

    /// Flushes `rects` (inside the screen) of the back buffer to the
    /// screen, converting the pixel format.
    fn flip(&self, rects: &[Rect]) {
        match self.pixels {
            Pixels::Bgr => self.blit(rects, |px| px),
            Pixels::Rgb => self.blit(rects, |px| {
                (px & 0xFF) << 16 | (px & 0xFF00) | (px >> 16) & 0xFF
            }),
            Pixels::Masks(masks) => self.blit(rects, |px| masks.encode(px)),
        }
        // The framebuffer is write-combining: the last bursts stay in the
        // CPU's buffers until a fence.
        // SAFETY: a fence has no operands and touches no state.
        unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
    }

    fn blit(&self, rects: &[Rect], encode: impl Fn(u32) -> u32) {
        let stride = self.info.stride as usize;
        for r in rects {
            for y in r.y..r.bottom() {
                let row = y as usize * stride;
                for i in row + r.x as usize..row + r.right() as usize {
                    let v = encode(self.back[i]);
                    // SAFETY: `r` lies inside width x height, so `i` <
                    // stride*height <= size/4, inside the mapped framebuffer.
                    unsafe { self.fb.add(i).write_volatile(v) };
                }
            }
        }
    }
}

/// Window content: the mapped client buffer and its event queue.
struct Content {
    id: u32,
    /// The session it was created through: slot + 1.
    owner: usize,
    virt: u64,
    width: u32,
    height: u32,
    events: [Option<Event>; QUEUE],
    count: usize,
    pending: Option<(Handle, u64)>,
    /// What it asked for beyond the basic events (`wants`).
    wants: u32,
    /// What it presented since the last frame, in content coordinates: it goes on
    /// screen where the window is when the frame is made.
    dirty: Option<Rect>,
    /// The place asked of its program, position and size: the window goes there
    /// when that size arrives, and any other size leaves it where it is.
    place_at: Option<Rect>,
    /// When a request to close was handed to the client, queued or as the
    /// answer to its poll (ms since boot), while it has not polled since:
    /// it has not come back from it (ADR 0077, audit L4).
    close_waiting: Option<u64>,
}

impl Content {
    /// Queues `ev`; when full, the oldest event goes, never a request to
    /// close (audit of ADR 0077, M3).
    fn push(&mut self, ev: Event) {
        // The pointer's latest position and the wheel's sum say the same as
        // many events: a program that falls behind does not lose its keys.
        if let Some(last) = self.count.checked_sub(1).map(|i| &mut self.events[i]) {
            match (last, ev) {
                (Some(Event::Motion { .. }), Event::Motion { x, y, buttons }) => {
                    self.events[self.count - 1] = Some(Event::Motion { x, y, buttons });
                    return;
                }
                (Some(Event::Configure { .. }), Event::Configure { width, height }) => {
                    self.events[self.count - 1] = Some(Event::Configure { width, height });
                    return;
                }
                (Some(Event::Wheel { delta: sum, .. }), Event::Wheel { x, y, delta }) => {
                    let delta = sum.saturating_add(delta);
                    self.events[self.count - 1] = Some(Event::Wheel { x, y, delta });
                    return;
                }
                _ => {}
            }
        }
        if self.count == QUEUE {
            // Motion can go: the next says where the pointer is. What says a
            // thing once stays as long as anything else can go.
            let once = |e: &Option<Event>| {
                matches!(
                    e,
                    Some(
                        Event::Close
                            | Event::Release { .. }
                            | Event::Focus(_)
                            | Event::Leave
                            | Event::Configure { .. }
                    )
                )
            };
            let drop = (0..QUEUE)
                .find(|&i| !once(&self.events[i]))
                .or((0..QUEUE).find(|&i| self.events[i] != Some(Event::Close)))
                .unwrap_or(0);
            self.events.copy_within(drop + 1..QUEUE, drop);
            self.count -= 1;
        }
        self.events[self.count] = Some(ev);
        self.count += 1;
    }

    fn close_queued(&self) -> bool {
        self.events[..self.count].contains(&Some(Event::Close))
    }
    fn pop(&mut self) -> Option<Event> {
        if self.count == 0 {
            return None;
        }
        let ev = self.events[0];
        self.events.copy_within(1..self.count, 0);
        self.count -= 1;
        ev
    }
}

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// `reply` carrying `handle` (moved to the client).
fn answer_with(reply_to: Handle, reply: Reply, handle: Handle) {
    if sys::ipc_send(reply_to, &reply.encode(), &[handle], 0).is_err() {
        let _ = sys::handle_close(handle);
    }
    let _ = sys::handle_close(reply_to);
}

/// The keyboard and what repeats it: a device's lost releases must stop both.
struct Keys {
    keyboard: Keyboard,
    repeat: KeyRepeat,
    /// Keys the focused window saw pressed and not released: a change of focus
    /// tells it, and a release of any other key is not its concern.
    down: [Option<u16>; 8],
    /// A device's releases were lost: the compositor forgets what is held too.
    lost: bool,
}

impl Keys {
    fn release_held(&mut self) {
        self.keyboard.release_modifiers();
        self.repeat.stop();
        self.lost = true;
    }
}

/// The pointer as windows see it (ADR 0078, stage 0).
#[derive(Default)]
struct Pointer {
    /// It moved since windows were last told.
    moved: bool,
    /// The window whose content it is over.
    inside: Option<u32>,
    /// The window a held button was pressed on: it gets the motion and the
    /// presses and releases wherever the pointer goes.
    grab: Option<u32>,
    /// The held buttons it saw pressed, and so will see released.
    grabbed: u8,
    drag: Option<Drag>,
    /// Buttons held, bit 0 = left.
    buttons: u8,
}

/// The owner dragging a window's title bar or an edge.
#[derive(Clone, Copy)]
struct Drag {
    id: u32,
    from: (i32, i32),
    start: Rect,
    /// `None` moves it; otherwise the `edge` bits being dragged.
    edges: Option<u8>,
}

/// Alt+Tab in progress: the windows in order, whether each was minimized when it
/// began, how many, and which now.
#[derive(Clone, Copy)]
struct Switching {
    order: [u32; MAX_WINDOWS],
    hidden: [bool; MAX_WINDOWS],
    n: usize,
    at: usize,
}

struct Compositor {
    screen: Screen,
    desktop: Desktop,
    contents: [Option<Content>; MAX_WINDOWS],
    keys: Keys,
    pointer: Pointer,
    switching: Option<Switching>,
    /// Ctrl+Alt+Backspace: the owner asks for the text console (ADR 0074).
    escape: EscapeChord,
    /// When a key was last pressed (ms since boot): a window opened after
    /// that by a session made before it does not take the focus.
    last_key_ms: Option<u64>,
    /// What the screen shows, and what changed since (window contents
    /// presented by their clients, plus the diff of the scene).
    shown: Scene,
    damage: Damage,
    next_frame_ms: u64,
    frames: u64,
    announced: bool,
    /// `gui_trace` on the command line: log every frame.
    trace: bool,
    /// Receive sides of the client sessions (ADR 0052) and their ids.
    sessions: [Option<Session>; MAX_SESSIONS],
    /// Generation of the next session id, so a stale `Detach` names no
    /// later session in the same slot.
    generation: u32,
    /// The session listed first in the next receive: it rotates, so one
    /// client's flood cannot starve the others (audit of ADR 0052, M1).
    first: usize,
    last_sweep_ms: u64,
    /// Late input events, recorded at most once a second (`LATE_MS`).
    late: aricode_faults::Meter<1>,
}

#[derive(Clone, Copy)]
struct Session {
    endpoint: Handle,
    /// `generation << 8 | slot`.
    id: u32,
    /// When it was made (ms since boot).
    made_ms: u64,
    /// It has opened a window already.
    opened: bool,
}

impl Compositor {
    fn content(&mut self, id: u32) -> Option<&mut Content> {
        self.contents.iter_mut().flatten().find(|c| c.id == id)
    }

    /// Window `id` if `owner` created it: another client's windows answer
    /// as if they did not exist (audit 2026-10-25 C, M5: any client could
    /// poll another's keys or close its window).
    fn owned(&mut self, id: u32, owner: usize) -> Option<&mut Content> {
        self.content(id).filter(|c| c.owner == owner)
    }

    /// `Attach`: a new session endpoint; the compositor keeps its receive
    /// side, the client gets the send side and init the id.
    fn attach(&mut self, reply_to: Handle, now_ms: u64) {
        if self.sessions.iter().all(Option::is_some) {
            // Clients that died since the last sweep free their slots first.
            self.reap_gone(now_ms);
        }
        let Some(slot) = self.sessions.iter().position(Option::is_none) else {
            return answer(reply_to, Reply::status(Status::Busy));
        };
        let Ok((endpoint, send)) = sys::endpoint_create_pair() else {
            return answer(reply_to, Reply::status(Status::Busy));
        };
        self.generation = (self.generation + 1) & 0x00ff_ffff;
        let id = self.generation << 8 | slot as u32;
        self.sessions[slot] = Some(Session {
            endpoint,
            id,
            made_ms: now_ms,
            opened: false,
        });
        answer_with(reply_to, Reply::ok(id as u64), send);
    }

    /// `Detach`: init saw the client of session `id` exit. Closing the
    /// endpoint also cuts off any other holder of its send side, such as a
    /// pending `Poll` that was handed the session itself as its reply
    /// endpoint (audit of ADR 0052, M2).
    fn detach(&mut self, reply_to: Handle, id: u32) {
        let Some(slot) = self
            .sessions
            .iter()
            .position(|s| s.is_some_and(|s| s.id == id))
        else {
            // Already retired by the sweep.
            return answer(reply_to, Reply::status(Status::BadWindow));
        };
        if let Some(s) = self.sessions[slot] {
            let _ = sys::endpoint_close(s.endpoint);
        }
        self.retire(slot);
        answer(reply_to, Reply::ok(0));
    }

    /// Retires every session whose client side is gone; a message found
    /// on the way is served. `PeerClosed` comes once the queue is empty and
    /// no send side is left.
    fn reap_gone(&mut self, now_ms: u64) {
        let mut msg = [0u8; Request::LEN];
        let mut handles = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        for slot in 0..MAX_SESSIONS {
            let Some(s) = self.sessions[slot] else {
                continue;
            };
            match sys::ipc_receive(s.endpoint, &mut msg, &mut handles, 0) {
                Ok((n, nh)) => serve(self, slot + 1, &msg[..n], &handles[..nh], now_ms),
                Err(e) if e.endpoint_gone() => self.retire(slot),
                Err(_) => {}
            }
        }
    }

    /// The client of session `slot` is gone: its endpoint closes and every
    /// window it made goes with it, so a dead client's window neither keeps
    /// the focus nor fills the table (audit 2026-10-25 C, M5).
    fn retire(&mut self, slot: usize) {
        let Some(s) = self.sessions[slot].take() else {
            return;
        };
        let _ = sys::handle_close(s.endpoint);
        let mut closed_windows = 0;
        for i in 0..self.contents.len() {
            if !self.contents[i]
                .as_ref()
                .is_some_and(|c| c.owner == slot + 1)
            {
                continue;
            }
            let Some(closed) = self.contents[i].take() else {
                continue;
            };
            if let Some((h, _)) = closed.pending {
                answer(h, Reply::status(Status::BadWindow));
            }
            self.unmap_unused(closed.virt);
            if let Some(w) = self.desktop.get(closed.id) {
                self.damage.add(&w.frame());
            }
            self.desktop.remove(closed.id);
            closed_windows += 1;
        }
        sys::print(b"compositor: client ");
        num(slot as u64 + 1);
        sys::print(b" gone: ");
        num(closed_windows);
        sys::print(b" window(s) closed\n");
        self.refocus();
    }

    /// `deliver` if the window asked for this kind of event.
    fn deliver_wanted(&mut self, id: u32, want: u32, ev: Event) {
        if self.content(id).is_some_and(|c| c.wants & want != 0) {
            self.deliver(id, ev);
        }
    }

    /// A point of the screen in the content of window `id`.
    fn in_content(&self, id: u32, px: i32, py: i32) -> Option<(i32, i32)> {
        let w = self.desktop.get(id)?;
        Some((px - w.content.x, py - w.content.y))
    }

    /// The window whose content is at a point of the screen: not its frame.
    fn content_at(&self, px: i32, py: i32) -> Option<u32> {
        match self.desktop.hit(px, py) {
            Hit::Window { id, .. } => self
                .desktop
                .get(id)
                .filter(|w| w.content.contains(px, py))
                .map(|w| w.id),
            _ => None,
        }
    }

    /// Tells windows where the pointer went, once per round: moving it makes
    /// two device events, one per axis, and one motion says it all.
    fn flush_pointer(&mut self) {
        if !core::mem::take(&mut self.pointer.moved) {
            return;
        }
        let (px, py) = self.desktop.pointer;
        if self.pointer.drag.is_some() {
            return self.continue_drag(px, py);
        }
        let under = self.pointer.grab.or_else(|| self.content_at(px, py));
        if under != self.pointer.inside {
            if let Some(old) = self.pointer.inside {
                self.deliver_wanted(old, wants::POINTER, Event::Leave);
            }
            if let Some((id, (x, y))) = under.and_then(|i| Some((i, self.in_content(i, px, py)?))) {
                self.deliver_wanted(id, wants::POINTER, Event::Enter { x, y });
            }
            self.pointer.inside = under;
        }
        if let Some((id, (x, y))) = under.and_then(|i| Some((i, self.in_content(i, px, py)?))) {
            let buttons = self.pointer.buttons;
            self.deliver_wanted(id, wants::POINTER, Event::Motion { x, y, buttons });
        }
    }

    /// Asks a window's program for a place on the screen. It moves at once if the
    /// size stays; otherwise when its program answers, so it is never shown cut.
    fn place(&mut self, id: u32, to: Rect) {
        let Some(w) = self.desktop.get(id).copied() else {
            return;
        };
        // No size a program can be asked for is outside these.
        let to = Rect::new(to.x, to.y, to.w.clamp(1, MAX_SIDE), to.h.clamp(1, MAX_SIDE));
        let same = (w.content.w, w.content.h) == (to.w, to.h);
        let asked = self.content(id).and_then(|c| c.place_at);
        if same && asked.is_none() {
            self.desktop.move_window(id, to.x, to.y);
            return;
        }
        if !w.resizable || asked.is_some_and(|a| a == to) {
            return;
        }
        if same {
            // Back to its size while another is asked: that one's answer must not
            // stay. The program is asked again, for this one.
            self.desktop.move_window(id, to.x, to.y);
        }
        if let Some(c) = self.content(id) {
            c.place_at = (!same).then_some(to);
        }
        let ev = Event::Configure {
            width: to.w,
            height: to.h,
        };
        self.deliver_wanted(id, wants::RESIZE, ev);
    }

    fn continue_drag(&mut self, px: i32, py: i32) {
        let Some(d) = self.pointer.drag else {
            return;
        };
        let (dx, dy) = (px - d.from.0, py - d.from.1);
        match d.edges {
            None => {
                self.desktop
                    .move_window(d.id, d.start.x + dx, d.start.y + dy);
            }
            Some(edges) => self.place(d.id, resized(d.start, edges, dx, dy)),
        }
    }

    /// The button is up: a title bar left at the screen's edge snaps there.
    fn end_drag(&mut self, px: i32, py: i32) {
        let Some(d) = self.pointer.drag.take() else {
            return;
        };
        self.pointer.moved = true;
        let travelled = (px - d.from.0).abs().max((py - d.from.1).abs());
        if d.edges.is_some() || travelled < MIN_DRAG {
            return;
        }
        let screen = self.desktop.screen;
        let resizable = self.desktop.get(d.id).is_some_and(|w| w.resizable);
        if let Some(snap) = snap_at(px, py, screen.w, screen.h).filter(|_| resizable) {
            // Where to go back to is where it was first, not the last snap.
            if self.desktop.get(d.id).is_some_and(|w| w.restore.is_none()) {
                self.desktop.set_restore(d.id, Some(d.start));
            }
            self.place(d.id, snap_content(snap, screen.w, screen.h));
        }
    }

    /// The zoom button: fills the screen, or goes back.
    fn zoom(&mut self, id: u32) {
        let Some(w) = self.desktop.get(id).copied() else {
            return;
        };
        let screen = self.desktop.screen;
        match w.restore.filter(|_| w.resizable) {
            Some(back) => {
                self.desktop.set_restore(id, None);
                self.place(id, back);
            }
            None if w.resizable => {
                self.desktop.set_restore(id, Some(w.content));
                let full = snap_content(aricode_gfx::Snap::Full, screen.w, screen.h);
                self.place(id, full);
            }
            None => {}
        }
    }

    /// Alt+Tab: the next window in the order they had when Alt went down, brought
    /// forward, even a minimized one: it stays shown if Alt comes up there, and is
    /// minimized again if Tab goes past it. The window in use is first, and each Tab
    /// goes one further.
    fn cycle_windows(&mut self) {
        if self.switching.is_none() {
            let mut sw = Switching {
                order: [0; MAX_WINDOWS],
                hidden: [false; MAX_WINDOWS],
                n: 0,
                at: 0,
            };
            let mut front_first = [None; MAX_WINDOWS];
            for (slot, w) in front_first.iter_mut().zip(self.desktop.iter()) {
                *slot = Some(*w);
            }
            let count = self.desktop.count();
            for w in front_first[..count].iter().rev().flatten() {
                sw.order[sw.n] = w.id;
                sw.hidden[sw.n] = w.hidden;
                sw.n += 1;
            }
            if let Some(focus) = sw.order[..sw.n]
                .iter()
                .position(|&id| Some(id) == self.desktop.focus)
            {
                sw.order[..=focus].rotate_right(1);
                sw.hidden[..=focus].rotate_right(1);
            }
            self.switching = Some(sw);
        }
        let Some(mut sw) = self.switching else {
            return;
        };
        // Windows closed meanwhile are skipped.
        let Some(next) = (1..=sw.n)
            .map(|step| (sw.at + step) % sw.n.max(1))
            .find(|&i| self.desktop.get(sw.order[i]).is_some())
        else {
            self.switching = None;
            return;
        };
        if sw.hidden[sw.at] && next != sw.at {
            self.hide(sw.order[sw.at]);
        }
        sw.at = next;
        self.switching = Some(sw);
        let id = sw.order[next];
        self.desktop.set_hidden(id, false);
        self.set_focus(id);
    }

    /// Minimizes a window: it hears that it lost the focus and that its keys went up,
    /// and the one below takes the focus.
    fn hide(&mut self, id: u32) {
        if self.desktop.focus == Some(id) {
            self.keys.repeat.stop();
            self.release_keys_of_focus();
            self.deliver(id, Event::Focus(false));
        }
        self.desktop.set_hidden(id, true);
    }

    /// A button. While one pressed on a window is held, every button goes to
    /// that window: a program that saw a press sees its release.
    fn on_button(&mut self, code: u16, pressed: bool, at: u64) {
        self.flush_pointer();
        let number = (code - BTN_LEFT) as u8 + 1;
        let bit = 1u8 << (code - BTN_LEFT);
        let (px, py) = self.desktop.pointer;
        if !pressed {
            self.pointer.buttons &= !bit;
            if code == BTN_LEFT {
                self.end_drag(px, py);
            }
            if let Some(id) = self
                .pointer
                .grab
                .filter(|_| self.pointer.grabbed & bit != 0)
            {
                self.pointer.grabbed &= !bit;
                if let Some((x, y)) = self.in_content(id, px, py) {
                    let release = Event::Release {
                        x,
                        y,
                        button: number,
                    };
                    self.deliver_wanted(id, wants::POINTER, release);
                }
                if self.pointer.grabbed == 0 {
                    self.pointer.grab = None;
                    self.pointer.moved = true;
                }
            }
            return;
        }
        self.pointer.buttons |= bit;
        let target = if let Some(id) = self.pointer.grab {
            self.in_content(id, px, py).map(|(x, y)| (id, x, y))
        } else {
            let left = code == BTN_LEFT;
            match self.desktop.hit(px, py) {
                Hit::Window { id, x, y } => Some((id, x, y)),
                Hit::Close { id } if left => {
                    self.ask_close(id, at);
                    None
                }
                Hit::Minimize { id } if left => {
                    self.hide(id);
                    self.pointer.moved = true;
                    self.refocus();
                    None
                }
                Hit::Zoom { id } if left => {
                    self.set_focus(id);
                    self.zoom(id);
                    None
                }
                Hit::TitleBar { id } if left => {
                    self.set_focus(id);
                    self.start_drag(id, (px, py), None);
                    None
                }
                Hit::Edge { id, edges } if left => {
                    self.set_focus(id);
                    self.start_drag(id, (px, py), Some(edges));
                    None
                }
                _ => None,
            }
        };
        if let Some((id, x, y)) = target {
            self.pointer.grab = Some(id);
            self.pointer.grabbed |= bit;
            self.set_focus(id);
            self.deliver(
                id,
                Event::Click {
                    x,
                    y,
                    button: number,
                },
            );
        }
    }

    fn start_drag(&mut self, id: u32, from: (i32, i32), edges: Option<u8>) {
        if let Some(w) = self.desktop.get(id) {
            let start = w.content;
            self.pointer.drag = Some(Drag {
                id,
                from,
                start,
                edges,
            });
        }
    }

    /// A device's releases were lost: nothing is held any more, and the window
    /// that had a key or a button down is told.
    fn after_lost_input(&mut self) {
        if !core::mem::take(&mut self.keys.lost) {
            return;
        }
        self.escape.reset();
        self.release_keys_of_focus();
        let (px, py) = self.desktop.pointer;
        if let Some(id) = self.pointer.grab.take() {
            if let Some((x, y)) = self.in_content(id, px, py) {
                let grabbed = self.pointer.grabbed;
                for button in (0..3).filter(|b| grabbed & (1 << b) != 0) {
                    let release = Event::Release {
                        x,
                        y,
                        button: button + 1,
                    };
                    self.deliver_wanted(id, wants::POINTER, release);
                }
            }
        }
        self.pointer.buttons = 0;
        self.pointer.grabbed = 0;
        self.pointer.drag = None;
        self.pointer.moved = true;
        self.switching = None;
    }

    /// The focused window hears the release of every key it saw go down.
    fn release_keys_of_focus(&mut self) {
        let down = core::mem::take(&mut self.keys.down);
        let Some(id) = self.desktop.focus else {
            return;
        };
        let mods = self.keys.keyboard.modifiers();
        for code in down.into_iter().flatten() {
            let state = KeyState::Released;
            self.deliver_wanted(id, wants::KEY_CODES, Event::KeyCode { code, state, mods });
        }
    }

    /// Repeats the key held, when it is due. A dead key waits for the next
    /// key instead of alternating between nothing and its accent.
    fn repeat_keys(&mut self, now_ms: u64) {
        if self.keys.keyboard.dead_pending() {
            return;
        }
        if let Some(code) = self.keys.repeat.due(now_ms) {
            self.on_key(
                aricode_input::Event {
                    kind: EV_KEY,
                    code,
                    value: 1,
                },
                KeyState::Repeated,
            );
        }
    }

    /// A key, to the focused window: its code if it asked, then its bytes.
    fn on_key(&mut self, ev: aricode_input::Event, state: KeyState) {
        let bytes = self.keys.keyboard.feed(ev);
        let Some(id) = self.desktop.focus else {
            return;
        };
        let mods = self.keys.keyboard.modifiers();
        let code = ev.code;
        let down = &mut self.keys.down;
        let told = match state {
            KeyState::Pressed => {
                // Past the slots, no release will come for it: a window needs
                // only know that a few are down.
                if let Some(slot) = down.iter_mut().find(|d| d.is_none()) {
                    *slot = Some(code);
                }
                true
            }
            KeyState::Repeated => down.contains(&Some(code)),
            // A key pressed for another window is not this one's to release.
            KeyState::Released => match down.iter_mut().find(|d| **d == Some(code)) {
                Some(slot) => {
                    *slot = None;
                    true
                }
                None => false,
            },
        };
        if told {
            self.deliver_wanted(id, wants::KEY_CODES, Event::KeyCode { code, state, mods });
        }
        // Each byte of the key (editing keys are escape sequences).
        for &ch in bytes.as_slice() {
            self.deliver(id, Event::Key(ch));
        }
    }

    fn deliver(&mut self, id: u32, ev: Event) {
        let Some(c) = self.content(id) else { return };
        if let Some((reply_to, _)) = c.pending.take() {
            answer(reply_to, Reply::event(ev));
        } else {
            c.push(ev);
        }
    }

    /// The main endpoint and every session, to receive on all at once (ADR
    /// 0052), starting from a different session each time; room is left
    /// for the input devices' replies. Also returns who owns each member.
    fn endpoints(
        &mut self,
    ) -> (
        [Handle; 1 + MAX_SESSIONS + MAX_INPUTS],
        [usize; 1 + MAX_SESSIONS],
        usize,
    ) {
        let mut eps = [SERVICE; 1 + MAX_SESSIONS + MAX_INPUTS];
        let mut owners = [MAIN; 1 + MAX_SESSIONS];
        let mut n = 1;
        for k in 0..MAX_SESSIONS {
            let i = (self.first + k) % MAX_SESSIONS;
            if let Some(s) = self.sessions[i] {
                eps[n] = s.endpoint;
                owners[n] = i + 1;
                n += 1;
            }
        }
        self.first = (self.first + 1) % MAX_SESSIONS;
        (eps, owners, n)
    }

    /// How long the loop may sleep at `now_ms`: until the next frame when
    /// something waits to be shown, a poll's deadline, the next sweep or
    /// the end of a window of late input held back, at most `SWEEP_MS`.
    fn next_deadline(&self, now_ms: u64) -> u64 {
        let frame = (!self.damage.is_empty()).then_some(self.next_frame_ms);
        let polls = self
            .contents
            .iter()
            .flatten()
            .filter_map(|c| c.pending.map(|(_, d)| d));
        let sweep = self.last_sweep_ms + SWEEP_MS;
        frame
            .into_iter()
            .chain(polls)
            .chain(self.keys.repeat.next_due())
            .chain(self.late.next_due())
            .fold(sweep, u64::min)
            .saturating_sub(now_ms)
            .min(SWEEP_MS)
    }

    /// Counts the events of a round handled at `now` more than `LATE_MS`
    /// after they were received, and records what is due (incident 0058).
    fn account_late(&mut self, received: impl Iterator<Item = u64>, now: u64) {
        let late = received
            .filter(|&at| now.saturating_sub(at) > LATE_MS)
            .count() as u32;
        let mut due = if late > 0 {
            self.late.report_many(0, now, late)
        } else {
            0
        };
        // What a window held back is recorded when it ends.
        if let Some((_, held)) = self.late.drain_due(now) {
            due += held;
        }
        if due > 0 {
            let mut label = [0u8; aricode_faults::LABEL_LEN];
            let label = aricode_faults::label("gui input late", due, &mut label);
            let _ = sys::event_record(0, label);
            sys::print(b"compositor: fault ");
            sys::print(label.as_bytes());
            sys::print(b"\n");
        }
    }

    /// The owner clicked the close box of `id` (ADR 0077): its program is
    /// asked to close it. A program that has not come back from the last
    /// request for `FORCE_CLOSE_MS` is marked as not responding at the
    /// next click, and the click after that ends its session: every window
    /// it has closes, and its next call fails, so it ends when it can
    /// (audit of ADR 0077, M2). A program that reads its events, even one
    /// that asks the owner first, is never closed for it.
    fn ask_close(&mut self, id: u32, now_ms: u64) {
        let Some(c) = self.contents.iter_mut().flatten().find(|c| c.id == id) else {
            return;
        };
        let owner = c.owner;
        if let Some(t) = c.close_waiting {
            if now_ms.saturating_sub(t) < FORCE_CLOSE_MS {
                return; // asked already; the program has yet to answer
            }
            if self.desktop.get(id).is_some_and(|w| w.hung) {
                sys::print(b"compositor: window ");
                num(id as u64);
                sys::print(b" still not responding; ending its program's session\n");
                self.retire(owner - 1);
                return;
            }
            self.desktop.set_hung(id, true);
            sys::print(b"compositor: window ");
            num(id as u64);
            sys::print(b" is not responding; its close box ends its program's session\n");
            return;
        }
        // Said before it is handed over: on another CPU the program may
        // answer, and print, before this line would (0.18 release suite).
        sys::print(b"compositor: window ");
        num(id as u64);
        sys::print(b" asked to close\n");
        c.close_waiting = Some(now_ms);
        if let Some((reply_to, _)) = c.pending.take() {
            answer(reply_to, Reply::event(Event::Close));
        } else {
            c.push(Event::Close);
        }
        self.set_focus(id);
    }

    /// After a window closed: the front one, if any, gets the focus, so
    /// keys reach a window again (audit of ADR 0077, L5).
    fn refocus(&mut self) {
        if self.desktop.focus.is_none() {
            if let Some(front) = self.desktop.front() {
                self.set_focus(front);
            }
        }
    }

    /// Closes the window of `slot`: a poll waiting on it is answered
    /// `BadWindow`, the buffer goes once no window shows it.
    fn close_window(&mut self, slot: usize) {
        let Some(closed) = self.contents[slot].take() else {
            return;
        };
        if let Some((h, _)) = closed.pending {
            answer(h, Reply::status(Status::BadWindow));
        }
        self.unmap_unused(closed.virt);
        self.desktop.remove(closed.id);
        self.refocus();
    }

    fn set_focus(&mut self, id: u32) {
        if self.desktop.focus == Some(id) {
            // It may be below a window that opened without taking the focus.
            self.desktop.raise(id);
            return;
        }
        // The key held was for the window that had the focus.
        self.keys.repeat.stop();
        self.release_keys_of_focus();
        if let Some(old) = self.desktop.focus {
            self.deliver(old, Event::Focus(false));
        }
        self.desktop.raise(id);
        self.deliver(id, Event::Focus(true));
    }

    /// One input event, received at `at` (ms since boot).
    fn on_input(&mut self, ev: aricode_input::Event, at: u64) {
        if ev.kind == EV_REL {
            let (dx, dy) = match ev.code {
                REL_X => (ev.value as i32, 0),
                REL_Y => (0, ev.value as i32),
                REL_WHEEL => {
                    self.flush_pointer();
                    let (px, py) = self.desktop.pointer;
                    let over = self.content_at(px, py);
                    if let Some((id, (x, y))) =
                        over.and_then(|i| Some((i, self.in_content(i, px, py)?)))
                    {
                        let delta = ev.value as i32;
                        self.deliver_wanted(id, wants::POINTER, Event::Wheel { x, y, delta });
                    }
                    return;
                }
                _ => return,
            };
            self.desktop.move_pointer(dx, dy);
            self.pointer.moved = true;
            return;
        }
        if ev.kind == EV_KEY && (BTN_LEFT..=BTN_LEFT + 2).contains(&ev.code) {
            // Pressed, or released; a device's repeat says nothing.
            if ev.value <= 1 {
                self.on_button(ev.code, ev.value == 1, at);
            }
            return;
        }
        // The owner's way back to the text console (ADR 0074): the
        // compositor leaves, and init ends the desktop and gives the console
        // the screen and the keyboards. No window ever sees the chord.
        if self.escape.feed(ev) {
            say(b"Ctrl+Alt+Backspace: the owner asked for the text console; leaving");
            sys::process_exit(0);
        }
        if ev.kind != EV_KEY || !self.keys.repeat.follow(&ev, at) {
            return;
        }
        // Alt+Tab is the desktop's: no window sees it.
        if ev.code == KEY_TAB && ev.value == 1 && self.keys.keyboard.modifiers() & mods::ALT != 0 {
            self.keys.repeat.stop();
            return self.cycle_windows();
        }
        if ev.code == KEY_LEFTALT && ev.value == 0 {
            self.switching = None;
        }
        if ev.value == 1 {
            self.last_key_ms = Some(at);
        }
        let state = if ev.value == 1 {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        self.on_key(ev, state);
    }

    fn handle(
        &mut self,
        req: Request,
        owner: usize,
        reply_to: Handle,
        buf: Option<Handle>,
        now_ms: u64,
    ) {
        // The main endpoint (init, the broker) only makes and ends
        // sessions; windows live in sessions, which can do neither.
        if (owner == MAIN) != matches!(req.op, Op::Attach | Op::Detach) {
            return answer(reply_to, Reply::status(Status::Invalid));
        }
        match req.op {
            Op::Attach => self.attach(reply_to, now_ms),
            Op::Detach => self.detach(reply_to, req.window),
            Op::Create => {
                let Some(buf) = buf else {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                // A DMA buffer, not a device's registers: mapping an `Mmio`
                // object would read the device every frame and move it into
                // the compositor's IOMMU domain (audit 2026-10-25 A, L3).
                if sys::dma_phys(buf).is_err() {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                }
                if req.width == 0
                    || req.height == 0
                    || req.width > MAX_SIDE
                    || req.height > MAX_SIDE
                {
                    return answer(reply_to, Reply::status(Status::Invalid));
                }
                let needed = req.width as u64 * req.height as u64 * 4;
                let Ok(len) = sys::region_len(buf) else {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                if len < needed {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                }
                let Ok(virt) = sys::mmio_map(buf) else {
                    return answer(reply_to, Reply::status(Status::BadHandles));
                };
                let open = self.contents.iter().flatten().filter(|c| c.owner == owner);
                let Some(slot) = self
                    .contents
                    .iter()
                    .position(|c| c.is_none())
                    .filter(|_| open.count() < MAX_WINDOWS_PER_SESSION)
                else {
                    self.unmap_unused(virt);
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                let Some(id) = self.desktop.create(req.width, req.height, req.title_str()) else {
                    self.unmap_unused(virt);
                    return answer(reply_to, Reply::status(Status::Busy));
                };
                self.contents[slot] = Some(Content {
                    id,
                    owner,
                    virt,
                    width: req.width,
                    height: req.height,
                    events: [None; QUEUE],
                    count: 0,
                    pending: None,
                    wants: req.events,
                    dirty: None,
                    place_at: None,
                    close_waiting: None,
                });
                if req.events & wants::RESIZE != 0 {
                    self.desktop.set_resizable(id);
                }
                sys::print(b"compositor: window ");
                num(id as u64);
                sys::print(b" '");
                sys::print(req.title_str().as_bytes());
                sys::print(b"' ");
                num(req.width as u64);
                sys::print(b"x");
                num(req.height as u64);
                sys::print(b"\n");
                answer(reply_to, Reply::ok(id as u64));
                // The program the owner just opened gets the keys typed
                // next, and no other window takes them (audit of phase C,
                // M2).
                if let Some(session) = owner.checked_sub(1).and_then(|i| self.sessions[i].as_mut())
                {
                    let first = !core::mem::replace(&mut session.opened, true);
                    let made = session.made_ms;
                    let typed = self.last_key_ms.is_some_and(|k| k >= made);
                    if self.desktop.focus.is_none()
                        || aricode_gfx::new_window_takes_focus(
                            first,
                            now_ms.saturating_sub(made),
                            typed,
                        )
                    {
                        self.set_focus(id);
                    }
                }
            }
            Op::Present => {
                let Some((virt, width)) = self.owned(req.window, owner).map(|c| (c.virt, c.width))
                else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                // Every window on that buffer shows the new pixels, wherever it
                // is when the frame is made.
                for c in self
                    .contents
                    .iter_mut()
                    .flatten()
                    .filter(|c| c.virt == virt)
                {
                    let whole = Rect::new(0, 0, c.width, c.height);
                    // The rectangles are in the presenter's rows: a window that
                    // shows the same buffer at another width shows them elsewhere.
                    if req.damage_len == 0 || c.width != width {
                        c.dirty = Some(whole);
                        continue;
                    }
                    for d in &req.damage[..req.damage_len as usize] {
                        let part =
                            Rect::new(d.x as i32, d.y as i32, d.width as u32, d.height as u32)
                                .intersect(&whole);
                        c.dirty = Some(match c.dirty {
                            Some(sum) if !part.is_empty() => sum.union(&part),
                            Some(sum) => sum,
                            None => part,
                        });
                    }
                }
                answer(reply_to, Reply::ok(0));
            }
            Op::Resize => self.resize(req, owner, reply_to, buf),
            Op::Poll => {
                let Some(c) = self.owned(req.window, owner) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                let ev = c.pop();
                // Polling again with no request left queued: it came back
                // from the request to close (ADR 0077).
                if c.close_waiting.is_some() && !c.close_queued() {
                    c.close_waiting = None;
                    self.desktop.set_hung(req.window, false);
                }
                let Some(c) = self.owned(req.window, owner) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                if let Some(ev) = ev {
                    return answer(reply_to, Reply::event(ev));
                }
                if c.pending.is_some() {
                    return answer(reply_to, Reply::status(Status::Busy));
                }
                let wait = req.timeout_ms.clamp(1, MAX_POLL_MS);
                c.pending = Some((reply_to, now_ms + wait as u64));
            }
            Op::Close => {
                let Some(slot) = self.contents.iter().position(|c| {
                    c.as_ref()
                        .is_some_and(|c| c.id == req.window && c.owner == owner)
                }) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                self.close_window(slot);
                answer(reply_to, Reply::ok(0));
            }
        }
    }

    /// `Op::Resize`: the same checks as `Create`, and only then the old buffer
    /// goes, so a refused request leaves the window as it was.
    fn resize(&mut self, req: Request, owner: usize, reply_to: Handle, buf: Option<Handle>) {
        if self.owned(req.window, owner).is_none() {
            return answer(reply_to, Reply::status(Status::BadWindow));
        }
        let Some(buf) = buf else {
            return answer(reply_to, Reply::status(Status::BadHandles));
        };
        if sys::dma_phys(buf).is_err() {
            return answer(reply_to, Reply::status(Status::BadHandles));
        }
        if req.width == 0 || req.height == 0 || req.width > MAX_SIDE || req.height > MAX_SIDE {
            return answer(reply_to, Reply::status(Status::Invalid));
        }
        let needed = req.width as u64 * req.height as u64 * 4;
        if sys::region_len(buf).map_or(true, |len| len < needed) {
            return answer(reply_to, Reply::status(Status::BadHandles));
        }
        let Ok(virt) = sys::mmio_map(buf) else {
            return answer(reply_to, Reply::status(Status::BadHandles));
        };
        if let Some(w) = self.desktop.get(req.window) {
            let old = w.frame();
            self.damage.add(&old);
        }
        self.desktop.resize(req.window, req.width, req.height);
        if let Some(w) = self.desktop.get(req.window) {
            let new = w.frame();
            self.damage.add(&new);
        }
        let Some(c) = self.content(req.window) else {
            return answer(reply_to, Reply::status(Status::BadWindow));
        };
        let old = core::mem::replace(&mut c.virt, virt);
        c.width = req.width;
        c.height = req.height;
        c.dirty = None;
        sys::print(b"compositor: window ");
        num(req.window as u64);
        sys::print(b" is now ");
        num(req.width as u64);
        sys::print(b"x");
        num(req.height as u64);
        sys::print(b"\n");
        // The size asked for: it goes to its place. Any other, say an earlier
        // answer, stays where it is until that one comes.
        let place = c.place_at.filter(|p| (p.w, p.h) == (req.width, req.height));
        if place.is_some() {
            c.place_at = None;
        }
        if let Some(p) = place {
            self.desktop.move_window(req.window, p.x, p.y);
        }
        self.unmap_unused(old);
        answer(reply_to, Reply::ok(0));
    }

    /// Unmaps a content buffer no window shows any more. The mapping is
    /// what keeps a client's buffer allocated once its handle is closed
    /// (ADR 0038), so it goes with the last window on it; a client may
    /// have created several windows on one buffer (the kernel then returns
    /// the same mapping), and those keep it.
    fn unmap_unused(&self, virt: u64) {
        if !self.contents.iter().flatten().any(|c| c.virt == virt) {
            let _ = sys::mmio_unmap(virt);
        }
    }

    fn expire(&mut self, now_ms: u64) {
        for c in self.contents.iter_mut().flatten() {
            if c.pending.as_ref().is_some_and(|(_, d)| now_ms >= *d) {
                let (h, _) = c.pending.take().unwrap();
                answer(h, Reply::status(Status::Timeout));
            }
        }
    }

    /// Puts what changed on screen: recomposes and flushes only the damaged
    /// rectangles, at most once per `FRAME_MS` (changes that arrive sooner
    /// wait for the next frame and are merged into it).
    fn compose(&mut self, now_ms: u64) {
        let scene = self.desktop.scene();
        self.shown.damage(&scene, &mut self.damage);
        self.shown = scene;
        for c in self.contents.iter_mut().flatten() {
            let Some(part) = c.dirty.take() else { continue };
            // A hidden window's pixels show when it does, through the scene.
            if let Some(w) = self.desktop.get(c.id).filter(|w| !w.hidden) {
                let at = Rect::new(w.content.x + part.x, w.content.y + part.y, part.w, part.h);
                self.damage.add(&at);
            }
        }
        if self.damage.is_empty() || now_ms < self.next_frame_ms {
            return;
        }
        self.next_frame_ms = now_ms + FRAME_MS;
        self.frames += 1;
        let contents = &self.contents;
        let desktop = &self.desktop;
        let mut screen = self.screen.canvas();
        for r in self.damage.rects() {
            desktop.compose_region(&mut screen, r, |w: &Window, out: &mut Canvas| {
                let Some(c) = contents.iter().flatten().find(|c| c.id == w.id) else {
                    return;
                };
                let n = (c.width * c.height) as usize;
                // SAFETY: client buffer mapped in `Create`, `width*height*4` bytes long.
                let pixels = unsafe { core::slice::from_raw_parts_mut(c.virt as *mut u32, n) };
                if let Some(src) = Canvas::new(pixels, c.width, c.height, c.width) {
                    out.blit(&src, w.content.x, w.content.y);
                }
            });
        }
        self.screen.flip(self.damage.rects());
        if self.trace {
            self.trace_frame();
        }
        self.damage.clear();
        if !self.announced && self.desktop.count() >= 2 {
            self.announced = true;
            say(b"desktop ready with 2 windows");
        }
    }

    /// `gui_trace`: the frame just flushed, and one pixel near the top-left
    /// corner of each window's content (deterministic samples for the tests).
    fn trace_frame(&mut self) {
        sys::print(b"compositor: frame ");
        num(self.frames);
        sys::print(b", ");
        num(self.desktop.count() as u64);
        sys::print(b" window(s), cursor (");
        num(self.desktop.pointer.0 as u64);
        sys::print(b",");
        num(self.desktop.pointer.1 as u64);
        sys::print(b"), flushed ");
        num(self.damage.pixels());
        sys::print(b" px in ");
        num(self.damage.rects().len() as u64);
        sys::print(b" rect(s)\n");
        let mut hex = [0u8; 16];
        let screen = self.screen.canvas();
        for w in self.desktop.iter().filter(|w| !w.hidden) {
            let (sx, sy) = (w.content.x + 10, w.content.y + 10);
            if let Some(px) = screen.get(sx, sy) {
                sys::print(b"compositor: window ");
                num(w.id as u64);
                sys::print(b" sample (");
                num(sx as u64);
                sys::print(b",");
                num(sy as u64);
                sys::print(b") = 0x");
                sys::print(fmt_hex(px as u64, 6, &mut hex));
                sys::print(b"\n");
            }
        }
    }
}

fn main(inputs: u64) -> u64 {
    let screen = match Screen::open() {
        Ok(s) => s,
        Err(step) => {
            say(b"no usable framebuffer");
            return step;
        }
    };
    sys::print(b"compositor: screen ");
    num(screen.info.width as u64);
    sys::print(b"x");
    num(screen.info.height as u64);
    sys::print(b"\n");
    let inputs = (inputs as usize).min(MAX_INPUTS);
    let mut devices: [Option<InputClient>; MAX_INPUTS] = [const { None }; MAX_INPUTS];
    for (i, slot) in devices.iter_mut().take(inputs).enumerate() {
        *slot = InputClient::new(Handle(FIRST_INPUT + i as u32)).ok();
    }
    let mut args = [0u8; sys::CMDLINE_MAX];
    // The keyboard layout of `kbd=`, the one the console uses too: a
    // Spanish keyboard types the same characters in a window as at the
    // console.
    let (trace, layout) = match sys::cmdline(SYSTEM, &mut args) {
        Ok(text) => {
            let line = CmdLine::new(text);
            let layout = line.get(keys::KBD).and_then(Layout::parse);
            (line.has_flag(keys::GUI_TRACE), layout.unwrap_or_default())
        }
        // A line that does not fit or is not text is reported, not read as
        // empty (ADR 0045, L5): the trace stays off.
        Err(_) => {
            say(b"cannot read the command line (too long or not text): gui_trace off, US keyboard");
            (false, Layout::Us)
        }
    };
    let mut keyboard = Keyboard::new();
    keyboard.set_layout(layout);
    if trace {
        say(b"gui_trace: logging every frame");
    }
    let desktop = Desktop::new(screen.info.width, screen.info.height);
    let mut comp = Compositor {
        shown: desktop.scene(),
        damage: Damage::full(desktop.screen),
        desktop,
        screen,
        contents: [const { None }; MAX_WINDOWS],
        keys: Keys {
            keyboard,
            repeat: KeyRepeat::new(),
            down: [None; 8],
            lost: false,
        },
        pointer: Pointer::default(),
        switching: None,
        escape: EscapeChord::new(),
        last_key_ms: None,
        next_frame_ms: 0,
        frames: 0,
        announced: false,
        trace,
        sessions: [None; MAX_SESSIONS],
        generation: 0,
        first: 0,
        last_sweep_ms: 0,
        late: aricode_faults::Meter::new(),
    };
    // Room for a request and for an input device's reply: the wait below
    // receives either.
    let mut msg = [0u8; MSG_LEN];
    let mut handles = [Handle(0); sys::abi::IPC_MAX_HANDLES];
    let mut inputs = Inputs::new(devices);
    say(b"serving requests");
    loop {
        let t = now(SYSTEM);
        for _ in 0..8 {
            let (eps, owners, n_eps) = comp.endpoints();
            let (which, n, nh) =
                match sys::ipc_receive_any(&eps[..n_eps], &mut msg, &mut handles, 0) {
                    Ok(r) => r,
                    // The service endpoint was closed or revoked (ADR 0030):
                    // nobody can send here again, so polling it would spin.
                    Err(e) if e.endpoint_gone() && n_eps == 1 => {
                        say(b"service endpoint closed; exiting");
                        return 9;
                    }
                    Err(_) => break,
                };
            serve(&mut comp, owners[which], &msg[..n], &handles[..nh], t);
        }
        // Clients that went away: their sessions and windows go too.
        if t.saturating_sub(comp.last_sweep_ms) >= SWEEP_MS {
            comp.last_sweep_ms = t;
            comp.reap_gone(t);
        }
        // Every reply already there, in time order (a mouse click must
        // precede the key that came after it).
        inputs.collect(&mut comp.keys, t);
        let handled = inputs.take();
        for (at, ev) in handled {
            comp.on_input(*ev, *at);
        }
        comp.account_late(handled.iter().map(|(at, _)| *at), now(SYSTEM));
        inputs.clear();
        comp.after_lost_input();
        comp.flush_pointer();
        comp.repeat_keys(t);
        comp.expire(t);
        comp.compose(t);
        // Sleep until a request, a device's events or the next deadline,
        // whichever comes first (incident 0058: reading the devices in
        // turn, each with its own wait, left the pointer behind the hand).
        inputs.start_reads(&mut comp.keys, t);
        let t = now(SYSTEM);
        let wait = comp.next_deadline(t).min(inputs.next_retry(t));
        let (mut eps, owners, n_eps) = comp.endpoints();
        let n_all = inputs.add_replies(&mut eps, n_eps);
        match sys::ipc_receive_any(&eps[..n_all], &mut msg, &mut handles, wait) {
            Ok((which, n, nh)) if which < n_eps => {
                serve(
                    &mut comp,
                    owners[which],
                    &msg[..n],
                    &handles[..nh],
                    now(SYSTEM),
                );
            }
            Ok((which, n, nh)) => {
                let at = now(SYSTEM);
                inputs.finish(which - n_eps, &msg[..n], &handles[..nh], &mut comp.keys, at);
            }
            Err(sys::Error::Timeout) => {}
            // A wait that fails at once (a member revoked) would spin the
            // loop: it pauses instead (audit of incident 0058, L2).
            Err(_) => sys::thread_sleep(RETRY_MS),
        }
    }
}

/// Input events held between two rounds: at most one reply from each
/// device, since a device's next read starts only after the round (the
/// reply the wait took, then `collect`, which skips that device).
const HELD: usize = MAX_INPUTS * MAX_EVENTS;

/// The longest message the loop receives: a request, or an input reply.
const MSG_LEN: usize = if Request::LEN > aricode_proto::input::Reply::LEN {
    Request::LEN
} else {
    aricode_proto::input::Reply::LEN
};

/// Slack past `READ_MS` before an unanswered read is given up.
const HUNG_MS: u64 = 5000;

/// Requests a read waits for at most: an idle device answers this often
/// with nothing, and is asked again.
const READ_MS: u32 = 1000;

/// The input devices, each with one read outstanding (ADR 0044), and the
/// events of the replies received and not yet handled.
struct Inputs {
    devices: [Option<InputClient>; MAX_INPUTS],
    /// The reply endpoint of each device's outstanding read.
    pending: [Option<Handle>; MAX_INPUTS],
    /// Which reply each member past the sessions in the wait belongs to.
    waiting: [usize; MAX_INPUTS],
    /// When a device that answered busy, or whose driver died holding the
    /// read, is asked again (ms since boot).
    retry_at: [u64; MAX_INPUTS],
    /// Which run of each driver its replies come from.
    instances: [DriverInstance; MAX_INPUTS],
    /// When each outstanding read was started (ms since boot).
    started: [u64; MAX_INPUTS],
    /// Each with when it was received and its order: the device, then its
    /// place among the events taken (`d * HELD + index`).
    events: [(u64, usize, aricode_input::Event); HELD],
    sorted: [(u64, aricode_input::Event); HELD],
    len: usize,
}

impl Inputs {
    fn new(devices: [Option<InputClient>; MAX_INPUTS]) -> Self {
        Self {
            devices,
            pending: [None; MAX_INPUTS],
            waiting: [0; MAX_INPUTS],
            retry_at: [0; MAX_INPUTS],
            instances: [DriverInstance::new(); MAX_INPUTS],
            started: [0; MAX_INPUTS],
            events: [(0, 0, aricode_input::Event::default()); HELD],
            sorted: [(0, aricode_input::Event::default()); HELD],
            len: 0,
        }
    }

    /// A read outstanding on every device that has none and is not
    /// waiting out a pause. A device whose endpoint nobody receives on any
    /// more is given up (ADR 0030).
    fn start_reads(&mut self, keys: &mut Keys, now_ms: u64) {
        for d in 0..MAX_INPUTS {
            let Some(dev) = &self.devices[d] else {
                continue;
            };
            if self.pending[d].is_some() || now_ms < self.retry_at[d] {
                continue;
            }
            match dev.start_read(READ_MS) {
                Ok(reply) => {
                    self.pending[d] = Some(reply);
                    self.started[d] = now_ms;
                }
                Err(e) if e.service_gone() => {
                    sys::print(b"compositor: input device ");
                    num(d as u64);
                    sys::print(b" is gone; dropping it\n");
                    // Its releases are lost: no modifier stays held.
                    keys.release_held();
                    self.devices[d] = None;
                }
                Err(_) => self.retry_at[d] = now_ms + RETRY_MS,
            }
        }
    }

    /// The outstanding reads' reply endpoints, after the `n` endpoints of
    /// `eps`; returns how many members there are now.
    fn add_replies(
        &mut self,
        eps: &mut [Handle; 1 + MAX_SESSIONS + MAX_INPUTS],
        n: usize,
    ) -> usize {
        let mut n_all = n;
        for (d, p) in self.pending.iter().enumerate() {
            if let Some(reply) = p {
                eps[n_all] = *reply;
                self.waiting[n_all - n] = d;
                n_all += 1;
            }
        }
        n_all
    }

    /// How long until a paused device is asked again, at most `SWEEP_MS`.
    fn next_retry(&self, now_ms: u64) -> u64 {
        (0..MAX_INPUTS)
            .filter(|&d| self.devices[d].is_some() && self.pending[d].is_none())
            .map(|d| self.retry_at[d].saturating_sub(now_ms))
            .min()
            .unwrap_or(SWEEP_MS)
    }

    /// Looks at every outstanding read without waiting: a reply already
    /// there is taken, and a read whose driver died holding it (its reply
    /// endpoint closed, which a wait on several endpoints does not report
    /// while others are open, ADR 0036) is started again after a pause.
    fn collect(&mut self, keys: &mut Keys, now_ms: u64) {
        let mut msg = [0u8; aricode_proto::input::Reply::LEN];
        let mut none = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        for d in 0..MAX_INPUTS {
            let Some(reply) = self.pending[d] else {
                continue;
            };
            match sys::ipc_receive(reply, &mut msg, &mut none, 0) {
                Ok((n, nh)) => self.take_reply(d, &msg[..n], &none[..nh], keys, now_ms),
                Err(sys::Error::PeerClosed) => {
                    let _ = sys::handle_close(reply);
                    self.pending[d] = None;
                    // The driver is relaunched on the same endpoint; the
                    // keys it held were never released.
                    keys.release_held();
                    self.retry_at[d] = now_ms + RETRY_MS;
                }
                // Alive but holding the read far past its time: given up
                // and asked again, said, so a stuck driver does not leave
                // the device dead unseen (audit of incident 0058, L1).
                Err(_) if now_ms.saturating_sub(self.started[d]) > READ_MS as u64 + HUNG_MS => {
                    let _ = sys::handle_close(reply);
                    self.pending[d] = None;
                    keys.release_held();
                    self.retry_at[d] = now_ms + RETRY_MS;
                    sys::print(b"compositor: input device ");
                    num(d as u64);
                    sys::print(b" did not answer its read in time; asking again\n");
                }
                Err(_) => {}
            }
        }
    }

    /// The reply the wait received, from the `i`-th outstanding read.
    fn finish(&mut self, i: usize, msg: &[u8], handles: &[Handle], keys: &mut Keys, now_ms: u64) {
        if let Some(&d) = self.waiting.get(i) {
            self.take_reply(d, msg, handles, keys, now_ms);
        }
    }

    fn take_reply(
        &mut self,
        d: usize,
        msg: &[u8],
        handles: &[Handle],
        keys: &mut Keys,
        now_ms: u64,
    ) {
        for h in handles {
            let _ = sys::handle_close(*h);
        }
        if let Some(reply) = self.pending[d].take() {
            let _ = sys::handle_close(reply);
        }
        // Relaunched since its last reply, outside a read of ours too: the
        // keys it held were never released (audit of the file manager, M2).
        if self.instances[d].changed(msg) {
            keys.release_held();
        }
        let mut events = [aricode_proto::input::Event::default(); MAX_EVENTS];
        let mut times = [0u64; MAX_EVENTS];
        match InputClient::read_reply(msg, &mut events, &mut times) {
            Ok(n) => {
                for (e, t) in events[..n].iter().zip(&times[..n]) {
                    self.events[self.len] = (
                        *t,
                        d * HELD + self.len,
                        aricode_input::Event {
                            kind: e.kind,
                            code: e.code,
                            value: e.value,
                        },
                    );
                    self.len += 1;
                }
            }
            // The read's time ran out with nothing to say: read again at
            // once, as the console bridge does (audit of incident 0058, M1).
            Err(InputError::Status(InputStatus::Timeout)) => {}
            // Another reader holds its one place, or the reply is not one:
            // asked again after a pause.
            Err(_) => self.retry_at[d] = now_ms + RETRY_MS,
        }
    }

    /// The events received so far, in time order, then by device, then as
    /// each device sent them. A driver stamps the events it read together
    /// with one time (ps2_input: a whole round), so the time alone does not
    /// keep a press before its release; neither does an unstable sort,
    /// which the order key makes total.
    fn take(&mut self) -> &[(u64, aricode_input::Event)] {
        let held = &mut self.events[..self.len];
        held.sort_unstable_by_key(|(t, order, _)| (*t, *order));
        for (out, (t, _, ev)) in self.sorted.iter_mut().zip(held.iter()) {
            *out = (*t, *ev);
        }
        &self.sorted[..self.len]
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

/// One request from `owner`: handles `[reply, buffer?]`; any others are
/// closed unread.
fn serve(comp: &mut Compositor, owner: usize, msg: &[u8], handles: &[Handle], t: u64) {
    let Some((&reply_to, rest)) = handles.split_first() else {
        return;
    };
    let buf = rest.first().copied();
    for &extra in rest.iter().skip(1) {
        let _ = sys::handle_close(extra);
    }
    match Request::decode(msg) {
        Some(req) => comp.handle(req, owner, reply_to, buf, t),
        None => answer(reply_to, Reply::status(Status::Invalid)),
    }
    // The content buffer stays mapped (the mapping keeps it allocated until
    // the window closes, ADR 0038); the handle is not needed.
    if let Some(b) = buf {
        let _ = sys::handle_close(b);
    }
}

aricode_rt::entry!(main);
