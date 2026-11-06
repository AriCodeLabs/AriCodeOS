// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's file manager (ADR 0074 phase C). It lists the files of
//! the owner's folder `home/`, the only files it reaches (the `home`
//! capability: a directory session of fsd on that folder), with their
//! sizes, and creates, renames and deletes them. A name typed in the field
//! is what New creates and what Rename gives the selected file; a rename
//! never replaces a file that already has the name. Delete asks first.
//! From the keyboard: Tab moves between the controls, Enter
//! in the field creates, Ctrl+R renames, Delete deletes, Esc goes back to
//! the list. Built on the toolkit (ADR 0078, stage 2).
//!
//! Handles, in the broker's order: the folder's file session, its
//! compositor session (both endpoints) and `HARDWARE`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_proto::fs::Status as FsStatus;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::window::{wants, Event, Window};
use aricode_rt::sys::{self, kind, permissions};
use aricode_ui::{Action, Dialog, Fill, Id, Space, Stack, Style, Theme, Tone, Ui};

const W: u32 = 480;
const H: u32 = 400;
/// Files listed at most.
const MAX_FILES: usize = 64;
/// The longest name fsd keeps inside `home/` (32 bytes in all).
const NAME_MAX: usize = 32 - 5;

fn say(parts: &[&[u8]]) {
    sys::print(b"files: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

struct Files {
    fs: FsClient,
    ui: Ui,
    list: Id,
    field: Id,
    new: Id,
    rename: Id,
    delete: Id,
    refresh: Id,
    status: Id,
    /// The names in the list's rows.
    names: Vec<String>,
    /// Deleting asks first.
    deleting: Dialog,
    /// More files than `MAX_FILES`: the list shows the first ones.
    more: bool,
    /// Files in subfolders of `home/`, not listed.
    nested: usize,
}

impl Files {
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

    /// The selected file's name.
    fn selected(&self) -> Option<(usize, String)> {
        let i = self.ui.selected(self.list)?;
        Some((i, self.names.get(i)?.clone()))
    }

    /// Lists `home/` again; `select` is the name to select afterwards
    /// (files move in the list as others are made, so the selection
    /// follows the name, not the row; audit of the file manager, M1).
    /// `false`, said, when the folder could not be listed.
    fn refresh(&mut self, select: Option<&str>) -> bool {
        let before = self.selected().map(|(_, name)| name);
        let mut names = Vec::new();
        let mut rows = Vec::new();
        let mut index = 0;
        let mut entry = [0u8; 32];
        let mut listed = true;
        self.more = false;
        self.nested = 0;
        loop {
            match self.fs.list(index, &mut entry) {
                // Files made in subfolders from the shell cannot be renamed
                // or deleted from here: counted, not listed (audit, L7).
                Ok(Some((n, _))) if entry[..n].contains(&b'/') => self.nested += 1,
                Ok(Some(_)) if names.len() == MAX_FILES => {
                    self.more = true;
                    break;
                }
                Ok(Some((n, size))) => {
                    let name = String::from_utf8_lossy(&entry[..n]).into_owned();
                    rows.push(format!("{name}\t{size}"));
                    names.push(name);
                }
                Ok(None) => break,
                Err(_) => {
                    self.set_status(true, &[b"home/ cannot be listed"]);
                    listed = false;
                    break;
                }
            }
            index += 1;
        }
        self.ui.set_rows(self.list, rows.iter().map(String::as_str));
        self.names = names;
        if let Some(row) = select
            .or(before.as_deref())
            .and_then(|w| self.names.iter().position(|n| n == w))
        {
            self.ui.select(self.list, Some(row));
        }
        listed
    }

    /// How many files there are, said.
    fn say_count(&mut self) {
        let mut b = [0u8; 20];
        let mut c = [0u8; 20];
        let count = aricode_rt::fmt_u64(self.names.len() as u64, &mut b);
        let nested = aricode_rt::fmt_u64(self.nested as u64, &mut c);
        match (self.more, self.nested) {
            (true, _) => self.set_status(false, &[b"the first ", count, b" files of home/"]),
            (false, 0) => self.set_status(false, &[count, b" file(s) in home/"]),
            (false, _) => self.set_status(
                false,
                &[
                    count,
                    b" file(s) in home/, ",
                    nested,
                    b" in subfolders not shown",
                ],
            ),
        }
    }

    fn typed(&mut self) -> Option<String> {
        let text = String::from(self.ui.text(self.field));
        if text.is_empty() {
            self.set_status(true, &[b"type a name in the field first"]);
            self.ui.focus(self.field);
            return None;
        }
        if !aricode_proto::fs::valid_plain_name(&text, NAME_MAX) {
            self.set_status(
                true,
                &["names: a-z, 0-9, . _ - (no accents or ñ)".as_bytes()],
            );
            self.ui.focus(self.field);
            return None;
        }
        Some(text)
    }

    fn create(&mut self) {
        let Some(name) = self.typed() else { return };
        // fsd's create refuses an existing name itself (`Exists`).
        match self.fs.create(&name) {
            Ok(fd) => {
                let _ = self.fs.close(fd);
                self.ui.set_text(self.field, "");
                if self.refresh(Some(&name)) {
                    self.set_status(false, &[b"created ", name.as_bytes()]);
                }
            }
            Err(e) => self.failed(b"cannot create ", &name, e),
        }
    }

    fn rename(&mut self) {
        let Some((_, old)) = self.selected() else {
            return self.set_status(true, &[b"select a file first"]);
        };
        let Some(new) = self.typed() else { return };
        // Never over a file of the new name: fsd checks it in the same
        // step, whatever this list shows (audit of the file manager, H1).
        let renamed = match self.fs.open(&old) {
            Ok(fd) => {
                let r = self.fs.rename_keeping(fd, &new);
                let _ = self.fs.close(fd);
                r
            }
            Err(e) => Err(e),
        };
        match renamed {
            Ok(()) => {
                self.ui.set_text(self.field, "");
                if self.refresh(Some(&new)) {
                    self.set_status(
                        false,
                        &[b"renamed ", old.as_bytes(), b" to ", new.as_bytes()],
                    );
                }
            }
            Err(e) => self.failed(b"cannot rename ", &old, e),
        }
    }

    fn ask_to_delete(&mut self) {
        let Some((_, name)) = self.selected() else {
            return self.set_status(true, &[b"select a file first"]);
        };
        let question = format!("Delete {name}? It cannot be brought back.");
        self.ui.ask(&self.deleting, &question, "Delete");
        say(&[b"asks to delete ", name.as_bytes()]);
    }

    fn delete(&mut self) {
        let Some((_, name)) = self.selected() else {
            return;
        };
        match self.fs.delete(&name) {
            Ok(()) => {
                if self.refresh(None) {
                    self.set_status(false, &[b"deleted ", name.as_bytes()]);
                }
            }
            Err(e) => self.failed(b"cannot delete ", &name, e),
        }
    }

    fn failed(&mut self, what: &[u8], name: &str, e: FsError) {
        let why: &[u8] = match e {
            FsError::Status(FsStatus::Unsupported) => b" (this disk is still AriFS v1)",
            FsError::Status(FsStatus::NotFound) => b" (it is gone)",
            FsError::Status(FsStatus::Invalid) => b" (not a valid name)",
            FsError::Status(FsStatus::NoSpace) => b" (no space left)",
            FsError::Status(FsStatus::Exists) => b" (that name is taken)",
            _ => b"",
        };
        self.set_status(true, &[what, name.as_bytes(), why]);
    }

    fn show(&mut self, win: &mut Window) {
        let frame = self.ui.paint(&mut win.canvas());
        if !frame.is_empty() {
            let _ = win.present_rects(frame.rects());
        }
    }
}

/// The window's widgets, and the folder they show.
fn build(fs: FsClient, dark: bool) -> Files {
    let mut ui = Ui::new(Theme::of(dark), W, H);
    let title = ui.label("home/", Style::Headline, Tone::Primary);
    let list = ui.list();
    ui.set_flex(list, 1);
    let caption = ui.label("Name", Style::Body, Tone::Primary);
    let field = ui.field(NAME_MAX, "a-z, 0-9, . _ -");
    ui.set_flex(field, 1);
    let name = ui.stack(Stack::row(), &[caption, field]);
    let new = ui.button("New");
    let rename = ui.button("Rename");
    let delete = ui.button("Delete");
    let refresh = ui.button("Refresh");
    let buttons = ui.stack(Stack::row(), &[new, rename, delete, refresh]);
    let status = ui.paragraph("", Style::Callout, Tone::Secondary);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[title, list, name, status, buttons],
    );
    ui.set_root(root);
    ui.focus(list);
    let deleting = ui.dialog("Delete", "Keep");
    Files {
        fs,
        ui,
        list,
        field,
        new,
        rename,
        delete,
        refresh,
        status,
        names: Vec::new(),
        deleting,
        more: false,
        nested: 0,
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
    let ([Some(home), Some(session)], Some(hw)) = (endpoints, hw) else {
        say(&[b"started without home/, a window or HARDWARE"]);
        return 1;
    };
    let Ok(fs) = FsClient::new(home, hw) else {
        say(&[b"cannot reach home/"]);
        return 2;
    };
    let wanted = wants::POINTER | wants::KEY_CODES | wants::RESIZE | wants::APPEARANCE;
    let Ok(mut win) = Window::create_wanting(session, hw, W, H, "Files", wanted) else {
        say(&[b"could not create its window"]);
        return 3;
    };
    let mut f = build(fs, win.starts_dark());
    if f.refresh(None) {
        f.say_count();
    }
    let frame = f.ui.paint(&mut win.canvas());
    if frame.is_empty() || win.present().is_err() {
        return 4;
    }
    say(&[b"ready"]);
    loop {
        let event = match win.poll(1000) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            // The compositor is gone: so is the desktop.
            Err(_) => return 5,
        };
        match event {
            // Nothing to save: it closes at once (ADR 0077).
            Event::Close => {
                say(&[b"closed"]);
                let _ = win.close();
                return 0;
            }
            Event::Configure { width, height } => {
                let _ = f.ui.configure(width, height, |ui| {
                    win.resize(hw, width, height, |c| ui.paint_all(c))
                });
            }
            event => f.ui.handle(event),
        }
        while let Some(action) = f.ui.next_action() {
            match action {
                Action::Pressed(id) if id == f.deleting.yes => {
                    f.ui.answered();
                    f.delete();
                }
                Action::Pressed(id) if id == f.deleting.no => {
                    f.ui.answered();
                    say(&[b"kept the file"]);
                }
                Action::DeleteRow(..) => f.ask_to_delete(),
                Action::Submitted(_) => f.create(),
                Action::Shortcut(b'r') => f.rename(),
                Action::Cancel => f.ui.focus(f.list),
                Action::Pressed(id) if id == f.new => f.create(),
                Action::Pressed(id) if id == f.rename => f.rename(),
                Action::Pressed(id) if id == f.delete => f.ask_to_delete(),
                Action::Pressed(id) if id == f.refresh && f.refresh(None) => f.say_count(),
                Action::Copied => {
                    if let Some(text) = f.ui.take_copied() {
                        if win.clipboard_set(hw, &text).is_err() {
                            f.set_status(true, &[b"could not copy"]);
                        }
                    }
                }
                Action::Cut(id) => {
                    if let Some(text) = f.ui.take_copied() {
                        match win.clipboard_set(hw, &text) {
                            Ok(()) => f.ui.cut_done(id),
                            Err(_) => f.set_status(true, &[b"could not cut: the text stays"]),
                        }
                    }
                }
                Action::Paste(id) => match win.clipboard_get(hw, |text| f.ui.paste(id, text)) {
                    Ok(true) => {}
                    Ok(false) => f.set_status(true, &[b"only part of it fits in a name"]),
                    Err(_) => f.set_status(true, &[b"could not paste"]),
                },
                _ => {}
            }
        }
        f.show(&mut win);
    }
}

aricode_rt::entry!(main);
