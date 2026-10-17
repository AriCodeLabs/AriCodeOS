// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test graphical application (Phase 26): paints its window and listens for
//! events. `arg` = 0 (Demo A) or 1 (Demo B, created 300 ms later so it ends
//! up on top). Handles: 0x10004 = compositor (WRITE), 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_gfx::{rgb, Rect};
use aricode_rt::sys::window::{Event, Window, WindowError};
use aricode_rt::sys::{self, Handle};

const COMPOSITOR: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn num_i32(v: i32) {
    let mut b = [0u8; 20];
    if v < 0 {
        sys::print(b"-");
    }
    sys::print(aricode_rt::fmt_u64(v.unsigned_abs() as u64, &mut b));
}

fn main(index: u64) -> u64 {
    let (name, title, color) = if index == 0 {
        (b"gui_demo A" as &[u8], "Demo A", rgb(0xc0, 0x40, 0x40))
    } else {
        sys::thread_sleep(300);
        (b"gui_demo B" as &[u8], "Demo B", rgb(0x40, 0x80, 0xc0))
    };
    let mut win = match Window::create(COMPOSITOR, HW, 400, 300, title) {
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
            Ok(Some(Event::Focus(f))) => {
                sys::print(name);
                sys::print(if f {
                    b": focus gained\n" as &[u8]
                } else {
                    b": focus lost\n"
                });
                idle = 0;
            }
            Ok(None) => idle += 1,
            Err(_) => return 3,
        }
    }
    if !got_key {
        sys::print(name);
        sys::print(b": no keys\n");
    }
    let _ = win.close();
    0
}

aricode_rt::entry!(main);
