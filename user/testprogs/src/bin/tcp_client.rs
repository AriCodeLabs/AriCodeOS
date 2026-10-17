// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Test TCP client (Phase 23): echo against the host server, two
//! simultaneous connections, a refused connection and our own server that
//! the host connects to via `hostfwd`. Handles: 0x10004 = netd, 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_TCP_ECHO_PORT, TEST_TCP_LISTEN_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const HOST: [u8; 4] = [10, 0, 2, 2];

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    sys::print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

/// Sends `msg` and waits for exactly its echo.
fn echo(net: &mut NetClient, sock: u16, msg: &[u8]) -> bool {
    if net.tcp_send_all(sock, msg).is_err() {
        return false;
    }
    let mut got = [0u8; 256];
    let mut n = 0;
    while n < msg.len() {
        match net.tcp_recv(sock, 3000, &mut got[n..]) {
            Ok(0) | Err(_) => return false,
            Ok(k) => n += k,
        }
    }
    &got[..n] == msg
}

fn main(_arg: u64) -> u64 {
    let Ok(mut net) = NetClient::new(NETD, HW) else {
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
    // 1. Simple echo.
    let sock = match net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000) {
        Ok(s) => s,
        Err(_) => return 3,
    };
    if !echo(&mut net, sock, b"hola tcp desde AriCodeOS") {
        return 4;
    }
    sys::print(b"tcp_client: TCP echo ok\n");
    if net.tcp_close(sock).is_err() {
        return 5;
    }
    // 2. Two simultaneous connections with different data.
    let (Ok(a), Ok(b)) = (
        net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000),
        net.tcp_connect(HOST, TEST_TCP_ECHO_PORT, 3000),
    ) else {
        return 6;
    };
    if !echo(&mut net, a, b"conexion A")
        || !echo(&mut net, b, b"conexion B")
        || !echo(&mut net, a, b"otra vez A")
    {
        return 7;
    }
    let _ = net.tcp_close(a);
    let _ = net.tcp_close(b);
    sys::print(b"tcp_client: two simultaneous connections ok\n");
    // 3. Closed port: the host answers RST.
    match net.tcp_connect(HOST, TEST_TCP_ECHO_PORT + 100, 3000) {
        Err(NetError::Status(Status::Reset)) => {
            sys::print(b"tcp_client: refused connection detected\n")
        }
        _ => return 8,
    }
    // 4. Our own server: the host connects via hostfwd and we echo its data.
    let Ok(listener) = net.tcp_listen(TEST_TCP_LISTEN_PORT) else {
        return 9;
    };
    if net.tcp_listen(TEST_TCP_LISTEN_PORT) != Err(NetError::Status(Status::InUse)) {
        return 10;
    }
    sys::print(b"TCP_LISTEN_READY\n");
    // The host probe connects as soon as it sees the marker; without a probe
    // boot should not be delayed by more than 1.5 s.
    match net.tcp_accept(listener, 1500) {
        Ok((conn, from, port)) => {
            let mut buf = [0u8; 20];
            sys::print(b"tcp_client: incoming connection from ");
            print_ip(from);
            sys::print(b" port ");
            sys::print(fmt_u64(port as u64, &mut buf));
            sys::print(b"\n");
            let mut data = [0u8; 256];
            match net.tcp_recv(conn, 3000, &mut data) {
                Ok(n) if n > 0 => {
                    if net.tcp_send_all(conn, &data[..n]).is_err() {
                        return 11;
                    }
                    sys::print(b"tcp_client: echoed to the host ");
                    sys::print(fmt_u64(n as u64, &mut buf));
                    sys::print(b" bytes\n");
                }
                _ => return 12,
            }
            // Wait for the host to close, then close.
            let _ = net.tcp_recv(conn, 2000, &mut data);
            let _ = net.tcp_close(conn);
        }
        Err(NetError::Status(Status::Timeout)) => {
            sys::print(b"tcp_client: nobody connected to the server\n")
        }
        Err(_) => return 13,
    }
    let _ = net.tcp_close(listener);
    sys::print(b"tcp_client: TCP over IPC ok\n");
    0
}

aricode_rt::entry!(main);
