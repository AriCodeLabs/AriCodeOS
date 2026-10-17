// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Client of an input driver that crashes holding its read (`input_restart`,
//! ADR 0030). The crash must come back as `ReplyLost` (retry), never as
//! `ServiceGone` (drop the device); after the supervisor relaunches the
//! driver on the same endpoint, the device must deliver events again, as
//! console-bridge and the compositor rely on. Handle 0x10004 = the input
//! endpoint (WRITE). Success = 0; otherwise the failing step.
#![no_std]
#![no_main]

use aricode_proto::input::{Event, MAX_EVENTS};
use aricode_rt::sys::input::InputClient;
use aricode_rt::sys::{self, Handle};

const EP: Handle = Handle(0x0001_0004);
/// Read timeout that makes `restart_input` crash with the read pending.
const HOLD_MS: u32 = 4321;
const KEY_A: u16 = 30;

fn main(_arg: u64) -> u64 {
    let Ok(dev) = InputClient::new(EP) else {
        return 1;
    };
    let mut events = [Event::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    match dev.read(HOLD_MS, &mut events, &mut times) {
        Err(e) if e.reply_lost() => {
            sys::print(b"restart_input_client: driver died holding the read: ReplyLost\n")
        }
        Err(e) if e.service_gone() => {
            sys::print(b"restart_input_client: a crash must not read as ServiceGone\n");
            return 2;
        }
        _ => return 3,
    }
    // The next read queues on the same endpoint for the relaunched driver.
    // Anything but `ServiceGone` (a timeout while the driver restarts) is
    // retried after a pause.
    for _ in 0..100 {
        match dev.read(100, &mut events, &mut times) {
            Ok(n) if n >= 1 && events[0].code == KEY_A => {
                sys::print(
                    b"restart_input_client: the device delivers events after the driver restart\n",
                );
                return 0;
            }
            Ok(_) => {}
            Err(e) if e.service_gone() => {
                sys::print(b"restart_input_client: device given up instead of restarted\n");
                return 4;
            }
            Err(_) => sys::thread_sleep(50),
        }
    }
    5
}

aricode_rt::entry!(main);
