//! `fs_victim`: escribe un secreto en su directorio privado y termina sin
//! cerrar el descriptor (regresión de la auditoría e9fdf38, hallazgo 1).
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
    // Sin `close`: el descriptor queda vivo en fsd.
    sys::print(b"fs_victim: secreto escrito, descriptor abierto al salir\n");
    0
}

aricode_rt::entry!(main);
