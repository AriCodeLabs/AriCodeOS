// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's editor (ADR 0074 phase C). It edits text files of the
//! owner's folder `home/`, the only files it reaches (the `home`
//! capability: a directory session of fsd on that folder, so names are
//! plain, `notes.txt`). The file name goes in the field at the top; Open
//! (or Enter there, or Ctrl+O) reads it, Save (or Ctrl+S) replaces it
//! atomically. Tab moves between the name and the text.
//!
//! Handles, in the broker's order: the folder's file session, its
//! compositor session (both endpoints) and `HARDWARE`.
#![no_std]
#![no_main]

use aricode_gfx::widgets::{self, Button, Key, KeyDecoder, TextArea, TextField, PAD, ROW_H};
use aricode_gfx::Rect;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::window::{Event, Window};
use aricode_rt::sys::{self, kind, permissions};
use aricode_sync::SpinLock;

const W: u32 = 640;
const H: u32 = 448;
/// The largest file it edits.
const MAX_TEXT: usize = 64 * 1024;

const NAME_RECT: Rect = Rect::new(PAD + 40, PAD, 360, ROW_H);
const OPEN: Button = Button::at(PAD + 408, PAD, "Open");
const SAVE: Button = Button::at(PAD + 464, PAD, "Save");
const STATUS_Y: i32 = PAD + ROW_H as i32 + 4;
const TEXT_RECT: Rect = Rect::new(
    PAD,
    STATUS_Y + widgets::LINE_H as i32 + 4,
    W - 2 * PAD as u32,
    H - (STATUS_Y as u32 + widgets::LINE_H + 4) - PAD as u32,
);

/// The text (64 KiB): too large for the stack.
static TEXT: SpinLock<TextArea<MAX_TEXT>> = SpinLock::new(TextArea::new());

fn say(parts: &[&[u8]]) {
    sys::print(b"editor: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Name,
    Text,
}

struct Editor {
    fs: FsClient,
    name: TextField<26>,
    focus: Focus,
    status: [u8; 80],
    status_len: usize,
    error: bool,
}

impl Editor {
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

    fn named(&mut self) -> Option<[u8; 26]> {
        if self.name.text().is_empty() {
            self.set_status(true, &[b"type a file name first"]);
            self.focus = Focus::Name;
            return None;
        }
        let mut n = [0u8; 26];
        n[..self.name.text().len()].copy_from_slice(self.name.text().as_bytes());
        Some(n)
    }

    fn open(&mut self) {
        let Some(buf) = self.named() else { return };
        let name = name_of(&buf);
        let fd = match self.fs.open(name) {
            Ok(fd) => fd,
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                TEXT.lock().set(b"");
                self.focus = Focus::Text;
                return self.set_status(
                    false,
                    &[b"new file ", name.as_bytes(), b" (saved with Save)"],
                );
            }
            Err(_) => return self.set_status(true, &[b"cannot open ", name.as_bytes()]),
        };
        // Read whole into the scratch buffer first: a file that cannot be
        // read, or is too large, leaves the text being edited as it was.
        let mut scratch = SCRATCH.lock();
        let mut loaded = 0usize;
        let outcome = loop {
            if loaded == MAX_TEXT {
                // Full: one more byte means the file is larger.
                let mut probe = [0u8; 1];
                break match self.fs.read(fd, loaded as u32, &mut probe) {
                    Ok(0) => Ok(()),
                    Ok(_) => Err(&b" is larger than 64 KiB: not opened"[..]),
                    Err(_) => Err(&b": cannot be read"[..]),
                };
            }
            let end = (loaded + 4096).min(MAX_TEXT);
            match self.fs.read(fd, loaded as u32, &mut scratch[loaded..end]) {
                Ok(0) => break Ok(()),
                Ok(n) => loaded += n,
                Err(_) => break Err(&b": cannot be read"[..]),
            }
        };
        let _ = self.fs.close(fd);
        if let Err(why) = outcome {
            return self.set_status(true, &[name.as_bytes(), why]);
        }
        TEXT.lock().set(&scratch[..loaded]);
        drop(scratch);
        self.focus = Focus::Text;
        let mut b = [0u8; 20];
        self.set_status(
            false,
            &[
                b"opened ",
                name.as_bytes(),
                b" (",
                aricode_rt::fmt_u64(loaded as u64, &mut b),
                b" bytes)",
            ],
        );
    }

    fn save(&mut self) {
        let Some(buf) = self.named() else { return };
        let name = name_of(&buf);
        let fd = match self.fs.open(name) {
            Ok(fd) => fd,
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                match self.fs.create(name) {
                    Ok(fd) => fd,
                    Err(_) => return self.set_status(true, &[b"cannot create ", name.as_bytes()]),
                }
            }
            Err(_) => return self.set_status(true, &[b"cannot open ", name.as_bytes()]),
        };
        let mut text = TEXT.lock();
        let saved = self.fs.replace(fd, text.text());
        let len = text.text().len();
        if saved.is_ok() {
            text.mark_saved();
        }
        drop(text);
        let _ = self.fs.close(fd);
        let mut b = [0u8; 20];
        match saved {
            Ok(_) => self.set_status(
                false,
                &[
                    b"saved ",
                    name.as_bytes(),
                    b" (",
                    aricode_rt::fmt_u64(len as u64, &mut b),
                    b" bytes)",
                ],
            ),
            Err(_) => self.set_status(true, &[b"cannot save ", name.as_bytes()]),
        }
    }

    fn key(&mut self, key: Key) {
        match (key, self.focus) {
            (Key::Ctrl(b's'), _) => self.save(),
            (Key::Ctrl(b'o'), _) => self.open(),
            (Key::Tab, Focus::Name) => self.focus = Focus::Text,
            (Key::Escape, Focus::Text) => self.focus = Focus::Name,
            (Key::Enter, Focus::Name) => self.open(),
            (k, Focus::Name) => {
                self.name.key(k);
            }
            (k, Focus::Text) => {
                TEXT.lock().key(k);
            }
        }
    }

    fn draw(&self, win: &mut Window) {
        let mut c = win.canvas();
        c.fill(widgets::BG);
        c.text(PAD, PAD + 4, "File", widgets::FG, None);
        self.name
            .draw(&mut c, &NAME_RECT, self.focus == Focus::Name);
        OPEN.draw(&mut c, false);
        SAVE.draw(&mut c, true);
        let mut text = TEXT.lock();
        let mut status = [0u8; 96];
        let mut n = self.status_len;
        status[..n].copy_from_slice(&self.status[..n]);
        if text.changed() {
            let mark = b"  [changed]";
            status[n..n + mark.len()].copy_from_slice(mark);
            n += mark.len();
        }
        let color = if self.error {
            widgets::ERROR
        } else {
            widgets::MUTED
        };
        let shown = core::str::from_utf8(&status[..n]).unwrap_or("");
        c.text(PAD, STATUS_Y, shown, color, None);
        text.draw(&mut c, &TEXT_RECT, self.focus == Focus::Text);
    }
}

/// A file's bytes as they are read, before they replace the text.
static SCRATCH: SpinLock<[u8; MAX_TEXT]> = SpinLock::new([0; MAX_TEXT]);

fn name_of(buf: &[u8; 26]) -> &str {
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
    let Ok(mut win) = Window::create(session, hw, W, H, "Editor") else {
        say(&[b"could not create its window"]);
        return 3;
    };
    let mut ed = Editor {
        fs,
        name: TextField::new(),
        focus: Focus::Name,
        status: [0; 80],
        status_len: 0,
        error: false,
    };
    ed.set_status(false, &[b"files in home/; type a name and press Enter"]);
    ed.draw(&mut win);
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
            Event::Key(b) => {
                for key in keys.feed(b).into_iter().flatten() {
                    ed.key(key);
                }
            }
            Event::Click { x, y, .. } => {
                if NAME_RECT.contains(x, y) {
                    ed.focus = Focus::Name;
                } else if TEXT_RECT.contains(x, y) {
                    ed.focus = Focus::Text;
                } else if OPEN.contains(x, y) {
                    ed.open();
                } else if SAVE.contains(x, y) {
                    ed.save();
                }
            }
            Event::Focus(_) => {}
        }
        ed.draw(&mut win);
        let _ = win.present();
    }
}

aricode_rt::entry!(main);
