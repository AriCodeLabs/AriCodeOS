// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Drives netd's deepest call path (incident 0018). Fills the 8-slot TCP
//! table with 7 connections to the host echo server and one connection
//! toward an address on the subnet that nobody answers ARP for (its SYN
//! waits parked for a MAC that never comes), asks for a ninth so netd
//! refuses it after a sweep, and exits without closing anything and
//! without waiting for the pending connect. The shell's next connection
//! then finds the table full within the sweep period: netd sweeps from
//! inside that request, finds this process gone, resets its connections,
//! and the reset toward the unresolved address goes through ARP parking
//! and an ARP request, the deepest direct path of the driver.
//!
//! Launched through the broker with `run tcp_orphan` and the manifest
//! `caps = net`: handles 0x10004 = its netd session, 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::net::{Op, Request, Status, TEST_TCP_ECHO_PORT};
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const HOST: [u8; 4] = [10, 0, 2, 2];
/// On QEMU's user network (10.0.2.0/24) but not one of the addresses it
/// answers ARP for (gateway .2, DNS .3): the next hop stays unresolved.
const NO_ARP: [u8; 4] = [10, 0, 2, 77];
const NO_ARP_PORT: u16 = 9;
/// netd's TCP table minus the connection left pending toward `NO_ARP`.
const ESTABLISHED: usize = 7;

fn main(_arg: u64) -> u64 {
    let Ok(net) = NetClient::new(NETD, HW) else {
        sys::print(b"tcp_orphan: no network client\n");
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
    for _ in 0..ESTABLISHED {
        if net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000).is_err() {
            sys::print(b"tcp_orphan: could not open the echo connections\n");
            return 3;
        }
    }
    // The pending connect: sent by hand with a reply endpoint nobody reads,
    // so the connection stays in SYN-SENT behind an unresolved next hop.
    let Ok((reply, reply_w)) = sys::endpoint_create_pair() else {
        return 4;
    };
    let mut req = Request::new(Op::TcpConnect);
    req.addr = NO_ARP;
    req.port = NO_ARP_PORT;
    req.timeout_ms = 30_000;
    if sys::ipc_send(NETD, &req.encode(), &[reply_w], 1000).is_err() {
        return 5;
    }
    // The ninth: refused after netd sweeps its sessions (this one is alive),
    // which also restarts the sweep period the shell's request falls in.
    match net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000) {
        Err(NetError::Status(Status::Busy)) => {}
        _ => {
            sys::print(b"tcp_orphan: the ninth connection was not refused with busy\n");
            return 6;
        }
    }
    let _ = reply;
    sys::print(b"tcp_orphan: 8 connections held, one toward an address without ARP; exiting without closing\n");
    0
}

aricode_rt::entry!(main);
