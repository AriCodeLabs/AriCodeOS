//! `installer` (stage 4, ADR 0018): trusted app that receives the main fsd
//! space (`install` capability) and installs or updates the signed packages
//! left in `pkgs/`: it verifies each one with the development key, compares
//! its version with the one installed at `apps/NAME.aripkg` and, if newer,
//! replaces it with a single atomic `replace`. A package that fails
//! verification or is not newer is skipped, and the installer says so.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions};

const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");
/// An installed package is replaced in one go: the AriFS journal (32 KiB).
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

/// Reads a whole file (up to `MAX_PACKAGE`) from the main space.
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

/// Version of the installed package with that name; 0 if there is none or
/// the existing one fails verification (it can be replaced).
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
        say(b"need the main file space (install capability)");
        return 1;
    };
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let mut pubkey = [0u8; 32];
    if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pubkey).is_none() {
        return 3;
    }
    // Pending packages: `pkgs/*.aripkg`, in directory order.
    let mut pending: [([u8; 32], usize, u64); MAX_PENDING] = [([0; 32], 0, 0); MAX_PENDING];
    let mut count = 0;
    let mut entry = [0u8; 32];
    let mut i = 0;
    while let Ok(Some((n, size))) = fs.list(i, &mut entry) {
        i += 1;
        let name = &entry[..n];
        if name.starts_with(b"pkgs/") && name.ends_with(aricode_pkg::EXTENSION.as_bytes()) {
            if count == MAX_PENDING {
                say(b"too many pending packages; handling the first ones");
                break;
            }
            pending[count] = (entry, n, size);
            count += 1;
        }
    }
    if count == 0 {
        say(b"nothing to install in pkgs/");
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
            say(b" cannot be read or is too large: skipped");
            ignored += 1;
            continue;
        };
        let verified = match aricode_pkg::verify(&bytes, &pubkey) {
            Ok(v) => v,
            Err(_) => {
                sys::print(b"installer: ");
                sys::print(path.as_bytes());
                sys::print(b" fails verification: skipped\n");
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
            sys::print(b" already installed (current ");
            num(current as u64);
            sys::print(b")\n");
            ignored += 1;
            continue;
        }
        sys::print(b"installer: installing ");
        sys::print(app.as_bytes());
        sys::print(b" version ");
        num(version as u64);
        sys::print(b" (from ");
        sys::print(path.as_bytes());
        sys::print(b")\n");
        let fd = match fs.open(target).or_else(|_| fs.create(target)) {
            Ok(fd) => fd,
            Err(_) => {
                say(b"could not open or create the installed package");
                return 4;
            }
        };
        // A single atomic replace: after a power cut there is either the old
        // version or the new one, never a mix.
        let result = fs.replace(fd, &bytes);
        let _ = fs.close(fd);
        if result != Ok(bytes.len()) {
            say(b"error writing the installed package");
            return 5;
        }
        sys::print(b"installer: ");
        sys::print(app.as_bytes());
        sys::print(b" ");
        num(current as u64);
        sys::print(b" -> ");
        num(version as u64);
        sys::print(b" installed (");
        sys::print(target.as_bytes());
        sys::print(b")\n");
        installed += 1;
    }
    sys::print(b"installer: ");
    num(installed);
    sys::print(b" package(s) installed, ");
    num(ignored);
    sys::print(b" skipped\n");
    0
}

aricode_rt::entry!(main);
