// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's launcher (ADR 0074 phase C). It asks the broker which
//! installed applications it may offer (signed packages that open a
//! window) and starts the one chosen, in the background and with that
//! application's own manifest; the broker refuses anything else. A click
//! selects a row, and a click on the selected row, Enter or "Open" opens
//! it (a stray click starts nothing); "Leave the desktop" ends the desktop
//! session and gives the screen back to the text console.
//!
//! Handles, in the broker's order: its compositor session, the launch
//! endpoint (both endpoints, send only) and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_gfx::widgets::{self, Button, Key, KeyDecoder, List, PAD, ROW_H};
use aricode_gfx::Rect;
use aricode_rt::sys::launch::{self, LaunchError, Status};
use aricode_rt::sys::window::{Event, Window};
use aricode_rt::sys::{self, kind, permissions, Handle};

const W: u32 = 320;
const H: u32 = 360;
/// Applications listed at most (the broker offers 16).
const MAX_APPS: usize = 16;
const NAME_MAX: usize = 32;

fn say(parts: &[&[u8]]) {
    sys::print(b"launcher: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

/// What the launcher shows and remembers.
struct Launcher {
    names: [[u8; NAME_MAX]; MAX_APPS],
    lens: [u8; MAX_APPS],
    list: List,
    /// The last outcome, shown under the buttons.
    status: [u8; 64],
    status_len: usize,
    status_error: bool,
}

const LIST_RECT: Rect = Rect::new(PAD, PAD + ROW_H as i32, W - 2 * PAD as u32, 9 * ROW_H);
const BUTTONS_Y: i32 = LIST_RECT.y + LIST_RECT.h as i32 + PAD;
const OPEN: Button = Button::at(PAD, BUTTONS_Y, "Open");
const REFRESH: Button = Button::at(PAD + 56, BUTTONS_Y, "Refresh");
const LEAVE: Button = Button::at(PAD + 136, BUTTONS_Y, "Leave the desktop");

impl Launcher {
    fn name(&self, i: usize) -> &str {
        core::str::from_utf8(&self.names[i][..self.lens[i] as usize]).unwrap_or("?")
    }

    fn set_status(&mut self, error: bool, parts: &[&[u8]]) {
        let mut n = 0;
        for p in parts {
            let take = p.len().min(self.status.len() - n);
            self.status[n..n + take].copy_from_slice(&p[..take]);
            n += take;
        }
        self.status_len = n;
        self.status_error = error;
        say(parts);
    }

    /// Asks the broker again which applications it may offer.
    fn refresh(&mut self, apps: Handle) {
        let mut count = 0;
        let mut failed = false;
        while count < MAX_APPS {
            let mut name = [0u8; NAME_MAX];
            match launch::installed(apps, count as u64, &mut name) {
                Ok(Some(n)) => {
                    self.names[count] = name;
                    self.lens[count] = n as u8;
                    count += 1;
                }
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        self.list.set_count(count);
        let mut b = [0u8; 20];
        if failed {
            self.set_status(true, &[b"the broker did not list the applications"]);
        } else if count == 0 {
            self.set_status(false, &[b"no applications installed yet"]);
        } else {
            self.set_status(
                false,
                &[
                    aricode_rt::fmt_u64(count as u64, &mut b),
                    b" application(s) offered",
                ],
            );
        }
    }

    /// Starts the selected application.
    fn open(&mut self, apps: Handle) {
        let Some(i) = self.list.selected else { return };
        let mut name = [0u8; NAME_MAX];
        let n = self.lens[i] as usize;
        name[..n].copy_from_slice(&self.names[i][..n]);
        let name = &name[..n];
        let app = core::str::from_utf8(name).unwrap_or("");
        match launch::spawn(apps, app, 0) {
            Ok(()) => self.set_status(false, &[b"started ", name]),
            Err(e) => {
                let why: &[u8] = match e {
                    LaunchError::Status(Status::Limit) => b": the desktop's 6 slots are full",
                    LaunchError::Status(Status::Denied) => b": not allowed",
                    LaunchError::Status(Status::NotFound) => b": no longer installed",
                    LaunchError::Status(Status::BadManifest) => b": its manifest is wrong",
                    LaunchError::Status(_) | LaunchError::BadName => b": did not start",
                    LaunchError::Ipc(_) => b": the broker did not answer",
                };
                self.set_status(true, &[name, why]);
            }
        }
    }

    fn draw(&self, win: &mut Window) {
        let mut c = win.canvas();
        c.fill(widgets::BG);
        c.text(PAD, PAD + 4, "Applications", widgets::FG, None);
        self.list.draw(&mut c, |i| self.name(i));
        OPEN.draw(&mut c, true);
        REFRESH.draw(&mut c, false);
        LEAVE.draw(&mut c, false);
        let status = core::str::from_utf8(&self.status[..self.status_len]).unwrap_or("");
        let color = if self.status_error {
            widgets::ERROR
        } else {
            widgets::MUTED
        };
        c.text(PAD, BUTTONS_Y + ROW_H as i32 + PAD, status, color, None);
        c.text(
            PAD,
            H as i32 - PAD - widgets::LINE_H as i32,
            "Enter opens; Ctrl+Alt+Backspace leaves",
            widgets::MUTED,
            None,
        );
    }
}

fn main(_arg: u64) -> u64 {
    let mut endpoints = [None; 2];
    let mut found = 0;
    let mut hw = None;
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) && found < endpoints.len() {
            endpoints[found] = Some(h);
            found += 1;
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let ([Some(session), Some(apps)], Some(hw)) = (endpoints, hw) else {
        say(&[b"started without a window, the launch endpoint or HARDWARE"]);
        return 1;
    };
    let Ok(mut win) = Window::create(session, hw, W, H, "Launcher") else {
        say(&[b"could not create its window"]);
        return 2;
    };
    let mut l = Launcher {
        names: [[0; NAME_MAX]; MAX_APPS],
        lens: [0; MAX_APPS],
        list: List::new(LIST_RECT),
        status: [0; 64],
        status_len: 0,
        status_error: false,
    };
    l.refresh(apps);
    for i in 0..l.list.count {
        say(&[b"offers ", l.name(i).as_bytes()]);
    }
    l.draw(&mut win);
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
            Event::Click { x, y, .. } => {
                if let Some(row) = l.list.hit(x, y) {
                    if l.list.selected == Some(row) {
                        l.open(apps);
                    } else {
                        l.list.select(row);
                    }
                } else if OPEN.contains(x, y) {
                    l.open(apps);
                } else if REFRESH.contains(x, y) {
                    l.refresh(apps);
                } else if LEAVE.contains(x, y) {
                    break;
                }
            }
            Event::Key(b) => {
                for key in keys.feed(b).into_iter().flatten() {
                    if key == Key::Enter {
                        l.open(apps);
                    } else {
                        l.list.key(key);
                    }
                }
            }
            Event::Focus(_) => {}
        }
        l.draw(&mut win);
        let _ = win.present();
    }
    say(&[b"leaving the desktop"]);
    let _ = win.close();
    0
}

aricode_rt::entry!(main);
