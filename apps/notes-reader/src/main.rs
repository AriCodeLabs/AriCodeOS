//! `notes_reader`: recibe del broker un endpoint de fsd ligado a un solo
//! fichero en modo lectura (`read_files` del manifiesto, ADR 0016). Lo lista,
//! lo lee y comprueba que no puede crear, escribir, reemplazar ni hacer
//! `flush`, y que no ve otros ficheros.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, kind, permissions};

fn denied<T>(r: Result<T, FsError>) -> bool {
    matches!(r, Err(FsError::Status(Status::Denied)))
}

fn main(_arg: u64) -> u64 {
    let (mut shared, mut hardware) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            shared = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (shared, hardware) else {
        sys::print(b"notes_reader: sin fichero compartido\n");
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let mut name = [0u8; 32];
    let Ok(Some((n, size))) = fs.list(0, &mut name) else {
        sys::print(b"notes_reader: el listado esta vacio\n");
        return 3;
    };
    if fs.list(1, &mut name).is_ok_and(|e| e.is_some()) {
        sys::print(b"notes_reader: veo mas de un fichero\n");
        return 4;
    }
    let Ok(shown) = core::str::from_utf8(&name[..n]) else {
        return 5;
    };
    let Ok(fd) = fs.open(shown) else {
        return 6;
    };
    let mut data = [0u8; 64];
    let Ok(read) = fs.read(fd, 0, &mut data) else {
        return 7;
    };
    let mut b = [0u8; 20];
    sys::print(b"notes_reader: ");
    sys::print(shown.as_bytes());
    sys::print(b" (");
    sys::print(aricode_rt::fmt_u64(size, &mut b));
    sys::print(b" bytes): ");
    sys::print(&data[..read]);
    if !denied(fs.create("otro"))
        || !denied(fs.write(fd, 0, b"x"))
        || !denied(fs.replace(fd, b"x"))
        || !denied(fs.flush())
    {
        sys::print(b"notes_reader: pude escribir en un fichero de solo lectura\n");
        return 8;
    }
    if !matches!(fs.open("README"), Err(FsError::Status(Status::NotFound))) {
        sys::print(b"notes_reader: pude abrir un fichero no compartido\n");
        return 9;
    }
    if fs.close(fd).is_err() {
        return 10;
    }
    sys::print(
        b"notes_reader: solo lectura respetada (Denied en create, write, replace y flush; README invisible)\n",
    );
    0
}

aricode_rt::entry!(main);
