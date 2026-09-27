//! `hello_app`: la aplicación de ejemplo del SDK (Fase 27). Muestra lo que
//! todo programa tiene al arrancar: una consola, su propio proceso e hilo y
//! los handles extra que le haya dado quien lo lanzó. Si recibe un `SYSTEM`
//! imprime también el tiempo de arranque.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, kind, permissions, time};
use aricode_rt::{fmt_hex, fmt_u64};

fn main(arg: u64) -> u64 {
    sys::print(b"hello_app: hola desde una aplicacion del SDK (arg ");
    let mut buf = [0u8; 20];
    sys::print(fmt_u64(arg, &mut buf));
    sys::print(b")\n");

    sys::print(b"hello_app: capacidades:");
    let mut count = 0;
    let mut system = None;
    permissions::granted(|h, name| {
        sys::print(b" ");
        sys::print(name.as_bytes());
        sys::print(b"(0x");
        let mut hex = [0u8; 16];
        sys::print(fmt_hex(h.0 as u64, 5, &mut hex));
        sys::print(b")");
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        }
        count += 1;
    });
    sys::print(b"\n");
    sys::print(b"hello_app: ");
    sys::print(fmt_u64(count, &mut buf));
    sys::print(b" handles\n");

    match system {
        Some(h) => match time::uptime_ms(h) {
            Ok(ms) => {
                sys::print(b"hello_app: uptime ");
                sys::print(fmt_u64(ms / 1000, &mut buf));
                sys::print(b" s\n");
            }
            Err(e) => aricode_rt::print_error(b"hello_app: uptime", e),
        },
        None => sys::print(b"hello_app: sin acceso al reloj del sistema (no me dieron SYSTEM)\n"),
    }
    0
}

aricode_rt::entry!(main);
