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
use aricode_proto::input::{Event, Reply, Status, MAX_EVENTS};
use aricode_rt::sys::input::{InputClient, InputError};
use aricode_rt::sys::{self, abi, Handle};

const CONSOLE_IN: Handle = Handle(0x0001_0004);
const FIRST_INPUT: u32 = 0x0001_0005;
const MAX_INPUTS: usize = 4;
/// How long each outstanding read waits for events: one read stays
/// outstanding per device and the bridge waits on all their replies at
/// once (ADR 0044), so a key press is forwarded as soon as its driver has
/// it and an idle driver is not woken (before: 20 ms reads, polled in
/// turn). A read that ends without events is simply started again.
const READ_MS: u32 = 10_000;
/// How often the bridge looks for a read whose driver died holding it: its
/// reply endpoint closes, which a wait on several endpoints does not
/// report while the others are open (ADR 0036).
const CHECK_MS: u64 = 1_000;
/// Pause before reading again from a driver that died holding the read
/// (ADR 0030): its supervisor relaunches it on the same endpoint.
const RETRY_MS: u64 = 50;

fn say(msg: &[u8]) {
    sys::print(b"console-bridge: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn dropped(i: usize) {
    sys::print(b"console-bridge: input device ");
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(i as u64, &mut b));
    sys::print(b" is gone; dropping it\n");
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
    // The reply endpoint of each device's outstanding read.
    let mut pending: [Option<Handle>; MAX_INPUTS] = [None; MAX_INPUTS];
    // One keyboard state per device: shift and lock state belong to it.
    let mut keyboards = [Keyboard::new(); MAX_INPUTS];
    let mut events = [Event::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    let mut msg = [0u8; Reply::LEN];
    let mut none = [Handle(0); abi::IPC_MAX_HANDLES];
    // Up to 4 bytes per key (escape sequences for the arrow keys).
    let mut text = [0u8; MAX_EVENTS * 4];
    // No announcement of its own: init does it before launching the shell,
    // so nothing interleaves with the first prompt on the console (SMP).
    let mut parked = false;
    loop {
        // A read outstanding on every device that has none. Only an
        // endpoint nobody can receive on (`ServiceGone`, the device given
        // up, ADR 0030) drops the device; anything else is tried again.
        let mut retry = false;
        for (i, slot) in clients.iter_mut().take(count).enumerate() {
            let Some(client) = slot else { continue };
            if pending[i].is_some() {
                continue;
            }
            match client.start_read(READ_MS) {
                Ok(reply) => pending[i] = Some(reply),
                Err(e) if e.service_gone() => {
                    dropped(i);
                    *slot = None;
                }
                Err(_) => retry = true,
            }
        }
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
        let mut set = [Handle(0); MAX_INPUTS];
        let mut owner = [0usize; MAX_INPUTS];
        let mut n_set = 0;
        for (i, p) in pending.iter().enumerate() {
            if let Some(reply) = p {
                set[n_set] = *reply;
                owner[n_set] = i;
                n_set += 1;
            }
        }
        if n_set == 0 {
            sys::thread_sleep(RETRY_MS);
            continue;
        }
        let wait = if retry { RETRY_MS } else { CHECK_MS };
        match sys::ipc_receive_any(&set[..n_set], &mut msg, &mut none, wait) {
            Ok((which, len, nh)) => {
                for h in &none[..nh] {
                    let _ = sys::handle_close(*h);
                }
                let i = owner[which];
                if let Some(reply) = pending[i].take() {
                    let _ = sys::handle_close(reply);
                }
                // No events (the read timed out): nothing to translate; the
                // read starts again. A busy service (another reader holds
                // its one place) is asked again only after a pause.
                let n = match InputClient::read_reply(&msg[..len], &mut events, &mut times) {
                    Ok(n) => n,
                    Err(InputError::Status(Status::Busy)) => {
                        sys::thread_sleep(RETRY_MS);
                        0
                    }
                    Err(_) => 0,
                };
                if inject(&mut keyboards[i], &events[..n], &mut text).is_err() {
                    say(b"console rejects input; exiting");
                    return 3;
                }
            }
            // Nothing within the wait, or every read's driver gone: look
            // for reads whose driver died holding them (their reply
            // endpoint closed) and start those again after a pause.
            Err(_) => {
                let mut lost = false;
                for p in pending.iter_mut() {
                    let Some(reply) = *p else { continue };
                    if sys::ipc_receive(reply, &mut msg, &mut none, 0)
                        == Err(sys::Error::PeerClosed)
                    {
                        let _ = sys::handle_close(reply);
                        *p = None;
                        lost = true;
                    }
                }
                if lost {
                    sys::thread_sleep(RETRY_MS);
                }
            }
        }
    }
}

/// Translates a device's key events into console text and injects it.
fn inject(keyboard: &mut Keyboard, events: &[Event], text: &mut [u8]) -> Result<(), sys::Error> {
    let mut len = 0;
    for e in events {
        let ev = aricode_input::Event {
            kind: e.kind,
            code: e.code,
            value: e.value,
        };
        // Mouse buttons are EV_KEY >= 0x100: they are not text.
        if ev.kind == EV_KEY && ev.code >= 0x100 {
            continue;
        }
        for &ch in keyboard.feed(ev).as_slice() {
            text[len] = ch;
            len += 1;
        }
    }
    if len > 0 {
        sys::console_inject(CONSOLE_IN, &text[..len])?;
    }
    Ok(())
}

aricode_rt::entry!(main);
