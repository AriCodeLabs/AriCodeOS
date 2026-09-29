//! `hello_ext`: aplicación construida fuera del árbol de AriCodeOS con el SDK
//! por ruta, empaquetada y firmada (`cargo xtask app-pack`) e instalada en
//! AriFS como `apps/hello_ext.aripkg` en vez de en el initrd (etapa 4, ADR 0017). El broker la lanza igual que a
//! las del initrd y le entrega solo lo que pide su manifiesto.
#![no_std]
#![no_main]

use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time};

/// Versión que xtask fija al empaquetar (`APP_VERSION`); "dev" si se
/// construye a mano.
const VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => "dev",
};

fn main(arg: u64) -> u64 {
    let mut b = [0u8; 20];
    sys::print(b"hello_ext v");
    sys::print(VERSION.as_bytes());
    sys::print(b": hola desde fuera del arbol (arg ");
    sys::print(aricode_rt::fmt_u64(arg, &mut b));
    sys::print(b")\n");
    let (mut system, mut fs_ep, mut hardware) = (None, None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    if let Some(h) = system {
        if let Ok(ms) = time::uptime_ms(h) {
            sys::print(b"hello_ext: uptime ");
            sys::print(aricode_rt::fmt_u64(ms / 1000, &mut b));
            sys::print(b" s\n");
        }
    }
    let (Some(ep), Some(hw)) = (fs_ep, hardware) else {
        sys::print(b"hello_ext: sin directorio privado\n");
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let fd = match fs.open("estado").or_else(|_| fs.create("estado")) {
        Ok(fd) => fd,
        Err(_) => return 3,
    };
    if fs.replace(fd, b"instalada y ejecutada").is_err() {
        return 4;
    }
    let mut data = [0u8; 32];
    if fs.read(fd, 0, &mut data) != Ok(21) || &data[..21] != b"instalada y ejecutada" {
        return 5;
    }
    let _ = fs.close(fd);
    sys::print(b"hello_ext: fichero privado 'estado' escrito y leido\n");
    0
}

aricode_rt::entry!(main);
