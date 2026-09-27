//! arish (Fase 21): shell interactivo en ring 3. Solo ve lo que init le da:
//! 0x10004 = endpoint de fsd (WRITE), 0x10005 = HARDWARE (buffer DMA),
//! 0x10006 = SYSTEM (READ|MANAGE), 0x10007 = entrada de consola (READ),
//! 0x10008 = raíz PCI (READ), 0x10009 = endpoint de netd (WRITE), 0x1000A =
//! SPAWNER (MANAGE) para `run`. `arg` = máscara de servicios en ejecución.
#![no_std]
#![no_main]

use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, abi, fs::FsClient, Handle, TIMEOUT_INFINITE};
use aricode_rt::{fmt_hex, fmt_u64};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const SYSTEM: Handle = Handle(0x0001_0006);
const CONSOLE_IN: Handle = Handle(0x0001_0007);
const PCI_ROOT: Handle = Handle(0x0001_0008);
const NETD: Handle = Handle(0x0001_0009);
const SPAWNER: Handle = Handle(0x0001_000A);

/// Bits de `arg`: servicios que init dejó en marcha.
const SERVICE_BLOCK: u64 = 1;
const SERVICE_FS: u64 = 2;
const SERVICE_NET: u64 = 4;
const SERVICE_INPUT: u64 = 8;
const SERVICE_GUI: u64 = 16;

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
        b"comandos:\n  help      esta ayuda\n  ps        procesos\n  mem       memoria fisica\n  ls        ficheros de AriFS\n  cat NOM   muestra un fichero\n  devices   funciones PCI\n  services  servicios de arranque\n  uptime    tiempo desde el arranque\n  net       configuracion de red\n  ping IP   eco ICMP (4 intentos)\n  dns NOM   resuelve un nombre\n  http URL  GET y muestra el cuerpo\n  run PROG  ejecuta un programa del initrd y espera\n  reboot    reinicia\n  shutdown  apaga\n  exit      termina el shell\n",
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

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    sys::print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

fn net_error(e: NetError) {
    match e {
        NetError::Status(abi_status) => {
            sys::print(match abi_status {
                aricode_proto::net::Status::Timeout => {
                    b"sin respuesta (plazo vencido)\n" as &[u8]
                }
                aricode_proto::net::Status::NoRoute => b"sin configuracion de red\n",
                aricode_proto::net::Status::NotFound => b"nombre desconocido\n",
                _ => b"el servicio de red rechazo la peticion\n",
            });
        }
        NetError::Ipc(_) => sys::print(b"el servicio de red no responde\n"),
    }
}

fn net(client: &NetClient) {
    match client.info() {
        Ok(i) => {
            sys::print(b"ip      ");
            print_ip(i.address);
            sys::print(b"\nmascara ");
            print_ip(i.mask);
            sys::print(b"\nrouter  ");
            print_ip(i.router);
            sys::print(b"\ndns     ");
            print_ip(i.dns);
            sys::print(b"\nmac     ");
            for (n, b) in i.mac.iter().enumerate() {
                if n > 0 {
                    sys::print(b":");
                }
                hex(*b as u64, 2);
            }
            sys::print(b"\n");
        }
        Err(e) => net_error(e),
    }
}

fn ping(client: &NetClient, arg: &[u8]) {
    let Some(ip) = core::str::from_utf8(arg)
        .ok()
        .and_then(aricode_net::Ipv4Addr::parse)
    else {
        sys::print(b"uso: ping A.B.C.D\n");
        return;
    };
    let mut received = 0;
    for seq in 1..=4u16 {
        match client.ping(ip.0, seq, 1000) {
            Ok(rtt) => {
                received += 1;
                sys::print(b"respuesta de ");
                print_ip(ip.0);
                sys::print(b": seq=");
                num(seq as u64);
                sys::print(b" tiempo=");
                num(rtt);
                sys::print(b" ms\n");
            }
            Err(e) => {
                sys::print(b"seq=");
                num(seq as u64);
                sys::print(b": ");
                net_error(e);
            }
        }
    }
    num(received);
    sys::print(b" de 4 respuestas\n");
}

fn dns(client: &mut NetClient, arg: &[u8]) {
    let Ok(name) = core::str::from_utf8(arg) else {
        sys::print(b"dns: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        sys::print(b"uso: dns NOMBRE\n");
        return;
    }
    match client.resolve(name, None, 3000) {
        Ok(ip) => {
            sys::print(name.as_bytes());
            sys::print(b" -> ");
            print_ip(ip);
            sys::print(b"\n");
        }
        Err(e) => net_error(e),
    }
}

fn http_get(client: &mut NetClient, arg: &[u8]) {
    let Ok(url) = core::str::from_utf8(arg) else {
        sys::print(b"http: url invalida\n");
        return;
    };
    if url.is_empty() {
        sys::print(b"uso: http [http://]host[:puerto][/ruta]\n");
        return;
    }
    let mut body = [0u8; 4096];
    match aricode_rt::sys::http::get(client, url, &mut body, 5000) {
        Ok(f) => {
            sys::print(b"HTTP ");
            num(f.status as u64);
            sys::print(b", ");
            num(f.body_len as u64);
            sys::print(b" bytes\n");
            sys::print(&body[..f.body_len]);
            if f.body_len > 0 && body[f.body_len - 1] != b'\n' {
                sys::print(b"\n");
            }
        }
        Err(aricode_rt::sys::http::HttpError::BadUrl) => sys::print(b"http: url invalida\n"),
        Err(aricode_rt::sys::http::HttpError::Resolve(e)) => net_error(e),
        Err(aricode_rt::sys::http::HttpError::Connect(e)) => {
            sys::print(b"http: no se pudo conectar: ");
            net_error(e);
        }
        Err(aricode_rt::sys::http::HttpError::Io(e)) => net_error(e),
        Err(aricode_rt::sys::http::HttpError::Malformed) => {
            sys::print(b"http: respuesta no valida\n")
        }
        Err(aricode_rt::sys::http::HttpError::Unsupported) => {
            sys::print(b"http: respuesta con codificacion o protocolo no soportado\n")
        }
        Err(aricode_rt::sys::http::HttpError::TooBig) => {
            sys::print(b"http: respuesta demasiado grande (4 KiB)\n")
        }
    }
}

/// Lanza un programa del initrd con solo sus handles iniciales (consola,
/// proceso e hilo) y un `SYSTEM` de solo lectura, y espera su salida.
fn run(arg: &[u8]) {
    let Ok(name) = core::str::from_utf8(arg) else {
        sys::print(b"run: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        sys::print(b"uso: run PROGRAMA [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    let Ok(system) = sys::handle_duplicate(SYSTEM, sys::rights::READ | sys::rights::TRANSFER)
    else {
        sys::print(b"run: sin acceso al sistema\n");
        return;
    };
    match sys::process_spawn_with(SPAWNER, prog, num_arg, &[system]) {
        Ok(child) => {
            let result = sys::process_wait(child, TIMEOUT_INFINITE);
            let _ = sys::handle_close(child);
            sys::print(b"run: ");
            sys::print(prog.as_bytes());
            match result {
                Ok(code) => {
                    sys::print(b" termino con ");
                    num(code);
                    sys::print(b"\n");
                }
                Err(sys::Error::Killed) => sys::print(b" fue terminado por el kernel\n"),
                Err(e) => aricode_rt::print_error(b"", e),
            }
        }
        Err(sys::Error::NotFound) => {
            sys::print(b"run: no existe ");
            sys::print(prog.as_bytes());
            sys::print(b" en el initrd\n");
        }
        Err(e) => aricode_rt::print_error(b"run", e),
    }
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
    sys::print(b"red       virtio_net  ");
    sys::print(if mask & SERVICE_NET != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    sys::print(b"entrada   virtio_input ");
    sys::print(if mask & SERVICE_INPUT != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    sys::print(b"grafico   compositor  ");
    sys::print(if mask & SERVICE_GUI != 0 {
        b"activo\n"
    } else {
        b"ausente (arranca con `gui`)\n"
    });
}

fn main(mask: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        sys::print(b"arish: sin cliente de ficheros\n");
        return 1;
    };
    let Ok(mut net_client) = NetClient::new(NETD, HW) else {
        sys::print(b"arish: sin cliente de red\n");
        return 2;
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
            b"net" => net(&net_client),
            b"ping" => ping(&net_client, rest),
            b"dns" => dns(&mut net_client, rest),
            b"http" => http_get(&mut net_client, rest),
            b"run" => run(rest),
            b"reboot" => {
                aricode_rt::print_error(b"reboot", sys::system_power(SYSTEM, abi::power::REBOOT));
            }
            b"shutdown" => {
                aricode_rt::print_error(
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

aricode_rt::entry!(main);
