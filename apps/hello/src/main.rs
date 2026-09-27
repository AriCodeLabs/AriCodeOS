//! `hello_app`: la aplicación de ejemplo del SDK (Fases 27 y 28). Recibe del
//! broker solo lo que pide su manifiesto (`system`, `fs`): un reloj de solo
//! lectura, un directorio privado en fsd y la capacidad de hardware para su
//! buffer, acotada por la memoria del manifiesto.
#![no_std]
#![no_main]

use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time};
use aricode_rt::{fmt_hex, fmt_u64};

fn main(arg: u64) -> u64 {
    sys::print(b"hello_app: hola desde una aplicacion del SDK (arg ");
    let mut buf = [0u8; 20];
    sys::print(fmt_u64(arg, &mut buf));
    sys::print(b")\n");

    // 1. Lo que me han dado.
    sys::print(b"hello_app: capacidades:");
    let mut count = 0;
    let (mut system, mut fs_ep, mut hardware) = (None, None, None);
    permissions::granted(|h, name| {
        sys::print(b" ");
        sys::print(name.as_bytes());
        sys::print(b"(0x");
        let mut hex = [0u8; 16];
        sys::print(fmt_hex(h.0 as u64, 5, &mut hex));
        sys::print(b")");
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
        count += 1;
    });
    sys::print(b"\nhello_app: ");
    sys::print(fmt_u64(count, &mut buf));
    sys::print(b" handles\n");

    // 2. El reloj, si lo tengo.
    match system {
        Some(h) => match time::uptime_ms(h) {
            Ok(ms) => {
                sys::print(b"hello_app: uptime ");
                sys::print(fmt_u64(ms / 1000, &mut buf));
                sys::print(b" s\n");
            }
            Err(e) => aricode_rt::print_error(b"hello_app: uptime", e),
        },
        None => sys::print(b"hello_app: sin acceso al reloj del sistema\n"),
    }

    // 3. Directorio privado: escribir, leer y listar (solo veo lo mio).
    let (Some(ep), Some(hw)) = (fs_ep, hardware) else {
        sys::print(b"hello_app: sin directorio privado (el manifiesto no pide fs)\n");
        return 0;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 1;
    };
    let fd = match fs.open("notas").or_else(|_| fs.create("notas")) {
        Ok(fd) => fd,
        Err(_) => return 2,
    };
    if fs.write(fd, 0, b"privado").is_err() {
        return 3;
    }
    let mut data = [0u8; 16];
    if fs.read(fd, 0, &mut data) != Ok(7) || &data[..7] != b"privado" {
        return 4;
    }
    let _ = fs.close(fd);
    sys::print(b"hello_app: fichero privado 'notas' escrito y leido\n");
    sys::print(b"hello_app: ls privado:");
    let mut name = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, _))) = fs.list(i, &mut name) {
        sys::print(b" ");
        sys::print(&name[..n]);
        if &name[..n] == b"hola.txt" {
            sys::print(b"\nhello_app: veo ficheros ajenos!\n");
            return 5;
        }
        i += 1;
    }
    sys::print(b"\n");
    if fs.open("hola.txt").is_ok() {
        sys::print(b"hello_app: pude abrir un fichero ajeno!\n");
        return 6;
    }

    // 4. Cuota de memoria: mas de lo que dice el manifiesto -> Limit.
    match sys::dma_alloc(hw, 256 * 1024) {
        Err(sys::Error::Limit) => sys::print(b"hello_app: cuota de memoria respetada (Limit)\n"),
        other => {
            sys::print(b"hello_app: la cuota no se aplico\n");
            let _ = other;
            return 7;
        }
    }
    0
}

aricode_rt::entry!(main);
