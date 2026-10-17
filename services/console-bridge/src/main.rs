// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Console bridge (stage 2, ADR 0013): forwards key presses from the
//! user-space keyboards (virtio-input, USB HID) to the kernel console input
//! ring, so the shell reads from them just like from COM1 or the PS/2
//! keyboard. Handles: 0x10004 = console input (`MANAGE`), 0x10005.. =
//! endpoints of the input services (`arg` = how many).
#![no_std]
#![no_main]

use aricode_input::{Keyboard, EV_KEY};
use aricode_proto::input::{Event, MAX_EVENTS};
use aricode_rt::sys::input::InputClient;
use aricode_rt::sys::{self, Handle};

const CONSOLE_IN: Handle = Handle(0x0001_0004);
const FIRST_INPUT: u32 = 0x0001_0005;
const MAX_INPUTS: usize = 4;
/// Wait per device on each pass: services are polled in turn (each accepts
/// a single waiting reader), so the worst-case latency of a key press is
/// this timeout times the number of remaining devices.
const POLL_MS: u32 = 20;
/// Pause before reading again from a driver that died holding the read
/// (ADR 0030): its supervisor relaunches it on the same endpoint.
const RETRY_MS: u64 = 50;

fn say(msg: &[u8]) {
    sys::print(b"console-bridge: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn main(count: u64) -> u64 {
    let count = (count as usize).min(MAX_INPUTS);
    if count == 0 {
        say(b"no input devices");
        return 1;
    }
    let mut clients: [Option<InputClient>; MAX_INPUTS] = [const { None }; MAX_INPUTS];
    for (i, slot) in clients.iter_mut().take(count).enumerate() {
        match InputClient::new(Handle(FIRST_INPUT + i as u32)) {
            Ok(c) => *slot = Some(c),
            Err(e) => {
                aricode_rt::print_error(b"console-bridge: input", e);
                return 2;
            }
        }
    }
    // One keyboard state per device: shift and lock state belong to it.
    let mut keyboards = [Keyboard::new(); MAX_INPUTS];
    let mut events = [Event::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    // Up to 4 bytes per key (escape sequences for the arrow keys).
    let mut text = [0u8; MAX_EVENTS * 4];
    // No announcement of its own: init does it before launching the shell,
    // so nothing interleaves with the first prompt on the console (SMP).
    let mut parked = false;
    loop {
        if clients.iter().all(Option::is_none) {
            // Every device was given up by the supervisor. Exiting would only
            // make init relaunch the bridge to find the same: park quietly.
            if !parked {
                say(b"every input device is gone; idle");
                parked = true;
            }
            sys::thread_sleep(60_000);
            continue;
        }
        for (i, slot) in clients.iter_mut().take(count).enumerate() {
            let Some(client) = slot else { continue };
            // No events (timeout) or service busy: nothing to translate.
            // The driver died holding the read (`ReplyLost`): its supervisor
            // relaunches it on the same endpoint, so read again after a
            // pause. Only an endpoint nobody can receive on (`ServiceGone`,
            // the device given up, ADR 0030) drops the device.
            let n = match client.read(POLL_MS, &mut events, &mut times) {
                Ok(n) => n,
                Err(e) if e.service_gone() => {
                    sys::print(b"console-bridge: input device ");
                    let mut b = [0u8; 20];
                    sys::print(aricode_rt::fmt_u64(i as u64, &mut b));
                    sys::print(b" is gone; dropping it\n");
                    *slot = None;
                    continue;
                }
                Err(e) if e.reply_lost() => {
                    sys::thread_sleep(RETRY_MS);
                    0
                }
                Err(_) => 0,
            };
            let mut len = 0;
            for e in &events[..n] {
                let ev = aricode_input::Event {
                    kind: e.kind,
                    code: e.code,
                    value: e.value,
                };
                // Mouse buttons are EV_KEY >= 0x100: they are not text.
                if ev.kind == EV_KEY && ev.code >= 0x100 {
                    continue;
                }
                for &ch in keyboards[i].feed(ev).as_slice() {
                    text[len] = ch;
                    len += 1;
                }
            }
            if len > 0 && sys::console_inject(CONSOLE_IN, &text[..len]).is_err() {
                say(b"console rejects input; exiting");
                return 3;
            }
        }
    }
}

aricode_rt::entry!(main);
