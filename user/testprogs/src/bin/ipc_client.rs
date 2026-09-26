//! Cliente de ping-pong y casos negativos del IPC. Handle 0x10004 = endpoint
//! del servidor, solo con WRITE. Exito = 0; si no, el paso que fallo.
#![no_std]
#![no_main]

use user_testprogs::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);
const ROUND_TRIPS: usize = 1000;

fn main() -> u64 {
    let mut buf = [0u8; 16];
    let mut none: [Handle; 0] = [];
    // 1. Sin READ no se puede recibir del endpoint del servidor.
    if sys::ipc_receive(EP, &mut buf, &mut none, 0) != Err(Error::Denied) {
        return 1;
    }
    // 2. Endpoint de respuesta propio; al servidor se le transfiere un
    //    duplicado con WRITE|TRANSFER en cada mensaje.
    let Ok(reply) = sys::endpoint_create() else {
        return 2;
    };
    for _ in 0..ROUND_TRIPS {
        let Ok(w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) else {
            return 3;
        };
        if sys::ipc_send(EP, b"ping", &[w], TIMEOUT_INFINITE).is_err() {
            return 4;
        }
        // 3. El handle transferido ya no es nuestro.
        if sys::object_kind(w) != Err(Error::BadHandle) {
            return 5;
        }
        match sys::ipc_receive(reply, &mut buf, &mut none, TIMEOUT_INFINITE) {
            Ok((4, 0)) if &buf[..4] == b"pong" => {}
            _ => return 6,
        }
    }
    // 4. Timeout: nadie escribe en `reply`.
    if sys::ipc_receive(reply, &mut buf, &mut none, 30) != Err(Error::Timeout) {
        return 7;
    }
    // 5. Cola acotada: 16 mensajes caben, el 17 con timeout 0 falla.
    for _ in 0..sys::abi::IPC_QUEUE_CAPACITY {
        if sys::ipc_send(reply, b"x", &[], 0).is_err() {
            return 8;
        }
    }
    if sys::ipc_send(reply, b"x", &[], 0) != Err(Error::Timeout) {
        return 9;
    }
    // 6. Un envio fallido no pierde capabilities: el handle sigue siendo nuestro.
    let Ok(keep) = sys::handle_duplicate(reply, rights::READ | rights::TRANSFER) else {
        return 10;
    };
    if sys::ipc_send(reply, b"x", &[keep], 0) != Err(Error::Timeout)
        || sys::object_kind(keep).is_err()
    {
        return 11;
    }
    // 7. Mensaje demasiado grande.
    let big = [0u8; sys::abi::IPC_MAX_DATA + 1];
    if sys::ipc_send(reply, &big, &[], 0) != Err(Error::TooBig) {
        return 12;
    }
    // 8. Un handle sin TRANSFER no se puede enviar.
    if sys::ipc_send(reply, b"x", &[Handle::CONSOLE], 0) != Err(Error::Denied) {
        return 13;
    }
    if sys::ipc_send(EP, b"quit", &[], TIMEOUT_INFINITE).is_err() {
        return 14;
    }
    0
}

user_testprogs::entry!(main);
