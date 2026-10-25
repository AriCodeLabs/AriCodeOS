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
use aricode_gfx::{Canvas, Damage, Desktop, Hit, Rect, Scene, Window};
use aricode_input::{Keyboard, BTN_LEFT, EV_KEY, EV_REL, REL_X, REL_Y};
use aricode_proto::window::{Event, Op, Reply, Request, Status, MAX_SIDE};
use aricode_rt::sys::input::InputClient;
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
const MAX_INPUTS: usize = 4;
const MAX_WINDOWS: usize = aricode_gfx::MAX_WINDOWS;
const QUEUE: usize = 8;
/// Client sessions (ADR 0052): one per program the broker launches with
/// `window`.
const MAX_SESSIONS: usize = 8;
/// How often every session is asked whether its client is still there.
const SWEEP_MS: u64 = 1000;
/// What arrives on the main endpoint (init): only `Attach`.
const MAIN: usize = 0;
/// Shortest interval between two frames (about 60 per second): changes
/// that arrive sooner are coalesced into the next frame, so an event waits
/// at most one frame.
const FRAME_MS: u64 = 16;

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
    fb: *mut u32,
    back: &'static mut [u32],
}

impl Screen {
    fn open() -> Result<Self, u64> {
        let mut b = [0u8; abi::sysinfo::Framebuffer::LEN];
        let n = sys::system_info(SYSTEM, abi::sysinfo::FRAMEBUFFER, &mut b).map_err(|_| 10u64)?;
        let info = abi::sysinfo::Framebuffer::decode(&b[..n]).ok_or(11u64)?;
        let fb = sys::mmio_map(FRAMEBUFFER).map_err(|_| 12u64)? as *mut u32;
        let pixels = info.stride as u64 * info.height as u64;
        let dma = sys::dma_alloc(HW, pixels * 4).map_err(|_| 13u64)?;
        let virt = sys::mmio_map(dma).map_err(|_| 14u64)?;
        // SAFETY: our own DMA buffer of `pixels` u32, mapped RW; lives until exit.
        let back = unsafe { core::slice::from_raw_parts_mut(virt as *mut u32, pixels as usize) };
        Ok(Self { info, fb, back })
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
        let rgb = self.info.format == abi::sysinfo::FB_FORMAT_RGB;
        let stride = self.info.stride as usize;
        for r in rects {
            for y in r.y..r.bottom() {
                let row = y as usize * stride;
                for i in row + r.x as usize..row + r.right() as usize {
                    let px = self.back[i];
                    let v = if rgb {
                        (px & 0xFF) << 16 | (px & 0xFF00) | (px >> 16) & 0xFF
                    } else {
                        px
                    };
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
}

impl Content {
    fn push(&mut self, ev: Event) {
        if self.count == QUEUE {
            self.events.copy_within(1..QUEUE, 0);
            self.count -= 1;
        }
        self.events[self.count] = Some(ev);
        self.count += 1;
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

struct Compositor {
    screen: Screen,
    desktop: Desktop,
    contents: [Option<Content>; MAX_WINDOWS],
    keyboard: Keyboard,
    /// What the screen shows, and what changed since (window contents
    /// presented by their clients, plus the diff of the scene).
    shown: Scene,
    damage: Damage,
    next_frame_ms: u64,
    frames: u64,
    announced: bool,
    /// `gui_trace` on the command line: log every frame.
    trace: bool,
    /// Receive sides of the client sessions (ADR 0052).
    sessions: [Option<Handle>; MAX_SESSIONS],
    last_sweep_ms: u64,
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
    /// side, the client gets the send side.
    fn attach(&mut self, reply_to: Handle) {
        let Some(slot) = self.sessions.iter().position(Option::is_none) else {
            return answer(reply_to, Reply::status(Status::Busy));
        };
        let Ok((receive, send)) = sys::endpoint_create_pair() else {
            return answer(reply_to, Reply::status(Status::Busy));
        };
        self.sessions[slot] = Some(receive);
        answer_with(reply_to, Reply::ok(0), send);
    }

    /// The client of session `slot` is gone: its endpoint closes and every
    /// window it made goes with it, so a dead client's window neither keeps
    /// the focus nor fills the table (audit 2026-10-25 C, M5).
    fn retire(&mut self, slot: usize) {
        let Some(ep) = self.sessions[slot].take() else {
            return;
        };
        let _ = sys::handle_close(ep);
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
    }

    fn deliver(&mut self, id: u32, ev: Event) {
        let Some(c) = self.content(id) else { return };
        if let Some((reply_to, _)) = c.pending.take() {
            answer(reply_to, Reply::event(ev));
        } else {
            c.push(ev);
        }
    }

    fn set_focus(&mut self, id: u32) {
        if self.desktop.focus == Some(id) {
            return;
        }
        if let Some(old) = self.desktop.focus {
            self.deliver(old, Event::Focus(false));
        }
        self.desktop.raise(id);
        self.deliver(id, Event::Focus(true));
    }

    fn on_input(&mut self, ev: aricode_input::Event) {
        if ev.kind == EV_REL {
            let (dx, dy) = match ev.code {
                REL_X => (ev.value as i32, 0),
                REL_Y => (0, ev.value as i32),
                _ => return,
            };
            self.desktop.move_pointer(dx, dy);
            return;
        }
        if ev.kind == EV_KEY && (BTN_LEFT..=BTN_LEFT + 2).contains(&ev.code) {
            if ev.value != 1 {
                return;
            }
            let (px, py) = self.desktop.pointer;
            if let Hit::Window { id, x, y } = self.desktop.hit(px, py) {
                self.set_focus(id);
                self.deliver(
                    id,
                    Event::Click {
                        x,
                        y,
                        button: (ev.code - BTN_LEFT) as u8 + 1,
                    },
                );
            }
            return;
        }
        // Each byte of the key (editing keys are escape sequences).
        for &ch in self.keyboard.feed(ev).as_slice() {
            if let Some(id) = self.desktop.focus {
                self.deliver(id, Event::Key(ch));
            }
        }
    }

    fn handle(
        &mut self,
        req: Request,
        owner: usize,
        reply_to: Handle,
        buf: Option<Handle>,
        now_ms: u64,
    ) {
        // The main endpoint (init, the broker) only makes sessions; windows
        // live in sessions, which cannot make more.
        if (owner == MAIN) != (req.op == Op::Attach) {
            return answer(reply_to, Reply::status(Status::Invalid));
        }
        match req.op {
            Op::Attach => self.attach(reply_to),
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
                let Some(slot) = self.contents.iter().position(|c| c.is_none()) else {
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
                });
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
            }
            Op::Present => {
                let Some(virt) = self.owned(req.window, owner).map(|c| c.virt) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                // Every window on that buffer shows the new pixels.
                for c in self.contents.iter().flatten().filter(|c| c.virt == virt) {
                    if let Some(w) = self.desktop.get(c.id) {
                        self.damage.add(&w.content);
                    }
                }
                answer(reply_to, Reply::ok(0));
            }
            Op::Poll => {
                let Some(c) = self.owned(req.window, owner) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                if let Some(ev) = c.pop() {
                    return answer(reply_to, Reply::event(ev));
                }
                if c.pending.is_some() {
                    return answer(reply_to, Reply::status(Status::Busy));
                }
                c.pending = Some((reply_to, now_ms + req.timeout_ms.max(1) as u64));
            }
            Op::Close => {
                let Some(slot) = self.contents.iter().position(|c| {
                    c.as_ref()
                        .is_some_and(|c| c.id == req.window && c.owner == owner)
                }) else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                let Some(closed) = self.contents[slot].take() else {
                    return answer(reply_to, Reply::status(Status::BadWindow));
                };
                if let Some((h, _)) = closed.pending {
                    answer(h, Reply::status(Status::BadWindow));
                }
                self.unmap_unused(closed.virt);
                self.desktop.remove(req.window);
                answer(reply_to, Reply::ok(0));
            }
        }
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
        for w in self.desktop.iter() {
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
    let mut devices: [Option<InputClient>; MAX_INPUTS] = [None, None, None, None];
    for (i, slot) in devices.iter_mut().take(inputs).enumerate() {
        *slot = InputClient::new(Handle(FIRST_INPUT + i as u32)).ok();
    }
    let mut args = [0u8; sys::CMDLINE_MAX];
    let trace = match sys::cmdline(SYSTEM, &mut args) {
        Ok(text) => CmdLine::new(text).has_flag(keys::GUI_TRACE),
        // A line that does not fit or is not text is reported, not read as
        // empty (ADR 0045, L5): the trace stays off.
        Err(_) => {
            say(b"cannot read the command line (too long or not text): gui_trace off");
            false
        }
    };
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
        keyboard: Keyboard::new(),
        next_frame_ms: 0,
        frames: 0,
        announced: false,
        trace,
        sessions: [None; MAX_SESSIONS],
        last_sweep_ms: 0,
    };
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); sys::abi::IPC_MAX_HANDLES];
    let mut events = [aricode_proto::input::Event::default(); aricode_proto::input::MAX_EVENTS];
    let mut times = [0u64; aricode_proto::input::MAX_EVENTS];
    say(b"serving requests");
    loop {
        let t = now(SYSTEM);
        for _ in 0..8 {
            // The main endpoint and every session at once (ADR 0052).
            let mut eps = [SERVICE; 1 + MAX_SESSIONS];
            let mut owners = [MAIN; 1 + MAX_SESSIONS];
            let mut n_eps = 1;
            for (i, s) in comp.sessions.iter().enumerate() {
                if let Some(ep) = s {
                    eps[n_eps] = *ep;
                    owners[n_eps] = i + 1;
                    n_eps += 1;
                }
            }
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
            for slot in 0..MAX_SESSIONS {
                let Some(ep) = comp.sessions[slot] else {
                    continue;
                };
                match sys::ipc_receive(ep, &mut msg, &mut handles, 0) {
                    Ok((n, nh)) => serve(&mut comp, slot + 1, &msg[..n], &handles[..nh], t),
                    Err(sys::Error::PeerClosed) => comp.retire(slot),
                    Err(_) => {}
                }
            }
        }
        // Read every device and process their events in time order
        // (a mouse click must precede the key that came after it).
        let mut merged: [(u64, usize, aricode_input::Event);
            MAX_INPUTS * aricode_proto::input::MAX_EVENTS] =
            [(0, 0, aricode_input::Event::default());
                MAX_INPUTS * aricode_proto::input::MAX_EVENTS];
        let mut merged_len = 0;
        for (d, slot) in devices.iter_mut().enumerate() {
            let Some(dev) = slot else { continue };
            let read = dev.read(1, &mut events, &mut times);
            match read {
                // Given up for good (ADR 0030): it would answer at once
                // forever and the 1 ms read would no longer pace the loop.
                Err(e) if e.service_gone() => {
                    sys::print(b"compositor: input device ");
                    let mut b = [0u8; 20];
                    sys::print(aricode_rt::fmt_u64(d as u64, &mut b));
                    sys::print(b" is gone; dropping it\n");
                    *slot = None;
                }
                // The driver died holding the read: it is relaunched on the
                // same endpoint; the next read waits for it.
                Err(e) if e.reply_lost() => sys::thread_sleep(RETRY_MS),
                _ => {}
            }
            if let Ok(n) = read {
                for (e, t) in events[..n].iter().zip(&times[..n]) {
                    merged[merged_len] = (
                        *t,
                        d,
                        aricode_input::Event {
                            kind: e.kind,
                            code: e.code,
                            value: e.value,
                        },
                    );
                    merged_len += 1;
                }
            }
        }
        merged[..merged_len].sort_unstable_by_key(|(t, d, _)| (*t, *d));
        for (_, _, ev) in &merged[..merged_len] {
            comp.on_input(*ev);
        }
        comp.expire(t);
        comp.compose(t);
        if devices.iter().all(Option::is_none) {
            sys::thread_sleep(10);
        }
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
