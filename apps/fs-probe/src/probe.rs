//! `fs_probe`: desde otro directorio privado intenta leer, escribir y cerrar
//! los descriptores 0..16 sin haber abierto nada. fsd debe responder `BadFd`
//! a todos: los descriptores pertenecen al endpoint que los abrió.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
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
    let mut buf = [0u8; 32];
    for fd in 0..16u32 {
        match fs.read(fd, 0, &mut buf) {
            Ok(n) => {
                sys::print(b"fs_probe: ACCESO CRUZADO en lectura: ");
                sys::print(&buf[..n]);
                sys::print(b"\n");
                return 3;
            }
            Err(FsError::Status(Status::BadFd)) => {}
            Err(_) => return 4,
        }
        if !matches!(fs.write(fd, 0, b"X"), Err(FsError::Status(Status::BadFd))) {
            sys::print(b"fs_probe: ACCESO CRUZADO en escritura\n");
            return 5;
        }
        if !matches!(fs.close(fd), Err(FsError::Status(Status::BadFd))) {
            sys::print(b"fs_probe: ACCESO CRUZADO en cierre\n");
            return 6;
        }
    }
    // Y su propio directorio sigue funcionando.
    let Ok(own) = fs.create("mio") else {
        return 7;
    };
    if fs.write(own, 0, b"ok").is_err() || fs.close(own).is_err() {
        return 8;
    }
    sys::print(
        b"fs_probe: descriptores ajenos inaccesibles (BadFd en 16 x lectura, escritura y cierre)\n",
    );
    0
}

aricode_rt::entry!(main);
