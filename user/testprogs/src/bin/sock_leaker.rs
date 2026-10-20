// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Socket leaker (incident 0016): binds UDP ports until netd refuses one,
//! connects to the host echo server until netd refuses one, says how many
//! of each it holds and exits without closing any, as a program killed or
//! crashed mid-work would. Launched through the broker with `run
//! sock_leaker` and the manifest `caps = net`: handles 0x10004 = its netd
//! session, 0x10005 = HARDWARE. netd must close the sockets once the
//! process is gone (ADR 0037), so the shell's next connection succeeds and
//! `diag` lists the reclaim.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_TCP_ECHO_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const HOST: [u8; 4] = [10, 0, 2, 2];
/// First UDP port bound; the rest follow.
const FIRST_UDP_PORT: u16 = 40_000;
/// More than either table of netd (8 slots each): both loops must end in a
/// refusal.
const MAX_ATTEMPTS: usize = 16;

/// Opens with `open` until netd answers `Busy`; returns how many were
/// opened and whether the refusal was the expected one.
fn fill(mut open: impl FnMut(usize) -> Result<u16, NetError>) -> (usize, bool) {
    let mut held = 0;
    loop {
        if held == MAX_ATTEMPTS {
            return (held, false);
        }
        match open(held) {
            Ok(_) => held += 1,
            Err(NetError::Status(Status::Busy)) => return (held, true),
            Err(_) => return (held, false),
        }
    }
}

fn main(_arg: u64) -> u64 {
    let Ok(net) = NetClient::new(NETD, HW) else {
        sys::print(b"sock_leaker: no network client\n");
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
    let (udp, udp_refused) = fill(|i| net.udp_bind(FIRST_UDP_PORT + i as u16));
    let (tcp, tcp_refused) = fill(|_| net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000));
    let mut buf = [0u8; 20];
    sys::print(b"sock_leaker: holding ");
    sys::print(fmt_u64(udp as u64, &mut buf));
    sys::print(b" udp sockets and ");
    sys::print(fmt_u64(tcp as u64, &mut buf));
    sys::print(b" tcp connections; the next of each was ");
    if udp_refused && tcp_refused {
        sys::print(b"refused (busy); exiting without closing any\n");
        0
    } else {
        sys::print(b"not refused with busy\n");
        3
    }
}

aricode_rt::entry!(main);
