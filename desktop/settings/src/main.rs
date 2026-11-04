// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's settings (ADR 0074 phase C). It shows the system (memory,
//! processors, uptime, keyboard layout), the network address and the
//! update sources, and chooses what the machine boots into: the text
//! console or the desktop. The system's figures come from `SYSTEM`; the
//! rest is asked of init over the `settings` endpoint, which reads and
//! writes for it (the owner's choice: never a file in its hands) and logs
//! a change in the install history. Enter, or a click on the chosen row,
//! applies it.
//!
//! Handles, in the broker's order: `SYSTEM` (read), its compositor session
//! and the settings endpoint (both endpoints), and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_cmdline::{keys, CmdLine};
use aricode_gfx::widgets::{self, Button, Key, KeyDecoder, List, PAD, ROW_H};
use aricode_gfx::Rect;
use aricode_proto::settings::{Item, Op, Reply, Request, Status};
use aricode_rt::sys::window::{Event, Window};
use aricode_rt::sys::{self, abi, kind, permissions, rpc, Handle};

const W: u32 = 720;
const H: u32 = 400;
const LINE: i32 = widgets::LINE_H as i32 + 4;
/// The lines of facts at the top: system, keyboard, network, updates,
/// session, driver keys; each gets two rows, so a long one (a reason the
/// desktop will not start) wraps instead of being clipped (audit of the
/// settings, M1).
const FACTS: usize = 6;
const COLS: usize = (W as usize - 2 * PAD as usize) / 8;
const CHOICE_Y: i32 = PAD + (2 * FACTS as i32 + 2) * LINE;
const CHOICE: Rect = Rect::new(PAD, CHOICE_Y, 240, 2 * ROW_H);
const REFRESH: Button = Button::at(PAD + 256, CHOICE_Y, "Refresh");
const STATUS_Y: i32 = CHOICE_Y + 2 * ROW_H as i32 + PAD;
const CHOICES: [&str; 2] = ["Text console", "Desktop"];

fn say(parts: &[&[u8]]) {
    sys::print(b"settings: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

/// One line of text the window shows.
#[derive(Clone, Copy)]
struct Line {
    buf: [u8; 128],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            buf: [0; 128],
            len: 0,
        }
    }

    fn set(&mut self, parts: &[&[u8]]) {
        self.len = 0;
        for p in parts {
            let take = p.len().min(self.buf.len() - self.len);
            self.buf[self.len..self.len + take].copy_from_slice(&p[..take]);
            self.len += take;
        }
    }

    fn text(&self) -> &str {
        widgets::utf8_prefix(&self.buf[..self.len])
    }
}

struct Settings {
    system: Handle,
    settings: Handle,
    facts: [Line; FACTS],
    status: Line,
    error: bool,
    choice: List,
}

impl Settings {
    /// Asks init for `item` (or a change); the reply's text, or why there
    /// is none.
    fn ask(&self, op: Op, item: Item, value: u8) -> (bool, Line) {
        let mut line = Line::new();
        let req = Request { op, item, value };
        let mut msg = [0u8; Reply::LEN];
        // Init's own file requests may take long: a change it made late is
        // not reported as one it never made (audit of the settings, L3).
        match rpc::call(self.settings, &req.encode(), &[], 2000, 15_000, &mut msg)
            .ok()
            .and_then(|n| Reply::decode(&msg[..n]))
        {
            Some(r) => {
                line.set(&[r.text()]);
                (r.status == Status::Ok, line)
            }
            None if op == Op::SetSession => {
                line.set(&[b"init did not answer: whether it changed is unknown; Refresh"]);
                (false, line)
            }
            None => {
                line.set(&[b"init did not answer"]);
                (false, line)
            }
        }
    }

    fn info(&self, kind: u64, out: &mut [u8]) -> Option<usize> {
        sys::system_info(self.system, kind, out).ok()
    }

    /// Reads every fact again.
    fn refresh(&mut self) {
        let mut b = [0u8; 20];
        let mut c = [0u8; 20];
        let mut d = [0u8; 20];
        let mut mem = [0u8; 16];
        let mut cpus = [0u8; 8];
        let mut up = [0u8; 8];
        let ram = self
            .info(abi::sysinfo::MEMORY, &mut mem)
            .filter(|&n| n == 16);
        let cpu = self.info(abi::sysinfo::CPUS, &mut cpus).filter(|&n| n == 8);
        let upt = self.info(abi::sysinfo::UPTIME, &mut up).filter(|&n| n == 8);
        match (ram, cpu, upt) {
            (Some(_), Some(_), Some(_)) => {
                let frames = u64::from_le_bytes(mem[..8].try_into().unwrap_or([0; 8]));
                let free = u64::from_le_bytes(mem[8..].try_into().unwrap_or([0; 8]));
                self.facts[0].set(&[
                    b"System: ",
                    aricode_rt::fmt_u64(free * 4 / 1024, &mut b),
                    b" of ",
                    aricode_rt::fmt_u64(frames * 4 / 1024, &mut c),
                    b" MiB free, ",
                    aricode_rt::fmt_u64(u64::from_le_bytes(cpus), &mut d),
                    b" processor(s), up ",
                    aricode_rt::fmt_u64(u64::from_le_bytes(up) / 1000, &mut [0u8; 20]),
                    b" s",
                ]);
            }
            _ => self.facts[0].set(&[b"System: the kernel does not say"]),
        }
        let mut args = [0u8; sys::CMDLINE_MAX];
        let layout = sys::cmdline(self.system, &mut args)
            .ok()
            .and_then(|t| CmdLine::new(t).get(keys::KBD))
            .unwrap_or("us");
        // A layout name is short ASCII; anything else is not shown.
        let layout = if layout.len() <= 8 && layout.bytes().all(|b| b.is_ascii_alphanumeric()) {
            layout
        } else {
            "?"
        };
        let mut kbd = [0u8; 8];
        let n = layout.len();
        kbd[..n].copy_from_slice(layout.as_bytes());
        self.facts[1].set(&[
            b"Keyboard: ",
            &kbd[..n],
            b" (kbd= on the boot command line)",
        ]);
        let (_, address) = self.ask(Op::Get, Item::Address, 0);
        self.facts[2].set(&[b"Network: ", address.text().as_bytes()]);
        let (_, sources) = self.ask(Op::Get, Item::UpdateSource, 0);
        self.facts[3].set(&[b"Updates: ", sources.text().as_bytes()]);
        let (known, session) = self.ask(Op::Get, Item::Session, 0);
        self.facts[4].set(&[b"Boots into: ", session.text().as_bytes()]);
        // Nothing is chosen for the owner when the session is not known:
        // the next Enter would write a guess (audit of the settings, L4).
        if known {
            self.choice
                .select(if session.text().starts_with("desktop") {
                    1
                } else {
                    0
                });
        } else {
            self.choice.selected = None;
        }
        self.facts[5].set(&[b"Driver keys: none (signed driver packages are not built yet)"]);
        for f in &self.facts {
            say(&[f.text().as_bytes()]);
        }
    }

    /// Chooses the selected session from the next boot.
    fn apply(&mut self) {
        let Some(choice) = self.choice.selected else {
            return;
        };
        let (ok, text) = self.ask(Op::SetSession, Item::Session, choice as u8);
        self.status.set(&[text.text().as_bytes()]);
        self.error = !ok;
        say(&[
            b"chose ",
            CHOICES[choice].as_bytes(),
            b": ",
            text.text().as_bytes(),
        ]);
        if ok {
            let (_, session) = self.ask(Op::Get, Item::Session, 0);
            self.facts[4].set(&[b"Boots into: ", session.text().as_bytes()]);
        }
    }

    fn draw(&self, win: &mut Window) {
        let mut c = win.canvas();
        c.fill(widgets::BG);
        c.text(PAD, PAD, "Settings", widgets::FG, None);
        for (i, f) in self.facts.iter().enumerate() {
            let y = PAD + (2 * i as i32 + 1) * LINE + 4;
            let (first, rest) = wrap(f.text(), COLS);
            c.text(PAD, y, first, widgets::FG, None);
            c.text(
                PAD + 16,
                y + LINE,
                wrap(rest, COLS - 2).0,
                widgets::FG,
                None,
            );
        }
        c.text(
            PAD,
            CHOICE_Y - LINE,
            "Boot into (Enter applies):",
            widgets::MUTED,
            None,
        );
        self.choice.draw(&mut c, |i| CHOICES[i]);
        REFRESH.draw(&mut c, false);
        let color = if self.error {
            widgets::ERROR
        } else {
            widgets::MUTED
        };
        c.text(PAD, STATUS_Y, self.status.text(), color, None);
    }
}

/// `text` split where a row of `cols` ends, at the last space before it:
/// the row and the rest.
fn wrap(text: &str, cols: usize) -> (&str, &str) {
    if text.len() <= cols || !text.is_char_boundary(cols) {
        return (text, "");
    }
    let cut = text[..cols].rfind(' ').filter(|&i| i > 0).unwrap_or(cols);
    (&text[..cut], text[cut..].trim_start())
}

fn main(_arg: u64) -> u64 {
    let mut endpoints = [None; 2];
    let mut found = 0;
    let mut system = None;
    let mut hw = None;
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) && found < endpoints.len() {
            endpoints[found] = Some(h);
            found += 1;
        } else if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let ([Some(session), Some(settings)], Some(system), Some(hw)) = (endpoints, system, hw) else {
        say(&[b"started without a window, the settings endpoint, SYSTEM or HARDWARE"]);
        return 1;
    };
    let Ok(mut win) = Window::create(session, hw, W, H, "Settings") else {
        say(&[b"could not create its window"]);
        return 2;
    };
    let mut s = Settings {
        system,
        settings,
        facts: [Line::new(); FACTS],
        status: Line::new(),
        error: false,
        choice: List::new(CHOICE),
    };
    s.choice.set_count(CHOICES.len());
    s.refresh();
    s.draw(&mut win);
    if win.present().is_err() {
        return 3;
    }
    say(&[b"ready"]);
    let mut keys = KeyDecoder::new();
    loop {
        let event = match win.poll(1000) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            // The compositor is gone: so is the desktop.
            Err(_) => return 4,
        };
        match event {
            Event::Key(b) => {
                for key in keys.feed(b).into_iter().flatten() {
                    if key == Key::Enter {
                        s.apply();
                    } else {
                        s.choice.key(key);
                    }
                }
            }
            Event::Click { x, y, .. } => {
                if let Some(row) = s.choice.hit(x, y) {
                    if s.choice.selected == Some(row) {
                        s.apply();
                    } else {
                        s.choice.select(row);
                    }
                } else if REFRESH.contains(x, y) {
                    s.refresh();
                }
            }
            Event::Focus(_) => {}
        }
        s.draw(&mut win);
        let _ = win.present();
    }
}

aricode_rt::entry!(main);
