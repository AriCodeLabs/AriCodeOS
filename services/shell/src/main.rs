//! arish (Fase 21): shell interactivo en ring 3. Solo ve lo que init le da:
//! 0x10004 = endpoint de fsd (WRITE), 0x10005 = HARDWARE (buffer DMA),
//! 0x10006 = SYSTEM (READ|MANAGE), 0x10007 = entrada de consola (READ),
//! 0x10008 = raíz PCI (READ). `arg` = máscara de servicios en ejecución.
#![no_std]
#![no_main]

use user_testprogs::sys::{self, abi, fs::FsClient, Handle, TIMEOUT_INFINITE};
use user_testprogs::{fmt_hex, fmt_u64};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const SYSTEM: Handle = Handle(0x0001_0006);
const CONSOLE_IN: Handle = Handle(0x0001_0007);
const PCI_ROOT: Handle = Handle(0x0001_0008);

/// Bits de `arg`: servicios que init dejó en marcha.
const SERVICE_BLOCK: u64 = 1;
const SERVICE_FS: u64 = 2;

const PROMPT: &[u8] = b"ari> ";
const LINE_MAX: usize = 256;

fn num(v: u64) {
    let mut buf = [0u8; 20];
    sys::print(fmt_u64(v, &mut buf));
}

fn hex(v: u64, width: usize) {
    let mut buf = [0u8; 16];
    sys::print(fmt_hex(v, width, &mut buf));
}

/// Lee una línea con eco, retroceso y fin en CR o LF.
fn read_line(line: &mut [u8; LINE_MAX]) -> usize {
    let mut len = 0;
    let mut chunk = [0u8; 64];
    loop {
        let Ok(n) = sys::console_read(CONSOLE_IN, &mut chunk, TIMEOUT_INFINITE) else {
            continue;
        };
        for &b in &chunk[..n] {
            match b {
                b'\r' | b'\n' => {
                    sys::print(b"\n");
                    return len;
                }
                0x08 | 0x7F => {
                    if len > 0 {
                        len -= 1;
                        sys::print(b"\x08 \x08");
                    }
                }
                0x20..=0x7E if len < LINE_MAX => {
                    line[len] = b;
                    len += 1;
                    sys::print(&[b]);
                }
                _ => {}
            }
        }
    }
}

/// Divide en (comando, resto) por el primer espacio.
fn split(line: &[u8]) -> (&[u8], &[u8]) {
    let line = trim(line);
    match line.iter().position(|&b| b == b' ') {
        Some(i) => (&line[..i], trim(&line[i + 1..])),
        None => (line, &[]),
    }
}

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
    let end = s.iter().rposition(|&b| b != b' ').map_or(start, |e| e + 1);
    &s[start..end]
}

fn help() {
    sys::print(
        b"comandos:\n  help      esta ayuda\n  ps        procesos\n  mem       memoria fisica\n  ls        ficheros de AriFS\n  cat NOM   muestra un fichero\n  devices   funciones PCI\n  services  servicios de arranque\n  uptime    tiempo desde el arranque\n  reboot    reinicia\n  shutdown  apaga\n  exit      termina el shell\n",
    );
}

fn ps() {
    use abi::sysinfo::{ProcessEntry, ProcessState};
    let mut buf = [0u8; ProcessEntry::LEN * 32];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::PROCESSES, &mut buf) else {
        sys::print(b"ps: sin acceso al sistema\n");
        return;
    };
    sys::print(b"PID  ESTADO      NOMBRE\n");
    for chunk in buf[..n].as_chunks::<{ ProcessEntry::LEN }>().0 {
        let Some(e) = ProcessEntry::decode(chunk) else {
            continue;
        };
        let mut b = [0u8; 20];
        let pid = fmt_u64(e.pid as u64, &mut b);
        sys::print(pid);
        for _ in pid.len()..5 {
            sys::print(b" ");
        }
        match e.state {
            ProcessState::Running => sys::print(b"activo      "),
            ProcessState::Exited => {
                sys::print(b"salio(");
                num(e.exit_code as u64);
                sys::print(b")");
                for _ in fmt_u64(e.exit_code as u64, &mut b).len()..5 {
                    sys::print(b" ");
                }
            }
            ProcessState::Killed => sys::print(b"terminado   "),
        }
        sys::print(e.name_str().as_bytes());
        sys::print(b"\n");
    }
}

fn mem() {
    let mut buf = [0u8; 16];
    if sys::system_info(SYSTEM, abi::sysinfo::MEMORY, &mut buf) != Ok(16) {
        sys::print(b"mem: sin acceso al sistema\n");
        return;
    }
    let total = u64::from_le_bytes(buf[..8].try_into().unwrap());
    let free = u64::from_le_bytes(buf[8..].try_into().unwrap());
    sys::print(b"memoria fisica utilizable: ");
    num(total * 4 / 1024);
    sys::print(b" MiB total, ");
    num(free * 4 / 1024);
    sys::print(b" MiB libres, ");
    num((total - free) * 4 / 1024);
    sys::print(b" MiB en uso (");
    num(total);
    sys::print(b" frames de 4 KiB)\n");
}

fn uptime() {
    let mut buf = [0u8; 8];
    if sys::system_info(SYSTEM, abi::sysinfo::UPTIME, &mut buf) != Ok(8) {
        sys::print(b"uptime: sin acceso al sistema\n");
        return;
    }
    let ms = u64::from_le_bytes(buf);
    sys::print(b"uptime: ");
    num(ms / 1000);
    sys::print(b".");
    let frac = (ms % 1000) / 10;
    if frac < 10 {
        sys::print(b"0");
    }
    num(frac);
    sys::print(b" s\n");
}

fn ls(fs: &FsClient) {
    let mut name = [0u8; 32];
    let mut i = 0;
    loop {
        match fs.list(i, &mut name) {
            Ok(Some((n, size))) => {
                sys::print(&name[..n]);
                for _ in n..20 {
                    sys::print(b" ");
                }
                num(size);
                sys::print(b" bytes\n");
                i += 1;
            }
            Ok(None) => break,
            Err(_) => {
                sys::print(b"ls: el servicio de ficheros no responde\n");
                return;
            }
        }
    }
    num(i as u64);
    sys::print(b" fichero(s)\n");
}

fn cat(fs: &mut FsClient, name: &[u8]) {
    let Ok(name) = core::str::from_utf8(name) else {
        sys::print(b"cat: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        sys::print(b"uso: cat NOMBRE\n");
        return;
    }
    let fd = match fs.open(name) {
        Ok(fd) => fd,
        Err(_) => {
            sys::print(b"cat: no existe ");
            sys::print(name.as_bytes());
            sys::print(b"\n");
            return;
        }
    };
    let mut buf = [0u8; 1024];
    let mut offset = 0u32;
    loop {
        match fs.read(fd, offset, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                sys::print(&buf[..n]);
                offset += n as u32;
            }
            Err(_) => {
                sys::print(b"cat: error de lectura\n");
                break;
            }
        }
    }
    let _ = fs.close(fd);
}

fn devices() {
    let Ok(n) = sys::pci_count(PCI_ROOT) else {
        sys::print(b"devices: sin acceso a PCI\n");
        return;
    };
    for i in 0..n {
        let Ok(v) = sys::pci_info(PCI_ROOT, i) else {
            continue;
        };
        hex(abi::pci_info::bus(v) as u64, 2);
        sys::print(b":");
        hex(abi::pci_info::dev(v) as u64, 2);
        sys::print(b".");
        num(abi::pci_info::func(v) as u64);
        sys::print(b"  ");
        hex(abi::pci_info::vendor(v) as u64, 4);
        sys::print(b":");
        hex(abi::pci_info::device(v) as u64, 4);
        sys::print(b"  clase ");
        hex(abi::pci_info::class(v) as u64, 2);
        sys::print(b".");
        hex(abi::pci_info::subclass(v) as u64, 2);
        sys::print(b"\n");
    }
    num(n);
    sys::print(b" funcion(es) PCI\n");
}

fn services(mask: u64, fs: &FsClient) {
    sys::print(b"bloques   virtio_blk  ");
    sys::print(if mask & SERVICE_BLOCK != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    sys::print(b"ficheros  fsd         ");
    if mask & SERVICE_FS == 0 {
        sys::print(b"ausente\n");
    } else {
        let mut name = [0u8; 32];
        sys::print(if fs.list(0, &mut name).is_ok() {
            b"activo, responde\n"
        } else {
            b"activo, no responde\n"
        });
    }
}

fn main(mask: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        sys::print(b"arish: sin cliente de ficheros\n");
        return 1;
    };
    sys::print(b"arish: shell de AriCodeOS (help para ayuda)\n");
    let mut line = [0u8; LINE_MAX];
    loop {
        sys::print(PROMPT);
        let len = read_line(&mut line);
        let (cmd, rest) = split(&line[..len]);
        match cmd {
            b"" => {}
            b"help" => help(),
            b"ps" => ps(),
            b"mem" => mem(),
            b"ls" => ls(&fs),
            b"cat" => cat(&mut fs, rest),
            b"devices" => devices(),
            b"services" => services(mask, &fs),
            b"uptime" => uptime(),
            b"reboot" => {
                user_testprogs::print_error(
                    b"reboot",
                    sys::system_power(SYSTEM, abi::power::REBOOT),
                );
            }
            b"shutdown" => {
                user_testprogs::print_error(
                    b"shutdown",
                    sys::system_power(SYSTEM, abi::power::SHUTDOWN),
                );
            }
            b"exit" => return 0,
            other => {
                sys::print(b"comando desconocido: ");
                sys::print(other);
                sys::print(b" (help)\n");
            }
        }
    }
}

user_testprogs::entry!(main);
