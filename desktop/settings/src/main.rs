// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's settings (ADR 0074 phase C). It shows the system (memory,
//! processors, uptime, keyboard layout), the network address and the
//! update sources, and chooses what the machine boots into: the text
//! console or the desktop. The system's figures come from `SYSTEM`; the
//! rest is asked of init over the `settings` endpoint, which reads and
//! writes for it (the owner's choice: never a file in its hands) and logs
//! a change in the install history. Enter, or a click on the chosen row,
//! applies it. Built on the toolkit (ADR 0078, stage 2).
//!
//! Handles, in the broker's order: `SYSTEM` (read), its compositor session
//! and the settings endpoint (both endpoints), and `HARDWARE`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_cmdline::{keys, CmdLine};
use aricode_proto::settings::{Item, Op, Reply, Request, Status};
use aricode_rt::sys::window::{wants, Event, Window};
use aricode_rt::sys::{self, abi, kind, permissions, rpc, Handle};
use aricode_ui::{Action, Align, Fill, Id, Space, Stack, Style, Theme, Tone, Ui};

const W: u32 = 720;
const H: u32 = 400;
/// The lines of facts: system, keyboard, network, updates, session, driver keys.
const FACTS: usize = 6;
const CHOICES: [&str; 2] = ["Text console", "Desktop"];
const LOOKS: [&str; 2] = ["Light", "Dark"];

fn say(parts: &[&[u8]]) {
    sys::print(b"settings: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

/// A zone's name, as init says it: `UTC`, `UTC+1`, `UTC-5`.
fn zone_name(hours: i32) -> String {
    if hours == 0 {
        String::from("UTC")
    } else {
        format!("UTC{hours:+}")
    }
}

/// The parts of a line, as text.
fn line(parts: &[&[u8]]) -> String {
    let mut text = Vec::new();
    for p in parts {
        text.extend_from_slice(p);
    }
    String::from_utf8_lossy(&text).into_owned()
}

struct Settings {
    system: Handle,
    settings: Handle,
    ui: Ui,
    facts: [Id; FACTS],
    status: Id,
    choice: Id,
    look: Id,
    zone: Id,
    refresh: Id,
}

impl Settings {
    /// Asks init for `item` (or a change); the reply's text, or why there
    /// is none.
    fn ask(&self, op: Op, item: Item, value: u8) -> (bool, String) {
        let req = Request { op, item, value };
        let mut msg = [0u8; Reply::LEN];
        // Init's own file requests may take long: a change it made late is
        // not reported as one it never made (audit of the settings, L3).
        match rpc::call(self.settings, &req.encode(), &[], 2000, 15_000, &mut msg)
            .ok()
            .and_then(|n| Reply::decode(&msg[..n]))
        {
            Some(r) => (r.status == Status::Ok, line(&[r.text()])),
            None if op == Op::SetSession => (
                false,
                String::from("init did not answer: whether it changed is unknown; Refresh"),
            ),
            None => (false, String::from("init did not answer")),
        }
    }

    fn info(&self, kind: u64, out: &mut [u8]) -> Option<usize> {
        sys::system_info(self.system, kind, out).ok()
    }

    fn fact(&mut self, i: usize, parts: &[&[u8]]) {
        self.ui.set_text(self.facts[i], &line(parts));
        say(parts);
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
                self.fact(
                    0,
                    &[
                        b"System: ",
                        aricode_rt::fmt_u64(free * 4 / 1024, &mut b),
                        b" of ",
                        aricode_rt::fmt_u64(frames * 4 / 1024, &mut c),
                        b" MiB free, ",
                        aricode_rt::fmt_u64(u64::from_le_bytes(cpus), &mut d),
                        b" processor(s), up ",
                        aricode_rt::fmt_u64(u64::from_le_bytes(up) / 1000, &mut [0u8; 20]),
                        b" s",
                    ],
                );
            }
            _ => self.fact(0, &[b"System: the kernel does not say"]),
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
        self.fact(
            1,
            &[
                b"Keyboard: ",
                &kbd[..n],
                b" (kbd= on the boot command line)",
            ],
        );
        let (_, address) = self.ask(Op::Get, Item::Address, 0);
        self.fact(2, &[b"Network: ", address.as_bytes()]);
        let (_, sources) = self.ask(Op::Get, Item::UpdateSource, 0);
        self.fact(3, &[b"Updates: ", sources.as_bytes()]);
        let (known, session) = self.ask(Op::Get, Item::Session, 0);
        self.fact(4, &[b"Boots into: ", session.as_bytes()]);
        // Nothing is chosen for the owner when the session is not known:
        // the next Enter would write a guess (audit of the settings, L4).
        let chosen = known.then_some(if session.starts_with("desktop") { 1 } else { 0 });
        self.ui.select(self.choice, chosen);
        self.fact(
            5,
            &[b"Driver keys: none (signed driver packages are not built yet)"],
        );
        let (known, look) = self.ask(Op::Get, Item::Appearance, 0);
        let chosen = known.then_some(if look == "dark" { 1 } else { 0 });
        self.ui.select(self.look, chosen);
        let (known, zone) = self.ask(Op::Get, Item::Timezone, 0);
        let row = (-12..=14).find(|&h| zone_name(h) == zone);
        let chosen = row.filter(|_| known).map(|h| (h + 12) as usize);
        self.ui.select(self.zone, chosen);
    }

    /// Chooses the selected session from the next boot.
    fn apply(&mut self) {
        let Some(choice) = self.ui.selected(self.choice) else {
            return;
        };
        let (ok, text) = self.ask(Op::SetSession, Item::Session, choice as u8);
        self.ui.set_text(self.status, &text);
        let tone = if ok { Tone::Secondary } else { Tone::Error };
        self.ui.set_tone(self.status, tone);
        say(&[
            b"chose ",
            CHOICES[choice].as_bytes(),
            b": ",
            text.as_bytes(),
        ]);
        if ok {
            let (_, session) = self.ask(Op::Get, Item::Session, 0);
            self.ui
                .set_text(self.facts[4], &line(&[b"Boots into: ", session.as_bytes()]));
        }
    }

    /// Chooses how the desktop looks: init keeps it and tells the compositor, which
    /// tells every window, this one included.
    fn apply_look(&mut self, look: usize) {
        let (ok, text) = self.ask(Op::SetAppearance, Item::Appearance, look as u8);
        self.ui.set_text(self.status, &text);
        let tone = if ok { Tone::Secondary } else { Tone::Error };
        self.ui.set_tone(self.status, tone);
        say(&[b"appearance: ", text.as_bytes()]);
    }

    /// Chooses the clock's zone: init keeps it and tells the compositor.
    fn apply_zone(&mut self, row: usize) {
        let (ok, text) = self.ask(Op::SetTimezone, Item::Timezone, row as u8);
        self.ui.set_text(self.status, &text);
        let tone = if ok { Tone::Secondary } else { Tone::Error };
        self.ui.set_tone(self.status, tone);
        say(&[b"clock: ", text.as_bytes()]);
    }

    fn show(&mut self, win: &mut Window) {
        let frame = self.ui.paint(&mut win.canvas());
        if !frame.is_empty() {
            let _ = win.present_rects(frame.rects());
        }
    }
}

/// The window's widgets.
fn build(system: Handle, settings: Handle, dark: bool) -> Settings {
    let mut ui = Ui::new(Theme::of(dark), W, H);
    let facts: [Id; FACTS] = core::array::from_fn(|_| ui.paragraph("", Style::Body, Tone::Primary));
    let card = ui.stack(
        Stack::column().pad(Space::Control).card(Fill::Content),
        &facts,
    );
    let view = ui.scroll(card);
    ui.set_flex(view, 1);
    let heading = ui.label(
        "Boot into (Enter applies):",
        Style::Callout,
        Tone::Secondary,
    );
    let choice = ui.list();
    ui.set_rows(choice, CHOICES);
    ui.set_min(choice, 200, 0);
    let refresh = ui.button("Refresh");
    let spacer = ui.spacer();
    let boot = ui.stack(Stack::column(), &[heading, choice]);
    let look_heading = ui.label("Appearance:", Style::Callout, Tone::Secondary);
    let look = ui.list();
    ui.set_rows(look, LOOKS);
    ui.set_min(look, 160, 0);
    let appearance = ui.stack(Stack::column(), &[look_heading, look]);
    let zone_heading = ui.label("Clock:", Style::Callout, Tone::Secondary);
    let zone = ui.list();
    let names: [String; 27] = core::array::from_fn(|i| zone_name(i as i32 - 12));
    ui.set_rows(zone, names.iter().map(String::as_str));
    ui.set_min(zone, 110, 0);
    let clock = ui.stack(Stack::column(), &[zone_heading, zone]);
    let row = ui.stack(
        Stack::row().align(Align::Start).gap(Space::Group),
        &[boot, appearance, clock, refresh, spacer],
    );
    let status = ui.paragraph("", Style::Callout, Tone::Secondary);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[view, row, status],
    );
    ui.set_root(root);
    ui.focus(choice);
    Settings {
        system,
        settings,
        ui,
        facts,
        status,
        choice,
        look,
        zone,
        refresh,
    }
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
    let wanted = wants::POINTER | wants::KEY_CODES | wants::RESIZE | wants::APPEARANCE;
    let Ok(mut win) = Window::create_wanting(session, hw, W, H, "Settings", wanted) else {
        say(&[b"could not create its window"]);
        return 2;
    };
    let mut s = build(system, settings, win.starts_dark());
    s.refresh();
    let frame = s.ui.paint(&mut win.canvas());
    if frame.is_empty() || win.present().is_err() {
        return 3;
    }
    say(&[b"ready"]);
    loop {
        let event = match win.poll(1000) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            // The compositor is gone: so is the desktop.
            Err(_) => return 4,
        };
        match event {
            // Every change is applied when chosen: it closes at once (ADR
            // 0077).
            Event::Close => {
                say(&[b"closed"]);
                let _ = win.close();
                return 0;
            }
            Event::Configure { width, height } => {
                let answer = s.ui.configure(width, height, |ui| {
                    win.resize(hw, width, height, |c| ui.paint_all(c))
                });
                let mut b = [0u8; 20];
                let mut c = [0u8; 20];
                say(&[
                    if answer.is_ok() {
                        b"window "
                    } else {
                        b"could not take "
                    },
                    aricode_rt::fmt_u64(width as u64, &mut b),
                    b"x",
                    aricode_rt::fmt_u64(height as u64, &mut c),
                ]);
            }
            event => s.ui.handle(event),
        }
        while let Some(action) = s.ui.next_action() {
            match action {
                Action::Activated(id, _) if id == s.choice => s.apply(),
                Action::Activated(id, look) if id == s.look => s.apply_look(look),
                Action::Activated(id, row) if id == s.zone => s.apply_zone(row),
                Action::Pressed(id) if id == s.refresh => s.refresh(),
                _ => {}
            }
        }
        s.show(&mut win);
    }
}

aricode_rt::entry!(main);
