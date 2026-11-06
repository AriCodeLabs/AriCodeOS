// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The desktop's editor (ADR 0074 phase C). It edits text files of the
//! owner's folder `home/`, the only files it reaches (the `home`
//! capability: a directory session of fsd on that folder, so names are
//! plain, `notes.txt`). The file name goes in the field at the top; Open
//! (or Enter there, or Ctrl+O) reads it, Save (or Ctrl+S) replaces it
//! atomically. Tab moves from the name to the text, Ctrl+L or Esc back.
//! Opening over unsaved changes, or saving over another file than the one
//! opened, takes a second press. It opens plain text only: a file that is not
//! UTF-8 or holds control characters would not come back from it byte for
//! byte, so it is refused and left alone. Built on the toolkit (ADR 0078,
//! stage 2).
//!
//! Handles, in the broker's order: the folder's file session, its
//! compositor session (both endpoints) and `HARDWARE`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::window::{wants, Event, Window};
use aricode_rt::sys::{self, kind, permissions};
use aricode_sync::SpinLock;
use aricode_ui::{Action, Fill, Id, Space, Stack, Style, Theme, Tone, Ui};

const W: u32 = 640;
const H: u32 = 448;
/// The largest file it edits.
const MAX_TEXT: usize = 64 * 1024;
/// More for the heap: the text, a copy of it as it is pasted, and the glyphs.
const MORE_HEAP: usize = 512 * 1024;

fn say(parts: &[&[u8]]) {
    sys::print(b"editor: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

/// Whether this comes back from the editor byte for byte: `text` keeps every line
/// break and tab, and anything else that is not printable would be changed.
fn is_plain(bytes: &[u8]) -> Option<&str> {
    let text = core::str::from_utf8(bytes).ok()?;
    text.chars()
        .all(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'))
        .then_some(text)
}

/// What a second press confirms.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    /// Open, throwing the unsaved changes away.
    Discard,
    /// Save over another file of that name.
    Replace,
    /// Close the window, throwing the unsaved changes away (ADR 0077).
    Close,
}

struct Editor {
    fs: FsClient,
    ui: Ui,
    name: Id,
    open: Id,
    save: Id,
    text: Id,
    status: Id,
    changed_mark: Id,
    /// The text differs from what was last read or saved.
    changed: bool,
    /// The file the text came from or was last saved to (empty: none).
    loaded: [u8; 26],
    /// That file did not exist when it was opened and was not saved yet:
    /// if someone made it since, saving over it asks first (audit, L2).
    loaded_new: bool,
    /// The action a first press asked to confirm, and for which name.
    armed: Option<(Confirm, [u8; 26])>,
}

impl Editor {
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

    fn set_changed(&mut self, changed: bool) {
        self.changed = changed;
        let mark = if changed { "changed" } else { "" };
        self.ui.set_text(self.changed_mark, mark);
    }

    /// The name typed, if it is one fsd keeps (audit of the editor, L2).
    fn named(&mut self) -> Option<[u8; 26]> {
        let text = String::from(self.ui.text(self.name));
        if text.is_empty() {
            self.set_status(true, &[b"type a file name first"]);
            self.ui.focus(self.name);
            return None;
        }
        if !aricode_proto::fs::valid_plain_name(&text, 26) {
            self.set_status(
                true,
                &["names: a-z, 0-9, . _ - (no accents or ñ)".as_bytes()],
            );
            self.ui.focus(self.name);
            return None;
        }
        let mut n = [0u8; 26];
        n[..text.len()].copy_from_slice(text.as_bytes());
        Some(n)
    }

    /// Whether this is the second press of `what` for `name`; the first
    /// one only arms it.
    fn confirmed(&mut self, what: Confirm, name: [u8; 26]) -> bool {
        if self.armed == Some((what, name)) {
            self.armed = None;
            return true;
        }
        self.armed = Some((what, name));
        false
    }

    /// The owner clicked the close box: whether to close now. Unsaved
    /// changes ask first; a second click discards them (ADR 0077).
    fn close_asked(&mut self) -> bool {
        if !self.changed || self.confirmed(Confirm::Close, [0; 26]) {
            return true;
        }
        self.set_status(true, &[b"unsaved changes: close again to discard them"]);
        false
    }

    fn disarm_close(&mut self) {
        if matches!(self.armed, Some((Confirm::Close, _))) {
            self.armed = None;
        }
    }

    /// The name field shows the file the text belongs to again, after an
    /// Open that failed (audit of the editor, M1).
    fn restore_name(&mut self) {
        let loaded = self.loaded;
        self.ui.set_text(self.name, name_of(&loaded));
    }

    fn open(&mut self) {
        let Some(buf) = self.named() else { return };
        let name = name_of(&buf);
        // Unsaved changes are never thrown away on one press (audit of the
        // editor, M2).
        if self.changed && !self.confirmed(Confirm::Discard, buf) {
            return self.set_status(true, &[b"unsaved changes: Open again to discard them"]);
        }
        let fd = match self.fs.open(name) {
            Ok(fd) => fd,
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                self.ui.set_text(self.text, "");
                self.set_changed(false);
                self.loaded = buf;
                self.loaded_new = true;
                self.ui.focus(self.text);
                return self.set_status(
                    false,
                    &[b"new file ", name.as_bytes(), b" (saved with Save)"],
                );
            }
            Err(_) => {
                self.set_status(true, &[b"cannot open ", name.as_bytes()]);
                return self.restore_name();
            }
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
            self.set_status(true, &[name.as_bytes(), why]);
            return self.restore_name();
        }
        let Some(text) = is_plain(&scratch[..loaded]) else {
            drop(scratch);
            self.set_status(true, &[name.as_bytes(), b" is not plain text: not opened"]);
            return self.restore_name();
        };
        self.ui.set_text(self.text, text);
        drop(scratch);
        self.set_changed(false);
        self.loaded = buf;
        self.loaded_new = false;
        self.ui.focus(self.text);
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
        let mut created = false;
        let fd = match self.fs.open(name) {
            // Another file of that name is replaced only on a second press
            // (audit of the editor, M1).
            Ok(fd)
                if (buf != self.loaded || self.loaded_new)
                    && !self.confirmed(Confirm::Replace, buf) =>
            {
                let _ = self.fs.close(fd);
                return self.set_status(
                    true,
                    &[name.as_bytes(), b" exists: Save again to replace it"],
                );
            }
            Ok(fd) => fd,
            Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                match self.fs.create(name) {
                    Ok(fd) => {
                        created = true;
                        fd
                    }
                    Err(_) => return self.set_status(true, &[b"cannot create ", name.as_bytes()]),
                }
            }
            Err(_) => return self.set_status(true, &[b"cannot open ", name.as_bytes()]),
        };
        let saved = self.fs.replace(fd, self.ui.text(self.text).as_bytes());
        let len = self.ui.text(self.text).len();
        if saved.is_ok() {
            self.set_changed(false);
        }
        let _ = self.fs.close(fd);
        // A file made for a save that failed is not left behind empty
        // (audit of the editor, L3).
        if saved.is_err() && created {
            let _ = self.fs.delete(name);
        }
        if saved.is_ok() {
            self.loaded = buf;
            self.loaded_new = false;
        }
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
            Err(_) => self.set_status(
                true,
                &[b"cannot save ", name.as_bytes(), b" (nothing written)"],
            ),
        }
    }

    fn show(&mut self, win: &mut Window) {
        let frame = self.ui.paint(&mut win.canvas());
        if !frame.is_empty() {
            let _ = win.present_rects(frame.rects());
        }
    }
}

/// The window's widgets, and the folder they work in.
fn build(fs: FsClient, dark: bool) -> Editor {
    let mut ui = Ui::new(Theme::of(dark), W, H);
    let name = ui.field(26, "File name");
    ui.set_flex(name, 1);
    let open = ui.button("Open");
    let save = ui.default_button("Save");
    let top = ui.stack(Stack::row(), &[name, open, save]);
    let status = ui.label("", Style::Callout, Tone::Secondary);
    ui.set_flex(status, 1);
    let changed_mark = ui.label("", Style::Callout, Tone::Secondary);
    let line = ui.stack(Stack::row(), &[status, changed_mark]);
    let text = ui.area(MAX_TEXT);
    ui.set_flex(text, 1);
    let root = ui.stack(
        Stack::column().pad(Space::Margin).fill(Fill::Window),
        &[top, line, text],
    );
    ui.set_root(root);
    ui.focus(name);
    Editor {
        fs,
        ui,
        name,
        open,
        save,
        text,
        status,
        changed_mark,
        changed: false,
        loaded: [0; 26],
        loaded_new: false,
        armed: None,
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
    // The window's quota holds this too: it stays for the program's life.
    let Some(heap) = sys::dma_alloc(hw, MORE_HEAP as u64)
        .ok()
        .and_then(|dma| sys::mmio_map(dma).ok())
    else {
        say(&[b"no memory for its text"]);
        return 3;
    };
    // SAFETY: a new mapping of whole pages, never unmapped, used by nothing else.
    unsafe { aricode_rt::extend_heap(heap as usize, MORE_HEAP) };
    let wanted = wants::POINTER | wants::KEY_CODES | wants::RESIZE | wants::APPEARANCE;
    let Ok(mut win) = Window::create_wanting(session, hw, W, H, "Editor", wanted) else {
        say(&[b"could not create its window"]);
        return 3;
    };
    let mut ed = build(fs, win.starts_dark());
    ed.set_status(false, &[b"files in home/; type a name and press Enter"]);
    let frame = ed.ui.paint(&mut win.canvas());
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
        // A key or a click, anything but a second click on the close box,
        // disarms it (audit of ADR 0077, L6).
        if matches!(event, Event::Key(_) | Event::Click { .. }) {
            ed.disarm_close();
        }
        let focus = ed.ui.focused();
        match event {
            Event::Close if ed.close_asked() => {
                say(&[b"closed"]);
                let _ = win.close();
                return 0;
            }
            Event::Configure { width, height } => {
                let _ = ed.ui.configure(width, height, |ui| {
                    win.resize(hw, width, height, |c| ui.paint_all(c))
                });
            }
            event => ed.ui.handle(event),
        }
        // Moving to another control disarms a confirmation.
        if ed.ui.focused() != focus {
            ed.armed = None;
        }
        while let Some(action) = ed.ui.next_action() {
            match action {
                Action::Edited(id) => {
                    ed.armed = None;
                    if id == ed.text {
                        ed.set_changed(true);
                    }
                }
                Action::Shortcut(b's') => ed.save(),
                Action::Shortcut(b'o') => ed.open(),
                Action::Submitted(_) => ed.open(),
                Action::Pressed(id) if id == ed.open => ed.open(),
                Action::Pressed(id) if id == ed.save => ed.save(),
                // Esc is known only with the key after it: Ctrl+L goes to the
                // name at once (audit of the editor, L4).
                Action::Shortcut(b'l') | Action::Cancel => {
                    ed.armed = None;
                    ed.ui.focus(ed.name);
                }
                Action::Copied => {
                    if let Some(text) = ed.ui.take_copied() {
                        let mut b = [0u8; 20];
                        let n = aricode_rt::fmt_u64(text.len() as u64, &mut b);
                        match win.clipboard_set(hw, &text) {
                            Ok(()) => say(&[b"copied ", n, b" bytes"]),
                            Err(_) => say(&[b"could not copy ", n, b" bytes"]),
                        }
                    }
                }
                Action::Paste(id) => {
                    let _ = win.clipboard_get(hw, |text| ed.ui.paste(id, text));
                }
                _ => {}
            }
        }
        ed.show(&mut win);
    }
}

aricode_rt::entry!(main);
