//! Cliente del servicio de ficheros (Fase 20): lista, abre, lee, crea,
//! escribe, reemplaza, hace flush y relee por IPC. Handles: 0x10004 = fsd (WRITE), 0x10005 = HARDWARE.
#![no_std]
#![no_main]

use aricode_proto::fs::Status;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, Handle};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);

fn main(_arg: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        return 1;
    };
    // 1. Listado.
    let mut name = [0u8; 32];
    let mut count = 0;
    let mut saw_hola = false;
    let mut i = 0;
    while let Ok(Some((n, size))) = fs.list(i, &mut name) {
        sys::print(b"fs_client: ");
        sys::print(&name[..n]);
        sys::print(b" (");
        let mut buf = [0u8; 20];
        sys::print(aricode_rt::fmt_u64(size, &mut buf));
        sys::print(b" bytes)\n");
        saw_hola |= &name[..n] == b"hola.txt";
        count += 1;
        i += 1;
    }
    if count < 2 || !saw_hola {
        return 2;
    }
    // 2. Leer hola.txt.
    let Ok(fd) = fs.open("hola.txt") else {
        return 3;
    };
    let mut data = [0u8; 64];
    let n = match fs.read(fd, 0, &mut data) {
        Ok(n) => n,
        Err(_) => return 4,
    };
    if &data[..n] != b"Hola desde AriFS\n" {
        return 5;
    }
    sys::print(b"fs_client: hola.txt dice: ");
    sys::print(&data[..n]);
    let _ = fs.close(fd);
    // 3. Crear, escribir, releer. Con un disco que ya arranco antes el
    //    fichero existe: se abre y se sobrescribe.
    let fd = match fs.create("nuevo.txt") {
        Ok(fd) => fd,
        Err(FsError::Status(Status::Exists)) => {
            sys::print(b"fs_client: nuevo.txt ya existia (disco persistente)\n");
            let Ok(fd) = fs.open("nuevo.txt") else {
                return 6;
            };
            // Vaciarlo para que las comprobaciones siguientes vean lo mismo
            // que en un disco recien formateado.
            if fs.replace(fd, b"").is_err() {
                return 6;
            }
            fd
        }
        Err(_) => return 6,
    };
    if fs.write(fd, 0, b"escrito desde ring 3") != Ok(20) {
        return 7;
    }
    if fs.read(fd, 8, &mut data) != Ok(12) || &data[..12] != b"desde ring 3" {
        return 8;
    }
    // 4. Reemplazo atomico (ADR 0014): el contenido entero cambia de una vez
    //    y queda duradero; flush explicito de las escrituras anteriores.
    if fs.replace(fd, b"contenido reemplazado de una vez") != Ok(32) {
        return 12;
    }
    if fs.read(fd, 0, &mut data) != Ok(32) || &data[..32] != b"contenido reemplazado de una vez" {
        return 13;
    }
    if fs.flush().is_err() {
        return 14;
    }
    sys::print(b"fs_client: reemplazo atomico y flush ok\n");
    // 5. Errores: crear duplicado, abrir inexistente, fd cerrado.
    if fs.create("nuevo.txt").is_ok() {
        return 9;
    }
    if fs.open("no-existe").is_ok() {
        return 10;
    }
    if fs.close(fd).is_err() || fs.read(fd, 0, &mut data).is_ok() || fs.replace(fd, b"x").is_ok() {
        return 11;
    }
    sys::print(b"fs_client: listar, leer, crear y escribir por IPC ok\n");
    0
}

aricode_rt::entry!(main);
