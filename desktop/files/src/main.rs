// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's file manager (ADR 0074 phase C). It lists the files of
//! the owner's folder `home/`, the only files it reaches (the `home`
//! capability: a directory session of fsd on that folder), with their
//! sizes, and creates, renames and deletes them. A name typed in the field
//! is what New creates and what Rename gives the selected file; a rename
//! never replaces a file that already has the name. Delete asks for a
//! second press. From the keyboard: Tab moves between the list and the
//! field, Enter in the field creates, Ctrl+R renames, Delete deletes.
//!
//! Handles, in the broker's order: the folder's file session, its
//! compositor session (both endpoints) and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_gfx::widgets::{self, Button, Key, KeyDecoder, List, TextField, PAD, ROW_H};
use aricode_gfx::Rect;
use aricode_proto::fs::Status as FsStatus;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::window::{Event, Window};
use aricode_rt::sys::{self, kind, permissions};

const W: u32 = 480;
const H: u32 = 400;
/// Files listed at most.
const MAX_FILES: usize = 64;
/// The longest name fsd keeps inside `home/` (32 bytes in all).
const NAME_MAX: usize = 32 - 5;

const LIST_RECT: Rect = Rect::new(PAD, PAD + ROW_H as i32, W - 2 * PAD as u32, 9 * ROW_H);
const FIELD_Y: i32 = LIST_RECT.y + LIST_RECT.h as i32 + PAD;
const NAME_RECT: Rect = Rect::new(PAD + 48, FIELD_Y, 200, ROW_H);
const BUTTONS_Y: i32 = FIELD_Y + ROW_H as i32 + PAD;
const NEW: Button = Button::at(PAD, BUTTONS_Y, "New");
const RENAME: Button = Button::at(PAD + 48, BUTTONS_Y, "Rename");
const DELETE: Button = Button::at(PAD + 120, BUTTONS_Y, "Delete");
const REFRESH: Button = Button::at(PAD + 192, BUTTONS_Y, "Refresh");
const STATUS_Y: i32 = BUTTONS_Y + ROW_H as i32 + PAD;

fn say(parts: &[&[u8]]) {
    sys::print(b"files: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Name,
}

struct Files {
    fs: FsClient,
    names: [[u8; 32]; MAX_FILES],
    lens: [u8; MAX_FILES],
    sizes: [u64; MAX_FILES],
    /// The rows as shown: name and size.
    rows: [[u8; 48]; MAX_FILES],
    row_lens: [u8; MAX_FILES],
    list: List,
    field: TextField<NAME_MAX>,
    focus: Focus,
    /// The file a first Delete was pressed on.
    armed: Option<usize>,
    status: [u8; 80],
    status_len: usize,
    error: bool,
}

impl Files {
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
        self.error = error;
        say(parts);
    }

    /// Lists `home/` again.
    fn refresh(&mut self) {
        let mut count = 0;
        let mut entry = [0u8; 32];
        while count < MAX_FILES {
            match self.fs.list(count as u32, &mut entry) {
                Ok(Some((n, size))) => {
                    self.names[count] = entry;
                    self.lens[count] = n as u8;
                    self.sizes[count] = size;
                    let mut row = [b' '; 48];
                    row[..n].copy_from_slice(&entry[..n]);
                    let mut b = [0u8; 20];
                    let digits = aricode_rt::fmt_u64(size, &mut b);
                    let at = 34usize.max(n + 1);
                    let end = (at + digits.len()).min(row.len());
                    row[at..end].copy_from_slice(&digits[..end - at]);
                    self.rows[count] = row;
                    self.row_lens[count] = end as u8;
                    count += 1;
                }
                Ok(None) => break,
                Err(_) => {
                    self.set_status(true, &[b"home/ cannot be listed"]);
                    break;
                }
            }
        }
        self.list.set_count(count);
        self.armed = None;
    }

    fn row(&self, i: usize) -> &str {
        core::str::from_utf8(&self.rows[i][..self.row_lens[i] as usize]).unwrap_or("?")
    }

    fn exists(&self, name: &str) -> bool {
        (0..self.list.count).any(|i| self.name(i) == name)
    }

    fn typed(&mut self) -> Option<[u8; NAME_MAX]> {
        if self.field.text().is_empty() {
            self.set_status(true, &[b"type a name in the field first"]);
            self.focus = Focus::Name;
            return None;
        }
        let mut n = [0u8; NAME_MAX];
        n[..self.field.text().len()].copy_from_slice(self.field.text().as_bytes());
        Some(n)
    }

    fn create(&mut self) {
        let Some(buf) = self.typed() else { return };
        let name = name_of(&buf);
        if self.exists(name) {
            return self.set_status(true, &[name.as_bytes(), b" already exists"]);
        }
        match self.fs.create(name) {
            Ok(fd) => {
                let _ = self.fs.close(fd);
                self.refresh();
                self.field.clear();
                self.set_status(false, &[b"created ", name.as_bytes()]);
            }
            Err(e) => self.failed(b"cannot create ", name, e),
        }
    }

    fn rename(&mut self) {
        let Some(i) = self.list.selected else {
            return self.set_status(true, &[b"select a file first"]);
        };
        let Some(buf) = self.typed() else { return };
        let new = name_of(&buf);
        let mut old = [0u8; 32];
        let n = self.lens[i] as usize;
        old[..n].copy_from_slice(&self.names[i][..n]);
        let old = core::str::from_utf8(&old[..n]).unwrap_or("");
        // fsd's rename replaces a file of the new name: never here.
        if self.exists(new) {
            return self.set_status(true, &[new.as_bytes(), b" already exists"]);
        }
        let renamed = match self.fs.open(old) {
            Ok(fd) => {
                let r = self.fs.rename(fd, new);
                let _ = self.fs.close(fd);
                r
            }
            Err(e) => Err(e),
        };
        match renamed {
            Ok(()) => {
                self.refresh();
                self.field.clear();
                self.set_status(
                    false,
                    &[b"renamed ", old.as_bytes(), b" to ", new.as_bytes()],
                );
            }
            Err(e) => self.failed(b"cannot rename ", old, e),
        }
    }

    fn delete(&mut self) {
        let Some(i) = self.list.selected else {
            return self.set_status(true, &[b"select a file first"]);
        };
        let mut name = [0u8; 32];
        let n = self.lens[i] as usize;
        name[..n].copy_from_slice(&self.names[i][..n]);
        let name = core::str::from_utf8(&name[..n]).unwrap_or("");
        if self.armed != Some(i) {
            self.armed = Some(i);
            return self.set_status(false, &[b"press Delete again to delete ", name.as_bytes()]);
        }
        match self.fs.delete(name) {
            Ok(()) => {
                self.refresh();
                self.set_status(false, &[b"deleted ", name.as_bytes()]);
            }
            Err(e) => self.failed(b"cannot delete ", name, e),
        }
    }

    fn failed(&mut self, what: &[u8], name: &str, e: FsError) {
        let why: &[u8] = match e {
            FsError::Status(FsStatus::Unsupported) => b" (this disk is still AriFS v1)",
            FsError::Status(FsStatus::NotFound) => b" (it is gone)",
            FsError::Status(FsStatus::Invalid) => b" (not a valid name)",
            FsError::Status(FsStatus::NoSpace) => b" (no space left)",
            _ => b"",
        };
        self.set_status(true, &[what, name.as_bytes(), why]);
    }

    fn key(&mut self, key: Key) {
        match (key, self.focus) {
            (Key::Tab, Focus::List) => self.focus = Focus::Name,
            (Key::Tab | Key::Escape, Focus::Name) => self.focus = Focus::List,
            (Key::Delete, Focus::List) => self.delete(),
            (Key::Enter, Focus::Name) => self.create(),
            (Key::Ctrl(b'r'), _) => self.rename(),
            (k, Focus::List) => {
                if self.list.key(k) {
                    self.armed = None;
                }
            }
            (k, Focus::Name) => {
                self.field.key(k);
            }
        }
    }

    fn draw(&self, win: &mut Window) {
        let mut c = win.canvas();
        c.fill(widgets::BG);
        c.text(PAD, PAD + 4, "home/", widgets::FG, None);
        self.list.draw(&mut c, |i| self.row(i));
        c.text(PAD, FIELD_Y + 4, "Name", widgets::FG, None);
        self.field
            .draw(&mut c, &NAME_RECT, self.focus == Focus::Name);
        NEW.draw(&mut c, false);
        RENAME.draw(&mut c, false);
        DELETE.draw(&mut c, self.armed.is_some());
        REFRESH.draw(&mut c, false);
        let status = core::str::from_utf8(&self.status[..self.status_len]).unwrap_or("");
        let color = if self.error {
            widgets::ERROR
        } else {
            widgets::MUTED
        };
        c.text(PAD, STATUS_Y, status, color, None);
    }
}

fn name_of(buf: &[u8; NAME_MAX]) -> &str {
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..n]).unwrap_or("")
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
    let Ok(mut win) = Window::create(session, hw, W, H, "Files") else {
        say(&[b"could not create its window"]);
        return 3;
    };
    let mut f = Files {
        fs,
        names: [[0; 32]; MAX_FILES],
        lens: [0; MAX_FILES],
        sizes: [0; MAX_FILES],
        rows: [[0; 48]; MAX_FILES],
        row_lens: [0; MAX_FILES],
        list: List::new(LIST_RECT),
        field: TextField::new(),
        focus: Focus::List,
        armed: None,
        status: [0; 80],
        status_len: 0,
        error: false,
    };
    f.refresh();
    let mut b = [0u8; 20];
    let count = aricode_rt::fmt_u64(f.list.count as u64, &mut b);
    f.set_status(false, &[count, b" file(s) in home/"]);
    f.draw(&mut win);
    if win.present().is_err() {
        return 4;
    }
    say(&[b"ready"]);
    let mut keys = KeyDecoder::new();
    loop {
        let event = match win.poll(1000) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            // The compositor is gone: so is the desktop.
            Err(_) => return 5,
        };
        match event {
            Event::Key(byte) => {
                for key in keys.feed(byte).into_iter().flatten() {
                    f.key(key);
                }
            }
            Event::Click { x, y, .. } => {
                if let Some(row) = f.list.hit(x, y) {
                    f.focus = Focus::List;
                    if f.list.selected != Some(row) {
                        f.armed = None;
                    }
                    f.list.select(row);
                } else if NAME_RECT.contains(x, y) {
                    f.focus = Focus::Name;
                } else if NEW.contains(x, y) {
                    f.create();
                } else if RENAME.contains(x, y) {
                    f.rename();
                } else if DELETE.contains(x, y) {
                    f.delete();
                } else if REFRESH.contains(x, y) {
                    f.refresh();
                    let count = aricode_rt::fmt_u64(f.list.count as u64, &mut b);
                    f.set_status(false, &[count, b" file(s) in home/"]);
                }
            }
            Event::Focus(_) => {}
        }
        f.draw(&mut win);
        let _ = win.present();
    }
}

aricode_rt::entry!(main);
