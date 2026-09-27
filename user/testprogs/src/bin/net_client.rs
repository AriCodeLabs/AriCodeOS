//! Cliente del servicio de red (Fase 22): configuración DHCP, ping al router,
//! eco UDP y DNS contra el respondedor de pruebas del host, y errores.
//! Handles: 0x10004 = netd (WRITE), 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_RESPONDER_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
/// Anfitrión virtual de la red de usuario de QEMU.
const HOST: [u8; 4] = [10, 0, 2, 2];

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    sys::print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

fn main(_arg: u64) -> u64 {
    let Ok(mut net) = NetClient::new(NETD, HW) else {
        return 1;
    };
    // 1. Configuracion por DHCP (netd puede tardar unos ms en obtenerla).
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
        sys::print(b"net_client: sin configuracion DHCP\n");
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
    // 2. Ping al router (dos ecos).
    let mut buf = [0u8; 20];
    for seq in 1..=2u16 {
        match net.ping(info.router, seq, 2000) {
            Ok(rtt) => {
                sys::print(b"net_client: respuesta de ");
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
    // 3. Ping a una direccion sin respuesta: vence el plazo.
    if net.ping([10, 0, 2, 99], 9, 300) != Err(NetError::Status(Status::Timeout)) {
        return 6;
    }
    // 4. Eco UDP contra el respondedor del host.
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
            sys::print(b"net_client: eco UDP recibido de ");
            print_ip(d.from);
            sys::print(b" puerto ");
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
    // 5. DNS contra el respondedor (registro A fijo).
    match net.resolve("aricodeos.test", Some((HOST, TEST_RESPONDER_PORT)), 2000) {
        Ok(ip) if ip == [10, 1, 2, 3] => {
            sys::print(b"net_client: aricodeos.test -> ");
            print_ip(ip);
            sys::print(b"\n");
        }
        _ => return 13,
    }
    sys::print(b"net_client: DHCP, ping, UDP y DNS por IPC ok\n");
    0
}

aricode_rt::entry!(main);
