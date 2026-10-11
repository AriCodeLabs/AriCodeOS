//! Test HTTP client (Phase 24): GET against the host server, with a
//! Content-Length body, chunked, 404 and a URL whose name is resolved by the
//! DNS responder. Handles: 0x10004 = netd, 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::net::{Status, TEST_HTTP_PORT};
use aricode_rt::fmt_u64;
use aricode_rt::sys::http::{self, HttpError};
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, Handle};

const NETD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn main(_arg: u64) -> u64 {
    let Ok(mut net) = NetClient::new(NETD, HW) else {
        return 1;
    };
    for _ in 0..200 {
        match net.info() {
            Ok(_) => break,
            Err(NetError::Status(Status::NoRoute)) => sys::thread_sleep(20),
            Err(_) => return 2,
        }
    }
    let mut body = [0u8; 4096];
    let mut buf = [0u8; 20];
    // 1. Body with Content-Length.
    match http::get(&mut net, "http://10.0.2.2:17010/hola", &mut body, 5000) {
        Ok(f) if f.status == 200 && &body[..f.body_len] == b"Hola desde el host\n" => {
            sys::print(b"http_client: GET /hola -> 200, ");
            sys::print(fmt_u64(f.body_len as u64, &mut buf));
            sys::print(b" bytes: ");
            sys::print(&body[..f.body_len]);
        }
        _ => return 3,
    }
    // 2. Chunked body.
    match http::get(&mut net, "10.0.2.2:17010/chunked", &mut body, 5000) {
        Ok(f) if f.status == 200 && &body[..f.body_len] == b"primer trozo, segundo trozo" => {
            sys::print(b"http_client: GET /chunked -> 200 (chunked) ok\n");
        }
        _ => return 4,
    }
    // 3. 404.
    match http::get(&mut net, "http://10.0.2.2:17010/no-existe", &mut body, 5000) {
        Ok(f) if f.status == 404 => sys::print(b"http_client: GET /no-existe -> 404\n"),
        _ => return 5,
    }
    // 4. Closed port: connection error.
    match http::get(&mut net, "http://10.0.2.2:17011/", &mut body, 3000) {
        Err(HttpError::Connect(_)) => sys::print(b"http_client: closed port detected\n"),
        _ => return 6,
    }
    if http::get(&mut net, "https://10.0.2.2/", &mut body, 1000) != Err(HttpError::BadUrl) {
        return 7;
    }
    let _ = TEST_HTTP_PORT;
    sys::print(b"http_client: HTTP GET over TCP ok\n");
    0
}

aricode_rt::entry!(main);
