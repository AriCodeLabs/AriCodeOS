//! `fs_probe`: desde otro directorio privado intenta leer, escribir y cerrar
//! los descriptores 0..16 sin haber abierto nada. fsd debe responder `BadFd`
//! a todos: los descriptores pertenecen al endpoint que los abrió. Después
//! (auditoría 6ee19fb) comprueba que un compromiso que no cubre sus propios
//! trozos se rechaza sin filtrar restos del journal de `fs_victim`, y que la
//! transacción que `fs_victim` dejó a medias caduca y no bloquea la suya.
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
    if fs.write(own, 0, b"ok").is_err() {
        return 8;
    }
    sys::print(
        b"fs_probe: descriptores ajenos inaccesibles (BadFd en 16 x lectura, escritura y cierre)\n",
    );
    // fs_victim dejo un reemplazo a medias: durante un tiempo el journal esta
    // ocupado (Busy); luego caduca y este reemplazo debe poder hacerse.
    let mut tries = 0;
    let mut waited = false;
    loop {
        match fs.replace(own, b"reemplazo propio") {
            Ok(_) => break,
            Err(FsError::Status(Status::Busy)) if tries < 60 => {
                waited = true;
                tries += 1;
                sys::thread_sleep(100);
            }
            Err(_) => return 9,
        }
    }
    if waited {
        sys::print(b"fs_probe: reemplazo tras una transaccion abandonada ok\n");
    }
    // Un trozo de un byte y un compromiso de 22: fsd no puede completar los
    // bytes con lo que fs_victim dejo en el journal.
    if fs.replace_chunk(own, 0, b"Y").is_err() {
        return 10;
    }
    match fs.replace_commit(own, 22) {
        Err(FsError::Status(Status::Invalid)) => {}
        Ok(_) => {
            sys::print(b"fs_probe: COMPROMISO INCOMPLETO ACEPTADO\n");
            return 11;
        }
        Err(_) => return 12,
    }
    let n = match fs.read(own, 0, &mut buf) {
        Ok(n) => n,
        Err(_) => return 13,
    };
    if buf[..n].windows(9).any(|w| w == b"YNTHETIC_") {
        sys::print(b"fs_probe: RESTOS DEL JOURNAL AJENO: ");
        sys::print(&buf[..n]);
        sys::print(b"\n");
        return 14;
    }
    if &buf[..n] != b"reemplazo propio" {
        return 15;
    }
    let _ = fs.close(own);
    sys::print(b"fs_probe: el journal no filtra datos ajenos (compromiso incompleto rechazado)\n");
    0
}

aricode_rt::entry!(main);
