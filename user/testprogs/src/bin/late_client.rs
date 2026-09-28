//! Cliente de la prueba de respuestas tardias (etapa 1B): la primera
//! peticion vence su plazo antes de que el servidor conteste; la segunda
//! debe recibir SU respuesta ("fresh"), nunca la tardia de la primera.
//! Handle 0x10004 = endpoint del servidor (WRITE).
#![no_std]
#![no_main]

use aricode_rt::sys::{self, rpc, Error, Handle};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let mut out = [0u8; 16];
    // 1. Vence el plazo (200 ms) antes de la respuesta (800 ms).
    match rpc::call(EP, b"one", &[], 500, 200, &mut out) {
        Err(Error::Timeout) => {}
        Ok(n) => {
            sys::print(b"late_client: la primera peticion no debia responder: ");
            sys::print(&out[..n]);
            sys::print(b"\n");
            return 2;
        }
        Err(_) => return 3,
    }
    // 2. Otra peticion mientras la respuesta tardia esta en camino: con un
    // endpoint de respuesta nuevo por peticion, la tardia se pierde en el
    // endpoint cerrado y esta recibe la suya.
    match rpc::call(EP, b"two", &[], 500, 5000, &mut out) {
        Ok(n) if &out[..n] == b"fresh" => {}
        Ok(n) => {
            sys::print(b"late_client: respuesta confundida: ");
            sys::print(&out[..n]);
            sys::print(b"\n");
            return 4;
        }
        Err(_) => return 5,
    }
    // 3. Y despues de que la tardia haya llegado (y se haya descartado), otra mas.
    sys::thread_sleep(900);
    match rpc::call(EP, b"three", &[], 500, 5000, &mut out) {
        Ok(n) if &out[..n] == b"fresh" => {}
        _ => return 6,
    }
    let _ = sys::ipc_send(EP, b"quit", &[], 500);
    sys::print(
        b"late_client: plazo vencido, respuesta tardia descartada, respuestas correlacionadas ok\n",
    );
    0
}

aricode_rt::entry!(main);
