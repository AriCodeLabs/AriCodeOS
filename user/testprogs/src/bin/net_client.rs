// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Network service client (Phase 22): DHCP configuration, ping to the router,
//! UDP echo and DNS against the host test responder, and errors.
//! Handles: 0x10004 = netd (WRITE), 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_RESPONDER_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
/// Virtual host of the QEMU user network.
const HOST: [u8; 4] = [10, 0, 2, 2];

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    sys::print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

fn main(_arg: u64) -> u64 {
    let Ok(mut net) = NetClient::new(NETD, HW) else {
        return 1;
    };
    // 1. DHCP configuration (netd may take a few ms to obtain it).
    let mut info = None;
    for _ in 0..200 {
        match net.info() {
            Ok(i) => {
                info = Some(i);
                break;
            }
            Err(NetError::Status(Status::NoRoute)) => sys::thread_sleep(20),
            Err(_) => return 2,
        }
    }
    let Some(info) = info else {
        sys::print(b"net_client: no DHCP configuration\n");
        return 3;
    };
    sys::print(b"net_client: ip ");
    print_ip(info.address);
    sys::print(b" router ");
    print_ip(info.router);
    sys::print(b" dns ");
    print_ip(info.dns);
    sys::print(b"\n");
    if info.address == [0; 4] || info.router == [0; 4] {
        return 4;
    }
    // 2. Ping the router (two echoes).
    let mut buf = [0u8; 20];
    for seq in 1..=2u16 {
        match net.ping(info.router, seq, 2000) {
            Ok(rtt) => {
                sys::print(b"net_client: reply from ");
                print_ip(info.router);
                sys::print(b" seq ");
                sys::print(fmt_u64(seq as u64, &mut buf));
                sys::print(b" rtt ");
                sys::print(fmt_u64(rtt, &mut buf));
                sys::print(b" ms\n");
            }
            Err(_) => return 5,
        }
    }
    // 3. Ping an address that never answers: deadline expires.
    if net.ping([10, 0, 2, 99], 9, 300) != Err(NetError::Status(Status::Timeout)) {
        return 6;
    }
    // 4. UDP echo against the host responder.
    let Ok(sock) = net.udp_bind(7) else {
        return 7;
    };
    if net.udp_bind(7) != Err(NetError::Status(Status::InUse)) {
        return 8;
    }
    if net.udp_send(sock, HOST, TEST_RESPONDER_PORT, b"hola desde AriCodeOS") != Ok(20) {
        return 9;
    }
    let mut data = [0u8; 64];
    match net.udp_recv(sock, 2000, &mut data) {
        Ok(d) if d.len == 20 && &data[..20] == b"hola desde AriCodeOS" && d.from == HOST => {
            sys::print(b"net_client: UDP echo received from ");
            print_ip(d.from);
            sys::print(b" port ");
            sys::print(fmt_u64(d.port as u64, &mut buf));
            sys::print(b"\n");
        }
        _ => return 10,
    }
    if net.udp_recv(sock, 100, &mut data) != Err(NetError::Status(Status::Timeout)) {
        return 11;
    }
    if net.udp_close(sock).is_err()
        || net.udp_recv(sock, 10, &mut data) != Err(NetError::Status(Status::BadSock))
    {
        return 12;
    }
    // 5. DNS against the responder (fixed A record).
    match net.resolve("aricodeos.test", Some((HOST, TEST_RESPONDER_PORT)), 2000) {
        Ok(ip) if ip == [10, 1, 2, 3] => {
            sys::print(b"net_client: aricodeos.test -> ");
            print_ip(ip);
            sys::print(b"\n");
        }
        _ => return 13,
    }
    sys::print(b"net_client: DHCP, ping, UDP and DNS over IPC ok\n");
    0
}

aricode_rt::entry!(main);
