// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test graphical application (Phase 26), a test package since ADR 0074:
//! paints its window and listens for events. `arg` = 0 (Demo A) or 1 (Demo
//! B, created 300 ms later so it ends up on top), or 2 (Demo C, which never
//! reads its events: the compositor closes its window for it at the second
//! click on its close box, ADR 0077), or 3 (Demo D, the window protocol's
//! events, `events` below), or 4 and 5 (Demo E, frames on two buffers or on one,
//! `frames` below, incident 0063), or 6 (Demo F, a window that may be resized and
//! never takes a size, `stubborn` below, ADR 0085). The broker grants what
//! its manifest asks for: its compositor session (WRITE, ADR 0052) and
//! `HARDWARE` for the window's buffer. Demo B also checks that Demo A's
//! window is out of its reach and leaves without closing its own: the
//! compositor closes it when the session goes (gui.two_windows).
#![no_std]
#![no_main]

use aricode_gfx::{rgb, Canvas, Rect};
use aricode_proto::window::{Op, Reply, Request, Status};
use aricode_rt::sys::window::{wants, DamageRect, Event, KeyState, Window, WindowError};
use aricode_rt::sys::{self, kind, permissions, rpc, Handle};

fn num_i32(v: i32) {
    let mut b = [0u8; 20];
    if v < 0 {
        sys::print(b"-");
    }
    sys::print(aricode_rt::fmt_u64(v.unsigned_abs() as u64, &mut b));
}

/// Sends `op` on window `id` through our session and returns the status.
fn ask(session: Handle, op: Op, id: u32) -> Option<Status> {
    let mut req = Request::new(op);
    req.window = id;
    let mut msg = [0u8; Reply::LEN];
    let len = rpc::call(
        session,
        &req.encode(),
        &[],
        rpc::GRACE_MS,
        rpc::GRACE_MS,
        &mut msg,
    )
    .ok()?;
    Reply::decode(&msg[..len]).map(|r| r.status)
}

/// Mode 3, window protocol checks (ADR 0078, stage 0): a window that asks for
/// every event says each kind the first time it comes. `r` resizes it, `d`
/// presents part of it, and `q` ends it.
fn events(session: Handle, hw: Handle) -> u64 {
    let wanted = wants::POINTER | wants::KEY_CODES | wants::RESIZE;
    let Ok(mut win) = Window::create_wanting(session, hw, 400, 300, "Demo D", wanted) else {
        return 1;
    };
    win.canvas().fill(rgb(0x60, 0x40, 0xa0));
    if win.present().is_err() {
        return 2;
    }
    sys::print(b"gui_demo D: window presented\n");
    let say = |seen: &mut u32, bit: u32, text: &[u8]| {
        if *seen & bit == 0 {
            *seen |= bit;
            sys::print(b"gui_demo D: ");
            sys::print(text);
            sys::print(b"\n");
        }
    };
    let mut seen = 0;
    let mut adding = false;
    let mut repeats = 0;
    let mut leaves = 0;
    for _ in 0..600 {
        let ev = match win.poll(100) {
            Ok(Some(ev)) => ev,
            Ok(None) => {
                // While it is on, a row more each time, as a hostile program would add
                // under the pointer: refused with a menu open (audit of ADR 0078).
                if adding {
                    match win.menu_add(0, "Later", 9) {
                        Ok(()) => {}
                        Err(WindowError::Status(Status::Busy)) => {
                            adding = false;
                            sys::print(b"gui_demo D: a menu stays as it is while open\n");
                        }
                        Err(_) => {
                            adding = false;
                            sys::print(b"gui_demo D: the row was refused otherwise\n");
                        }
                    }
                }
                continue;
            }
            Err(_) => return 3,
        };
        match ev {
            Event::Key(b'm') => {
                adding = win.menu_add(0, "Demo", 0).is_ok();
                sys::print(b"gui_demo D: menu given\n");
            }
            Event::Focus(true) => sys::print(b"gui_demo D: focus gained\n"),
            Event::Focus(false) => sys::print(b"gui_demo D: focus lost\n"),
            Event::Enter { .. } => say(&mut seen, 1, b"enter"),
            Event::Leave => {
                leaves += 1;
                say(
                    &mut seen,
                    if leaves == 1 { 2 } else { 2048 },
                    if leaves == 1 { b"leave 1" } else { b"leave 2" },
                );
            }
            Event::Motion { buttons: 0, .. } => say(&mut seen, 4, b"motion"),
            Event::Motion { buttons: 1, x, .. } if x < 0 => {
                say(&mut seen, 1024, b"motion outside it, left held");
            }
            Event::Release { button: 1, .. } => say(&mut seen, 16, b"release of the left button"),
            Event::Wheel { delta, .. } if delta != 0 => say(&mut seen, 32, b"wheel"),
            Event::KeyCode {
                state: KeyState::Released,
                code: 30,
                ..
            } => say(&mut seen, 64, b"key code released"),
            Event::KeyCode {
                state: KeyState::Pressed,
                code: 30,
                ..
            } => say(&mut seen, 128, b"key code pressed"),
            Event::KeyCode {
                state: KeyState::Repeated,
                ..
            } => {
                repeats += 1;
                say(&mut seen, 256, b"key code repeated");
            }
            Event::KeyCode {
                state: KeyState::Pressed,
                code: 48,
                mods,
            } if mods & aricode_rt::sys::window::mods::SHIFT != 0 => {
                say(&mut seen, 512, b"key code with shift");
            }
            Event::Configure { width, height } => {
                let paint = |c: &mut Canvas| c.fill(rgb(0x20, 0x80, 0x80));
                if win.resize(hw, width, height, paint).is_err() {
                    return 4;
                }
                let drawn = win.present().is_ok() && win.size() == (width, height);
                sys::print(if drawn {
                    b"gui_demo D: configured\n" as &[u8]
                } else {
                    b"gui_demo D: the resize failed\n"
                });
            }
            Event::Key(b'r') => {
                let paint = |c: &mut Canvas| c.fill(rgb(0x20, 0x80, 0x80));
                if win.resize(hw, 500, 350, paint).is_err() {
                    return 4;
                }
                let drawn = win.present().is_ok() && win.size() == (500, 350);
                sys::print(if drawn {
                    b"gui_demo D: resized to 500x350\n" as &[u8]
                } else {
                    b"gui_demo D: the resize failed\n"
                });
            }
            Event::Key(b'd') => {
                let part = DamageRect {
                    x: 10,
                    y: 10,
                    width: 20,
                    height: 20,
                };
                if win.present_rects(&[part]).is_err() {
                    return 5;
                }
                sys::print(b"gui_demo D: presented a part\n");
            }
            // The clipboard (ADR 0078): copying with the focus works, and reading
            // only as the answer to the owner's Ctrl+V, once.
            Event::Key(b'c') => {
                let copied = win.clipboard_set(hw, "demo text").is_ok();
                sys::print(if copied {
                    b"gui_demo D: copied\n" as &[u8]
                } else {
                    b"gui_demo D: copying was refused\n"
                });
                let read = win.clipboard_get(hw, |_| ());
                sys::print(if read.is_err() {
                    b"gui_demo D: the clipboard stays closed without Ctrl+V\n" as &[u8]
                } else {
                    b"gui_demo D: the clipboard was open without Ctrl+V\n"
                });
            }
            Event::Key(0x16) => {
                let same = win.clipboard_get(hw, |text| text == "demo text");
                sys::print(if same == Ok(true) {
                    b"gui_demo D: pasted what was copied\n" as &[u8]
                } else {
                    b"gui_demo D: the paste failed\n"
                });
                sys::print(if win.clipboard_get(hw, |_| ()).is_err() {
                    b"gui_demo D: one read for one press\n" as &[u8]
                } else {
                    b"gui_demo D: a second read for one press\n"
                });
            }
            // Only the launcher offers applications in the dock (ADR 0078).
            Event::Key(b'k') => {
                sys::print(match win.dock_app(0, "gui_demo") {
                    Err(WindowError::Status(Status::Denied)) => {
                        b"gui_demo D: the dock refused its application\n" as &[u8]
                    }
                    _ => b"gui_demo D: the dock took its application\n",
                });
            }
            Event::Key(b'q') => {
                sys::print(if repeats > 1 {
                    b"gui_demo D: the key repeated\n" as &[u8]
                } else {
                    b"gui_demo D: the key did not repeat\n"
                });
                sys::print(b"gui_demo D: done\n");
                return 0;
            }
            _ => {}
        }
    }
    0
}

/// Modes 4 and 5, incident 0063: frames of one colour each, and right after each
/// present the buffer just given up is painted `FREED`, as a program starting its
/// next frame paints over the last. The compositor makes at most one frame each
/// `FRAME_MS`: the first present of a pair gets one at once, the second, coming
/// sooner, is left for later, and that later frame is made while the program is
/// painting `FREED` and pausing, as a game or a benchmark faster than the screen
/// is. With two buffers (mode 4) the compositor never reads the buffer not shown,
/// so `FREED` never reaches the screen; with one (mode 5, the old way) it does.
/// Mode 4 then resizes: the window gets its second buffer again, of the new size,
/// and its frames still never show `FREED`.
fn frames(session: Handle, hw: Handle, double: bool) -> u64 {
    const FRAMES: [u32; 2] = [0xfe0102, 0x0102fe];
    const FREED: u32 = 0x00fe01;
    let Ok(mut win) = Window::create(session, hw, 200, 120, "Demo E") else {
        return 1;
    };
    if double && win.double_buffer(hw).is_err() {
        sys::print(b"gui_demo E: no second buffer\n");
        return 2;
    }
    let pairs = |win: &mut Window, count: usize| {
        for _ in 0..count {
            for colour in FRAMES {
                win.canvas().fill(colour);
                if win.present().is_err() {
                    return false;
                }
            }
            win.canvas().fill(FREED);
            sys::thread_sleep(30);
        }
        true
    };
    if !pairs(&mut win, 60) {
        return 3;
    }
    sys::print(b"gui_demo E: 120 frames presented\n");
    if double {
        if win.resize(hw, 240, 140, |c| c.fill(FRAMES[0])).is_err() {
            return 4;
        }
        if !win.is_double() {
            sys::print(b"gui_demo E: one buffer after resizing\n");
            return 5;
        }
        if !pairs(&mut win, 10) {
            return 6;
        }
        sys::print(b"gui_demo E: two buffers after resizing, 20 frames presented\n");
    }
    let _ = win.close();
    0
}

/// Requests for a size a stroke of the pointer may bring a window that never
/// answers: the compositor asks again only once each 250 ms (ADR 0085), so a drag
/// of under a second asks a handful of times, and one ask per move is a storm.
const FEW_ASKS: u32 = 10;

/// Mode 6, Demo F (ADR 0085): a window that may be resized and never answers. It
/// reads its events, so it is not hung, but takes no size it is asked for, and
/// counts the requests. `q` ends it and says how many came.
fn stubborn(session: Handle, hw: Handle) -> u64 {
    let Ok(mut win) = Window::create_wanting(session, hw, 400, 300, "Demo F", wants::RESIZE) else {
        return 1;
    };
    win.canvas().fill(rgb(0xa0, 0x60, 0x20));
    if win.present().is_err() {
        return 2;
    }
    sys::print(b"gui_demo F: window presented\n");
    let mut asked = 0u32;
    for _ in 0..600 {
        match win.poll(100) {
            Ok(Some(Event::Configure { .. })) => {
                asked += 1;
                if asked == 1 {
                    sys::print(b"gui_demo F: asked for a size, not answering\n");
                }
            }
            Ok(Some(Event::Key(b'q') | Event::Close)) => {
                sys::print(b"gui_demo F: asked ");
                num_i32(asked as i32);
                if asked > FEW_ASKS {
                    sys::print(b" times: a storm\n");
                    return 7;
                }
                sys::print(b" times: a few\n");
                return 0;
            }
            Ok(_) => {}
            Err(_) => return 3,
        }
    }
    0
}

fn main(index: u64) -> u64 {
    let (mut session, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            session = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let (Some(session), Some(hw)) = (session, hw) else {
        sys::print(b"gui_demo: started without a compositor session\n");
        return 6;
    };
    if index == 3 {
        return events(session, hw);
    }
    if index == 4 || index == 5 {
        return frames(session, hw, index == 4);
    }
    if index == 6 {
        return stubborn(session, hw);
    }
    let (name, title, color) = if index == 0 {
        (b"gui_demo A" as &[u8], "Demo A", rgb(0xc0, 0x40, 0x40))
    } else if index == 2 {
        (b"gui_demo C" as &[u8], "Demo C", rgb(0x40, 0xa0, 0x40))
    } else {
        sys::thread_sleep(300);
        (b"gui_demo B" as &[u8], "Demo B", rgb(0x40, 0x80, 0xc0))
    };
    let mut win = match Window::create(session, hw, 400, 300, title) {
        Ok(w) => w,
        Err(e) => {
            sys::print(name);
            sys::print(b": could not create the window: ");
            match e {
                WindowError::Ipc(err) => aricode_rt::print_error(b"ipc", err),
                WindowError::Status(s) => {
                    let mut b = [0u8; 20];
                    sys::print(b"status ");
                    sys::print(aricode_rt::fmt_u64(s as u64, &mut b));
                    sys::print(b"\n");
                }
                WindowError::Invalid => sys::print(b"invalid parameters\n"),
            }
            return 1;
        }
    };
    {
        let mut c = win.canvas();
        c.fill(color);
        c.fill_rect(&Rect::new(0, 0, 400, 2), rgb(255, 255, 255));
        c.fill_rect(&Rect::new(0, 298, 400, 2), rgb(255, 255, 255));
        c.fill_rect(&Rect::new(0, 0, 2, 300), rgb(255, 255, 255));
        c.fill_rect(&Rect::new(398, 0, 2, 300), rgb(255, 255, 255));
        c.text(20, 20, "Window  ", rgb(255, 255, 255), None);
        c.text(20 + 8 * 8, 20, title, rgb(255, 255, 0), None);
        c.text(20, 60, "AriCodeOS GUI", rgb(255, 255, 255), None);
    }
    if win.present().is_err() {
        return 2;
    }
    sys::print(name);
    sys::print(b": window presented\n");
    if index == 2 {
        // Hung, as far as the compositor can tell: no poll for 12 s.
        sys::thread_sleep(12_000);
        sys::print(match win.poll(10) {
            Err(_) => b"gui_demo C: its window was closed for it\n" as &[u8],
            Ok(_) => b"gui_demo C: its window is still open\n",
        });
        return 0;
    }
    let mut got_key = false;
    let mut idle = 0;
    while !got_key && idle < 10 {
        match win.poll(300) {
            Ok(Some(Event::Key(ch))) => {
                sys::print(name);
                sys::print(b": key '");
                sys::print(&[ch]);
                sys::print(b"'\n");
                got_key = true;
            }
            Ok(Some(Event::Click { x, y, button })) => {
                sys::print(name);
                sys::print(b": click button ");
                num_i32(button as i32);
                sys::print(b" at (");
                num_i32(x);
                sys::print(b",");
                num_i32(y);
                sys::print(b")\n");
                idle = 0;
            }
            Ok(Some(Event::Close)) => {
                sys::print(name);
                sys::print(b": asked to close; closing\n");
                if win.close().is_err() {
                    return 5;
                }
                sys::print(name);
                sys::print(b": window closed\n");
                return 0;
            }
            Ok(Some(Event::Focus(f))) => {
                sys::print(name);
                sys::print(if f {
                    b": focus gained\n" as &[u8]
                } else {
                    b": focus lost\n"
                });
                idle = 0;
            }
            // None of these without asking: it asked for nothing.
            Ok(Some(
                Event::Motion { .. }
                | Event::Release { .. }
                | Event::Enter { .. }
                | Event::Leave
                | Event::Wheel { .. }
                | Event::KeyCode { .. },
            )) => {
                sys::print(name);
                sys::print(b": an event it did not ask for\n");
            }
            Ok(Some(_)) => {}
            Ok(None) => idle += 1,
            Err(_) => return 3,
        }
    }
    if !got_key {
        sys::print(name);
        sys::print(b": no keys\n");
    }
    if index == 1 {
        let foreign = (1..=2).find(|&id| id != win.id()).unwrap_or(1);
        for op in [Op::Poll, Op::Present, Op::Close] {
            if ask(session, op, foreign) != Some(Status::BadWindow) {
                sys::print(b"gui_demo B: reached a window that is not its own\n");
                return 4;
            }
        }
        // Sessions cannot make or end sessions.
        if ask(session, Op::Attach, 0) != Some(Status::Invalid)
            || ask(session, Op::Detach, foreign) != Some(Status::Invalid)
        {
            sys::print(b"gui_demo B: its session made or ended a session\n");
            return 4;
        }
        sys::print(b"gui_demo B: the other window is out of reach\n");
        sys::print(b"gui_demo B: leaving without closing its window\n");
        return 0;
    }
    if win.close().is_err() {
        return 5;
    }
    sys::print(name);
    sys::print(b": window closed\n");
    0
}

aricode_rt::entry!(main);
