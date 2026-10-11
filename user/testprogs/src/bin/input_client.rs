//! Input client (Phase 25): identifies the virtio-input devices, announces
//! INPUT_READY and decodes what the host injects over QMP: a keyboard line
//! plus mouse motion and a click. Handles: 0x10004.. = input service
//! endpoints (`arg` = how many).
#![no_std]
#![no_main]

use aricode_input::{describe, Keyboard, BTN_LEFT, EV_KEY, EV_REL, REL_X, REL_Y};
use aricode_proto::input::{Event, Status, MAX_EVENTS};
use aricode_rt::sys::input::{InputClient, InputError};
use aricode_rt::sys::{self, Handle};

fn num_i32(v: i32) {
    let mut b = [0u8; 20];
    if v < 0 {
        sys::print(b"-");
    }
    sys::print(aricode_rt::fmt_u64(v.unsigned_abs() as u64, &mut b));
}

fn main(count: u64) -> u64 {
    let count = (count as usize).min(4);
    if count == 0 {
        sys::print(b"input_client: no input devices\n");
        return 0;
    }
    let mut clients: [Option<InputClient>; 4] = [None, None, None, None];
    let mut keyboard = None;
    for (i, slot) in clients.iter_mut().take(count).enumerate() {
        let Ok(c) = InputClient::new(Handle(0x0001_0004 + i as u32)) else {
            return 1;
        };
        let mut name = [0u8; 32];
        let Ok(n) = c.name(&mut name) else {
            return 2;
        };
        sys::print(b"input_client: device ");
        let mut b = [0u8; 20];
        sys::print(aricode_rt::fmt_u64(i as u64, &mut b));
        sys::print(b": ");
        sys::print(&name[..n]);
        sys::print(b"\n");
        if name[..n].windows(8).any(|w| w == b"Keyboard") {
            keyboard = Some(i);
        }
        *slot = Some(c);
    }
    sys::print(b"INPUT_READY\n");
    let mut kbd = Keyboard::new();
    let mut line = [0u8; 64];
    let mut line_len = 0;
    let mut got_line = false;
    let mut dx = 0i32;
    let mut dy = 0i32;
    let mut moved = false;
    let mut clicked = false;
    let mut idle_rounds = 0;
    let mut events = [Event::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    while !(got_line && moved && clicked) && idle_rounds < 8 {
        let mut any = false;
        for (i, c) in clients.iter().take(count).enumerate() {
            let Some(c) = c else { continue };
            let n = match c.read(200, &mut events, &mut times) {
                Ok(n) => n,
                Err(InputError::Status(Status::Timeout)) => 0,
                Err(_) => return 3,
            };
            for e in &events[..n] {
                any = true;
                let ev = aricode_input::Event {
                    kind: e.kind,
                    code: e.code,
                    value: e.value,
                };
                // By event type, not by device: one service may combine
                // keyboard and mouse (USB HID) in a single device.
                let is_button = ev.kind == EV_KEY && ev.code >= 0x100;
                if ev.kind == EV_KEY && !is_button && (Some(i) == keyboard || keyboard.is_none()) {
                    for &ch in kbd.feed(ev).as_slice() {
                        if ch == b'\n' {
                            sys::print(b"input_client: keyboard: ");
                            sys::print(&line[..line_len]);
                            sys::print(b"\n");
                            got_line = true;
                        } else if line_len < line.len() {
                            line[line_len] = ch;
                            line_len += 1;
                        }
                    }
                } else if ev.kind == EV_REL {
                    match ev.code {
                        REL_X => dx += ev.value as i32,
                        REL_Y => dy += ev.value as i32,
                        _ => {}
                    }
                    moved = true;
                } else if ev.kind == EV_KEY && ev.code == BTN_LEFT && ev.value == 1 {
                    sys::print(b"input_client: mouse: ");
                    sys::print(describe(&ev).as_bytes());
                    sys::print(b" pressed\n");
                    clicked = true;
                }
            }
        }
        if any {
            idle_rounds = 0;
        } else {
            idle_rounds += 1;
        }
    }
    if moved {
        sys::print(b"input_client: mouse: dx=");
        num_i32(dx);
        sys::print(b" dy=");
        num_i32(dy);
        sys::print(b"\n");
    }
    if !(got_line && moved && clicked) {
        sys::print(b"input_client: no more events (nobody typed)\n");
    }
    sys::print(b"input_client: input events over IPC ok\n");
    0
}

aricode_rt::entry!(main);
