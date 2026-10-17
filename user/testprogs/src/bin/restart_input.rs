// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Input driver that crashes holding a read (`input_restart`, ADR 0030).
//! Speaks the input protocol on endpoint 0x10004: `Info` names the device;
//! a `Read` asking for exactly `HOLD_MS` makes it crash with the request
//! in hand (as a driver that faults while a client waits for events);
//! any other `Read` gets one key event. init supervises it, so the crash
//! is followed by a relaunch on the same endpoint.
#![no_std]
#![no_main]

use aricode_proto::input::{Event, Op, Reply, Request, Status};
use aricode_rt::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);
/// Read timeout that tells this driver to crash (shared with
/// `restart_input_client`).
const HOLD_MS: u32 = 4321;
const EV_KEY: u16 = 1;
const KEY_A: u16 = 30;

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

fn main(_arg: u64) -> u64 {
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 1];
    loop {
        let Ok((n, 1)) = sys::ipc_receive(EP, &mut msg, &mut handles, TIMEOUT_INFINITE) else {
            return 1;
        };
        let reply_to = handles[0];
        match Request::decode(&msg[..n]) {
            Some(Request { op: Op::Info, .. }) => answer(
                reply_to,
                Reply::status(Status::Ok)
                    .with_name("Restart Test Keyboard")
                    .unwrap_or(Reply::status(Status::Ok)),
            ),
            Some(Request {
                op: Op::Read,
                timeout_ms: HOLD_MS,
            }) => {
                sys::print(b"restart_input: crashing with a read pending\n");
                // SAFETY: deliberate fault; page 0 is never mapped and the
                // address is hidden from the compiler so that it emits the read.
                let bad = core::hint::black_box(0x40u64) as *const u64;
                return unsafe { bad.read_volatile() };
            }
            Some(Request { op: Op::Read, .. }) => {
                let event = Event {
                    kind: EV_KEY,
                    code: KEY_A,
                    value: 1,
                };
                answer(reply_to, Reply::events(&[event], &[0]));
            }
            None => answer(reply_to, Reply::status(Status::Invalid)),
        }
    }
}

aricode_rt::entry!(main);
