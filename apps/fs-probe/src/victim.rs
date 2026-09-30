//! `fs_victim`: escribe un secreto en su directorio privado y termina sin
//! cerrar el descriptor (regresión de la auditoría e9fdf38, hallazgo 1).
//! Además reemplaza su fichero con una cadena sintética y deja un reemplazo
//! por trozos a medias al salir (auditoría 6ee19fb, hallazgos 1 y 3): nada
//! de eso debe llegar a otra aplicación ni bloquear sus reemplazos.
#![no_std]
#![no_main]

use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions};

fn main(_arg: u64) -> u64 {
    let (mut fs_ep, mut hw) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hw = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (fs_ep, hw) else {
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let fd = match fs.open("secreto").or_else(|_| fs.create("secreto")) {
        Ok(fd) => fd,
        Err(_) => return 3,
    };
    if fs.write(fd, 0, b"VICTIM_SECRET").is_err() {
        return 4;
    }
    // Un reemplazo atomico deja sus bytes en el journal compartido de fsd.
    if fs.replace(fd, b"XYNTHETIC_PRIVATE_DATA").is_err() {
        return 5;
    }
    // Y un reemplazo por trozos que nunca se compromete: el dueño se va.
    if fs.replace_chunk(fd, 0, b"abandonado").is_err() {
        return 6;
    }
    // Sin `close`: el descriptor y la transaccion quedan vivos en fsd.
    sys::print(b"fs_victim: secreto escrito, descriptor abierto al salir\n");
    0
}

aricode_rt::entry!(main);
