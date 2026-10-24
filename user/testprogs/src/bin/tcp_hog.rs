// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! TCP table hog (incident 0012): opens connections to the host echo server
//! until netd refuses one, prints how many it holds and what netd answered
//! (`NetError::describe`, the text the shell shows), then closes them all
//! and exits. Launched through the broker with `run tcp_hog` and the
//! manifest `caps = net`: handles 0x10004 = netd, 0x10005 = HARDWARE.
//! Right after it exits the closed connections sit in TIME-WAIT (2 s), so
//! a connection the shell attempts at once is refused the same way.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_TCP_ECHO_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const HOST: [u8; 4] = [10, 0, 2, 2];
/// More than netd's TCP table (8 slots): the loop must end in a refusal.
const MAX_ATTEMPTS: usize = 16;

fn main(arg: u64) -> u64 {
    let Ok(net) = NetClient::new(NETD, HW) else {
        sys::print(b"tcp_hog: no network client\n");
        return 1;
    };
    // Wait for the DHCP configuration.
    for _ in 0..200 {
        match net.info() {
            Ok(_) => break,
            Err(NetError::Status(Status::NoRoute)) => sys::thread_sleep(20),
            Err(_) => return 2,
        }
    }
    let mut socks = [0u16; MAX_ATTEMPTS];
    let mut open = 0;
    let mut buf = [0u8; 20];
    let refused = loop {
        if open == MAX_ATTEMPTS {
            sys::print(b"tcp_hog: netd never refused a connection\n");
            break None;
        }
        match net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000) {
            Ok(s) => {
                socks[open] = s;
                open += 1;
            }
            Err(e) => break Some(e),
        }
    };
    sys::print(b"tcp_hog: ");
    sys::print(fmt_u64(open as u64, &mut buf));
    sys::print(b" connections open");
    if let Some(e) = refused {
        sys::print(b"; the next one: ");
        sys::print(e.describe().as_bytes());
    }
    sys::print(b"\n");
    // With `arg` = N, the table stays full of live connections for N ms
    // (incident 0027: TIME-WAIT slots are recycled, live ones are not).
    if arg > 0 {
        sys::print(b"tcp_hog: holding them for ");
        sys::print(fmt_u64(arg, &mut buf));
        sys::print(b" ms\n");
        sys::thread_sleep(arg);
    }
    for s in &socks[..open] {
        let _ = net.tcp_close(*s);
    }
    match refused {
        Some(NetError::Status(Status::Busy)) => 0,
        _ => 3,
    }
}

aricode_rt::entry!(main);
