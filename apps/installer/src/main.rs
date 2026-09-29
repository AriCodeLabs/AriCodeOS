//! `installer` (etapa 4, ADR 0018): aplicación de confianza que recibe el
//! espacio principal de fsd (capacidad `install`) e instala o actualiza los
//! paquetes firmados dejados en `pkgs/`: verifica cada uno con la clave de
//! desarrollo, compara su versión con la instalada en `apps/NOMBRE.aripkg`
//! y, si es más nueva, la sustituye con un único `replace` atómico. Un
//! paquete que no verifica o que no es más nuevo se ignora y se dice.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions};

const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");
/// Un paquete instalado se sustituye de una vez: el journal de AriFS (32 KiB).
const MAX_PACKAGE: u64 = 32 * 1024;
const MAX_PENDING: usize = 8;

fn say(msg: &[u8]) {
    sys::print(b"installer: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(v, &mut b));
}

/// Lee un fichero entero (hasta `MAX_PACKAGE`) del espacio principal.
fn read_all(fs: &mut FsClient, name: &str, size: u64) -> Option<Vec<u8>> {
    if size == 0 || size > MAX_PACKAGE {
        return None;
    }
    let fd = fs.open(name).ok()?;
    let mut data = alloc::vec![0u8; size as usize];
    let mut done = 0;
    while done < data.len() {
        match fs.read(fd, done as u32, &mut data[done..]) {
            Ok(n) if n > 0 => done += n,
            _ => {
                let _ = fs.close(fd);
                return None;
            }
        }
    }
    let _ = fs.close(fd);
    Some(data)
}

/// Versión del paquete instalado con ese nombre; 0 si no hay ninguno o el
/// que hay no verifica (se puede sustituir).
fn installed_version(fs: &mut FsClient, path: &str, pubkey: &[u8; 32]) -> u32 {
    let mut entry = [0u8; 32];
    let mut i = 0;
    let mut size = None;
    while let Ok(Some((n, s))) = fs.list(i, &mut entry) {
        if &entry[..n] == path.as_bytes() {
            size = Some(s);
            break;
        }
        i += 1;
    }
    let Some(size) = size else {
        return 0;
    };
    read_all(fs, path, size)
        .and_then(|bytes| {
            aricode_pkg::verify(&bytes, pubkey)
                .ok()
                .map(|v| v.manifest.version)
        })
        .unwrap_or(0)
}

fn main(_arg: u64) -> u64 {
    let (mut root, mut hardware) = (None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::ENDPOINT) {
            root = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(ep), Some(hw)) = (root, hardware) else {
        say(b"necesito el espacio principal de ficheros (capacidad install)");
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let mut pubkey = [0u8; 32];
    if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pubkey).is_none() {
        return 3;
    }
    // Paquetes pendientes: `pkgs/*.aripkg`, en el orden del directorio.
    let mut pending: [([u8; 32], usize, u64); MAX_PENDING] = [([0; 32], 0, 0); MAX_PENDING];
    let mut count = 0;
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, size))) = fs.list(i, &mut entry) {
        i += 1;
        let name = &entry[..n];
        if name.starts_with(b"pkgs/") && name.ends_with(aricode_pkg::EXTENSION.as_bytes()) {
            if count == MAX_PENDING {
                say(b"demasiados paquetes pendientes; se atienden los primeros");
                break;
            }
            pending[count] = (entry, n, size);
            count += 1;
        }
    }
    if count == 0 {
        say(b"nada que instalar en pkgs/");
        return 0;
    }
    let (mut installed, mut ignored) = (0u64, 0u64);
    for (name, n, size) in &pending[..count] {
        let Ok(path) = core::str::from_utf8(&name[..*n]) else {
            continue;
        };
        let Some(bytes) = read_all(&mut fs, path, *size) else {
            sys::print(b"installer: ");
            sys::print(path.as_bytes());
            say(b" no se puede leer o es demasiado grande: se ignora");
            ignored += 1;
            continue;
        };
        let verified = match aricode_pkg::verify(&bytes, &pubkey) {
            Ok(v) => v,
            Err(_) => {
                sys::print(b"installer: ");
                sys::print(path.as_bytes());
                sys::print(b" no verifica: se ignora\n");
                ignored += 1;
                continue;
            }
        };
        let app = verified.manifest.name_str();
        let version = verified.manifest.version;
        let mut target = [0u8; 48];
        let prefix = b"apps/";
        target[..prefix.len()].copy_from_slice(prefix);
        target[prefix.len()..prefix.len() + app.len()].copy_from_slice(app.as_bytes());
        let ext = aricode_pkg::EXTENSION.as_bytes();
        let total = prefix.len() + app.len() + ext.len();
        target[prefix.len() + app.len()..total].copy_from_slice(ext);
        let Ok(target) = core::str::from_utf8(&target[..total]) else {
            continue;
        };
        let current = installed_version(&mut fs, target, &pubkey);
        if version <= current {
            sys::print(b"installer: ");
            sys::print(app.as_bytes());
            sys::print(b" version ");
            num(version as u64);
            sys::print(b" ya instalada (actual ");
            num(current as u64);
            sys::print(b")\n");
            ignored += 1;
            continue;
        }
        sys::print(b"installer: instalando ");
        sys::print(app.as_bytes());
        sys::print(b" version ");
        num(version as u64);
        sys::print(b" (desde ");
        sys::print(path.as_bytes());
        sys::print(b")\n");
        let fd = match fs.open(target).or_else(|_| fs.create(target)) {
            Ok(fd) => fd,
            Err(_) => {
                say(b"no se pudo abrir ni crear el paquete instalado");
                return 4;
            }
        };
        // Un solo reemplazo atomico: tras un corte hay la version anterior o
        // la nueva, nunca una mezcla.
        let result = fs.replace(fd, &bytes);
        let _ = fs.close(fd);
        if result != Ok(bytes.len()) {
            say(b"error al escribir el paquete instalado");
            return 5;
        }
        sys::print(b"installer: ");
        sys::print(app.as_bytes());
        sys::print(b" ");
        num(current as u64);
        sys::print(b" -> ");
        num(version as u64);
        sys::print(b" instalado (");
        sys::print(target.as_bytes());
        sys::print(b")\n");
        installed += 1;
    }
    sys::print(b"installer: ");
    num(installed);
    sys::print(b" paquete(s) instalado(s), ");
    num(ignored);
    sys::print(b" ignorado(s)\n");
    0
}

aricode_rt::entry!(main);
