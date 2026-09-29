//! Servicio de prueba para el supervisor (etapa 1): atiende "ping" con
//! "pong" por el endpoint 0x10004; ante "die" contesta "pong" y muere por
//! fallo. Sirve para provocar reinicios deterministas y comprobar que el
//! endpoint, que es de init, sobrevive a cada generacion.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Handle, TIMEOUT_INFINITE};

const EP: Handle = Handle(0x0001_0004);

fn main(_arg: u64) -> u64 {
    let mut buf = [0u8; 16];
    let mut handles = [Handle(0); 1];
    loop {
        let Ok((n, nh)) = sys::ipc_receive(EP, &mut buf, &mut handles, TIMEOUT_INFINITE) else {
            return 1;
        };
        let die = match &buf[..n] {
            b"ping" if nh == 1 => false,
            b"die" if nh == 1 => true,
            _ => return 4,
        };
        // El endpoint de respuesta puede haber desaparecido (cliente que ya
        // no espera): no es motivo para morir.
        let _ = sys::ipc_send(handles[0], b"pong", &[], 500);
        let _ = sys::handle_close(handles[0]);
        if die {
            // SAFETY: fallo deliberado; la pagina 0 nunca esta mapeada y la
            // direccion se oculta al compilador para que emita la lectura.
            let bad = core::hint::black_box(0x40u64) as *const u64;
            return unsafe { bad.read_volatile() };
        }
    }
}

aricode_rt::entry!(main);
