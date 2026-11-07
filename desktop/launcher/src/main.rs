// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's launcher (ADR 0074 phase C). It asks the broker which
//! installed applications it may offer (signed packages that open a
//! window) and starts the one chosen, in the background and with that
//! application's own manifest; the broker refuses anything else. Typing
//! narrows the list, and the arrows move in it from the search field. A click
//! selects a row, and a click on the selected row, Enter or "Open" opens
//! it (a stray click starts nothing); "Leave the desktop" ends the desktop
//! session and gives the screen back to the text console.
//!
//! Handles, in the broker's order: its compositor session, the launch
//! endpoint (both endpoints, send only) and `HARDWARE`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use aricode_rt::sys::launch::{self, LaunchError, Status};
use aricode_rt::sys::window::{wants, Event, Window};
use aricode_rt::sys::{self, kind, permissions, Handle};
use aricode_ui::{Action, Dialog, Fill, Id, ListIcons, Space, Stack, Style, Theme, Tone, Ui};

const W: u32 = 400;
const H: u32 = 420;
/// The numbers of its menu.
const MENU_OPEN: u32 = 1;
const MENU_REFRESH: u32 = 2;
const MENU_LEAVE: u32 = 3;
/// Applications listed at most (the broker offers 16).
const MAX_APPS: usize = 16;
const NAME_MAX: usize = 32;

fn launcher_theme(dark: bool) -> Theme {
    let mut theme = Theme::of(dark);
    theme.row_h = 32;
    theme
}

fn say(parts: &[&[u8]]) {
    sys::print(b"launcher: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

/// What the launcher shows.
struct Launcher {
    ui: Ui,
    /// What is typed narrows the list.
    search: Id,
    empty: Id,
    /// Every application offered; the list shows those that match.
    names: Vec<String>,
    list: Id,
    open: Id,
    refresh: Id,
    leave: Id,
    /// The last outcome. Above the buttons, so that growing it moves none of them.
    status: Id,
    /// Leaving ends every app the launcher started, unsaved text included: it asks
    /// first (audit of ADR 0077, M1).
    leaving: Dialog,
}

impl Launcher {
    fn new(dark: bool) -> Self {
        let mut ui = Ui::new(launcher_theme(dark), W, H);
        let title = ui.label("Applications", Style::Title2, Tone::Primary);
        let search = ui.field(NAME_MAX, "Search applications");
        let list = ui.list();
        ui.set_list_icons(list, ListIcons::Applications);
        let empty = ui.paragraph(
            "No matching applications. Try another search.",
            Style::Body,
            Tone::Secondary,
        );
        ui.set_hidden(empty, true);
        ui.set_flex(list, 1);
        let status = ui.paragraph("", Style::Callout, Tone::Secondary);
        let open = ui.default_button("Open");
        let refresh = ui.button("Refresh");
        let leave = ui.button("Leave the desktop");
        let spacer = ui.spacer();
        let buttons = ui.stack(Stack::row(), &[open, refresh, spacer]);
        let exit = ui.stack(Stack::row(), &[leave]);
        let hint = ui.label(
            "Type to search · Enter to open",
            Style::Subheadline,
            Tone::Secondary,
        );
        let root = ui.stack(
            Stack::column().pad(Space::Margin).fill(Fill::Window),
            &[title, search, list, empty, status, buttons, exit, hint],
        );
        // A snapped window can be shorter than the controls' natural height.
        // Keep every action reachable, and let Tab reveal the focused control.
        let root = ui.scroll(root);
        ui.set_root(root);
        ui.focus(search);
        let leaving = ui.dialog("Leave", "Stay");
        Self {
            ui,
            search,
            empty,
            names: Vec::new(),
            list,
            open,
            refresh,
            leave,
            status,
            leaving,
        }
    }

    fn set_status(&mut self, error: bool, parts: &[&[u8]]) {
        let mut text = Vec::new();
        for p in parts {
            text.extend_from_slice(p);
        }
        self.ui
            .set_text(self.status, &String::from_utf8_lossy(&text));
        let tone = if error { Tone::Error } else { Tone::Secondary };
        self.ui.set_tone(self.status, tone);
        say(parts);
    }

    /// Asks the broker again which applications it may offer.
    fn refresh(&mut self, apps: Handle) {
        let mut names: Vec<String> = Vec::new();
        let mut failed = false;
        while names.len() < MAX_APPS {
            let mut name = [0u8; NAME_MAX];
            match launch::installed(apps, names.len() as u64, &mut name) {
                Ok(Some(n)) => names.push(String::from_utf8_lossy(&name[..n]).into_owned()),
                Ok(None) => break,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        let count = names.len();
        self.names = names;
        self.show_matches();
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

    /// The list shows the names that hold what was typed, in any case, the first of them
    /// selected.
    fn show_matches(&mut self) {
        let typed = self.ui.text(self.search).to_lowercase();
        let rows: Vec<&str> = self
            .names
            .iter()
            .map(String::as_str)
            .filter(|n| n.to_lowercase().contains(&typed))
            .collect();
        self.ui.set_rows(self.list, rows);
        let first = (self.ui.list_len(self.list) > 0).then_some(0);
        self.ui.select(self.list, first);
        self.ui.set_enabled(self.open, first.is_some());
        self.ui.set_hidden(self.empty, first.is_some());
        self.ui.set_text(
            self.empty,
            if self.names.is_empty() {
                "No applications installed yet."
            } else {
                "No matching applications. Try another search."
            },
        );
    }

    /// Starts the selected application.
    fn open(&mut self, apps: Handle) {
        let Some(i) = self.ui.selected(self.list) else {
            return;
        };
        let app = String::from(self.ui.row(self.list, i));
        let name = app.as_bytes();
        match launch::spawn(apps, &app, 0) {
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

    fn ask_to_leave(&mut self) {
        let question = "Leave the desktop? Open applications close with it.";
        self.ui.ask(&self.leaving, question, "Leave");
        say(&[b"asks to leave the desktop"]);
    }

    fn show(&mut self, win: &mut Window) {
        let frame = self.ui.paint(&mut win.canvas());
        if !frame.is_empty() {
            let _ = win.present_rects(frame.rects());
        }
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
    let wanted = wants::POINTER | wants::KEY_CODES | wants::RESIZE | wants::APPEARANCE;
    let Ok(mut win) = Window::create_wanting(session, hw, W, H, "Launcher", wanted) else {
        say(&[b"could not create its window"]);
        return 2;
    };
    let file = [
        ("Desktop", 0),
        ("Open\tEnter", MENU_OPEN),
        ("Refresh", MENU_REFRESH),
        ("Leave the desktop", MENU_LEAVE),
    ];
    if file
        .iter()
        .any(|&(label, id)| win.menu_add(0, label, id).is_err())
    {
        say(&[b"the bar did not take its menu"]);
    }
    let mut l = Launcher::new(win.starts_dark());
    l.refresh(apps);
    for i in 0..l.ui.list_len(l.list) {
        say(&[b"offers ", l.ui.row(l.list, i).as_bytes()]);
    }
    let frame = l.ui.paint(&mut win.canvas());
    if frame.is_empty() || win.present().is_err() {
        return 3;
    }
    say(&[b"ready"]);
    'running: loop {
        let event = match win.poll(1000) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            // The compositor is gone: so is the desktop.
            Err(_) => return 4,
        };
        match event {
            Event::Appearance { dark } => l.ui.set_theme(launcher_theme(dark)),
            // A second click on the close box answers the question it asked.
            Event::Close if l.ui.asking() => break 'running,
            Event::Close => l.ask_to_leave(),
            Event::Configure { width, height } => {
                let _ = l.ui.configure(width, height, |ui| {
                    win.resize(hw, width, height, |c| ui.paint_all(c))
                });
            }
            event => l.ui.handle(event),
        }
        while let Some(action) = l.ui.next_action() {
            match action {
                Action::Pressed(id) if id == l.leaving.yes => break 'running,
                Action::Pressed(id) if id == l.leaving.no => l.ui.answered(),
                Action::Pressed(id) if id == l.leave => l.ask_to_leave(),
                Action::Pressed(id) if id == l.open => l.open(apps),
                Action::Pressed(id) if id == l.refresh => l.refresh(apps),
                Action::Menu(MENU_LEAVE) => l.ask_to_leave(),
                Action::Menu(MENU_OPEN) => l.open(apps),
                Action::Menu(MENU_REFRESH) => l.refresh(apps),
                Action::Activated(..) | Action::Submitted(_) => l.open(apps),
                Action::Edited(id) if id == l.search => l.show_matches(),
                Action::Arrow(_, up) => l.ui.select_next(l.list, up),
                _ => {}
            }
        }
        l.show(&mut win);
    }
    say(&[b"leaving the desktop"]);
    let _ = win.close();
    0
}

aricode_rt::entry!(main);
