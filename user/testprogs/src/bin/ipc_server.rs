//! Servidor de ping-pong: recibe "ping" con un handle de respuesta, contesta
//! "pong" por el y lo cierra; con "quit" termina. Handle 0x10004 = endpoint.
#![no_std]
#![no_main]

use user_testprogs::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);

fn main() -> u64 {
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 1];
    loop {
        let Ok((n, nh)) = sys::ipc_receive(EP, &mut buf, &mut handles, TIMEOUT_INFINITE) else {
            return 1;
        };
        match &buf[..n] {
            b"ping" if nh == 1 => {
                if sys::ipc_send(handles[0], b"pong", &[], TIMEOUT_INFINITE).is_err() {
                    return 2;
                }
                if sys::handle_close(handles[0]).is_err() {
                    return 3;
                }
            }
            b"quit" => return 0,
            _ => return 4,
        }
    }
}

user_testprogs::entry!(main);
