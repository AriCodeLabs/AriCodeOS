//! arish (Fase 21): shell interactivo en ring 3. Solo ve lo que init le da:
//! 0x10004 = endpoint de fsd (WRITE), 0x10005 = HARDWARE (buffer DMA),
//! 0x10006 = SYSTEM (READ|MANAGE), 0x10007 = entrada de consola (READ),
//! 0x10008 = raíz PCI (READ), 0x10009 = endpoint de netd (WRITE), 0x1000A =
//! broker de lanzamiento de init (WRITE) para `run`. `arg` = máscara de
//! servicios en ejecución. El shell no puede crear procesos por sí mismo.
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
const BROKER: Handle = Handle(0x0001_000A);

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
        b"comandos:\n  help      esta ayuda\n  ps        procesos\n  mem       memoria fisica\n  ls        ficheros de AriFS\n  cat NOM   muestra un fichero\n  write NOM TEXTO  guarda TEXTO en el fichero (lo crea o lo reemplaza entero)\n  devices   funciones PCI\n  services  servicios de arranque (--detail: estado del supervisor)\n  uptime    tiempo desde el arranque\n  cpus      procesadores en linea y reposo por CPU\n  diag      informe completo (uptime, mem, cpus, ps, services)\n  net       configuracion de red\n  ping IP   eco ICMP (4 intentos)\n  dns NOM   resuelve un nombre\n  http URL  GET y muestra el cuerpo\n  run PROG  ejecuta un programa del initrd y espera\n  spawn PROG  lo lanza en segundo plano (no se puede detener)\n  repeat N CMD  repite un comando N veces\n  reboot    reinicia\n  shutdown  apaga\n  exit      termina el shell\n",
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

fn cpus() {
    let mut buf = [0u8; 8];
    if sys::system_info(SYSTEM, abi::sysinfo::CPUS, &mut buf) != Ok(8) {
        sys::print(b"cpus: sin acceso al sistema\n");
        return;
    }
    sys::print(b"cpus: ");
    num(u64::from_le_bytes(buf));
    sys::print(b" en linea\n");
    // Reposo por CPU: ciclos en `hlt` frente a los transcurridos.
    let mut idle = [0u8; 8 * (2 + 16)];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::IDLE, &mut idle) else {
        return;
    };
    let words = idle[..n].as_chunks::<8>().0;
    let (Some(elapsed), Some(cpus)) = (words.first(), words.get(1)) else {
        return;
    };
    let (elapsed, cpus) = (u64::from_le_bytes(*elapsed), u64::from_le_bytes(*cpus));
    for (i, cycles) in words.iter().skip(2).take(cpus as usize).enumerate() {
        let cycles = u64::from_le_bytes(*cycles).min(elapsed);
        sys::print(b"cpu ");
        num(i as u64);
        sys::print(b": ");
        num(cycles * 100 / elapsed.max(1));
        sys::print(b"% en reposo\n");
    }
}

/// `diag`: informe completo en un bloque, para adjuntar al describir un
/// problema (docs/performance.md).
fn diag(fs: &mut FsClient, mask: u64) {
    sys::print(b"=== diagnostico AriCodeOS ===\n");
    uptime();
    mem();
    cpus();
    ps();
    services(mask, fs);
    services_detail();
    sys::print(b"=== fin del diagnostico ===\n");
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

/// `write NOMBRE TEXTO`: crea el fichero si no existe y sustituye su
/// contenido entero (más un salto de línea) de forma atómica y duradera
/// (ADR 0014).
fn write(fs: &mut FsClient, rest: &[u8]) {
    let (name, text) = split(rest);
    let Ok(name) = core::str::from_utf8(name) else {
        sys::print(b"write: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        sys::print(b"uso: write NOMBRE TEXTO\n");
        return;
    }
    let fd = match fs.open(name).or_else(|_| fs.create(name)) {
        Ok(fd) => fd,
        Err(_) => {
            sys::print(b"write: no se pudo crear ");
            sys::print(name.as_bytes());
            sys::print(b"\n");
            return;
        }
    };
    let mut content = [0u8; LINE_MAX + 1];
    content[..text.len()].copy_from_slice(text);
    content[text.len()] = b'\n';
    match fs.replace(fd, &content[..text.len() + 1]) {
        Ok(n) => {
            sys::print(b"write: ");
            num(n as u64);
            sys::print(b" bytes en ");
            sys::print(name.as_bytes());
            sys::print(b"\n");
        }
        Err(_) => sys::print(b"write: error de escritura\n"),
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

/// Pide al broker (init) que ejecute un programa del initrd con las
/// capabilities de su manifiesto y espera su salida.
fn run(arg: &[u8]) {
    use aricode_rt::sys::launch::{self, LaunchError, Status};
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
    // El resultado se imprime entero al final: mientras el programa corre,
    // su salida y la del broker se intercalan con la nuestra.
    let result = launch::run(BROKER, prog, num_arg);
    sys::print(b"run: ");
    sys::print(prog.as_bytes());
    match result {
        Ok(code) => {
            sys::print(b" termino con ");
            num(code);
            sys::print(b"\n");
        }
        Err(LaunchError::Status(Status::NotFound)) => {
            sys::print(b": no existe en el initrd ni en AriFS\n")
        }
        Err(LaunchError::Status(Status::Denied)) => {
            sys::print(b": denegado por el broker (pide capacidades del sistema)\n")
        }
        Err(LaunchError::Status(Status::Killed)) => sys::print(b": fue terminado por el kernel\n"),
        Err(LaunchError::Status(Status::BadManifest)) => sys::print(b": manifiesto invalido\n"),
        Err(LaunchError::Status(Status::Limit)) => sys::print(b": limite de programas en marcha\n"),
        Err(LaunchError::Status(_)) => sys::print(b": el broker no pudo lanzarlo\n"),
        Err(LaunchError::BadName) => sys::print(b": nombre invalido\n"),
        Err(LaunchError::Ipc(e)) => aricode_rt::print_error(b"", e),
    }
}

/// `spawn PROGRAMA [ARG]`: lanzamiento en segundo plano por el broker.
fn spawn(arg: &[u8]) {
    use aricode_rt::sys::launch::{self, LaunchError, Status};
    let Ok(name) = core::str::from_utf8(arg) else {
        sys::print(b"spawn: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        sys::print(b"uso: spawn PROGRAMA [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    sys::print(b"spawn: ");
    sys::print(prog.as_bytes());
    match launch::spawn(BROKER, prog, num_arg) {
        Ok(()) => sys::print(b" lanzado en segundo plano\n"),
        Err(LaunchError::Status(Status::Limit)) => {
            sys::print(b": limite de programas en marcha (");
            num(4);
            sys::print(b")\n");
        }
        Err(LaunchError::Status(Status::NotFound)) => {
            sys::print(b": no existe en el initrd ni en AriFS\n")
        }
        Err(LaunchError::Status(Status::Denied)) => {
            sys::print(b": denegado por el broker (pide capacidades del sistema)\n")
        }
        Err(LaunchError::Status(Status::BadManifest)) => sys::print(b": manifiesto invalido\n"),
        Err(LaunchError::Status(_)) => sys::print(b": el broker no pudo lanzarlo\n"),
        Err(LaunchError::BadName) => sys::print(b": nombre invalido\n"),
        Err(LaunchError::Ipc(e)) => aricode_rt::print_error(b"", e),
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

/// `services --detail`: lo que init sabe de cada servicio supervisado.
fn services_detail() {
    use aricode_rt::sys::status::{service_status, LastExit};
    sys::print(b"SERVICIO         ESTADO       GEN  REINICIOS  ULTIMA SALIDA     LISTO DESDE\n");
    let mut index = 0u8;
    loop {
        let entry = match service_status(BROKER, index) {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => {
                sys::print(b"services: init no responde\n");
                return;
            }
        };
        let name = entry.name_str().as_bytes();
        sys::print(name);
        for _ in name.len()..17 {
            sys::print(b" ");
        }
        let state = entry.state.name().as_bytes();
        sys::print(state);
        for _ in state.len()..13 {
            sys::print(b" ");
        }
        let mut b = [0u8; 20];
        let g = aricode_rt::fmt_u64(entry.generation as u64, &mut b);
        sys::print(g);
        for _ in g.len()..5 {
            sys::print(b" ");
        }
        let r = aricode_rt::fmt_u64(entry.restarts as u64, &mut b);
        sys::print(r);
        for _ in r.len()..11 {
            sys::print(b" ");
        }
        match entry.last_exit {
            LastExit::None => sys::print(b"-                 "),
            LastExit::Code => {
                sys::print(b"codigo ");
                let c = aricode_rt::fmt_u64(entry.last_code, &mut b);
                sys::print(c);
                for _ in c.len()..11 {
                    sys::print(b" ");
                }
            }
            LastExit::Killed => sys::print(b"fallo (killed)    "),
            LastExit::StartTimeout => sys::print(b"plazo de arranque "),
        }
        if entry.ready_for_ms > 0 {
            sys::print(b"hace ");
            num(entry.ready_for_ms / 1000);
            sys::print(b" s");
        } else {
            sys::print(b"-");
        }
        if entry.optional {
            sys::print(b"  (opcional)");
        }
        sys::print(b"\n");
        index += 1;
        if index == 255 {
            break;
        }
    }
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
        if execute(&mut fs, &mut net_client, mask, &line[..len], 0) {
            return 0;
        }
    }
}

/// Ejecuta una línea; devuelve `true` si el shell debe terminar. `depth`
/// acota la recursión de `repeat`.
fn execute(
    fs: &mut FsClient,
    net_client: &mut NetClient,
    mask: u64,
    line: &[u8],
    depth: u8,
) -> bool {
    let (cmd, rest) = split(line);
    match cmd {
        b"" => {}
        b"help" => help(),
        b"ps" => ps(),
        b"mem" => mem(),
        b"ls" => ls(fs),
        b"cat" => cat(fs, rest),
        b"write" => write(fs, rest),
        b"devices" => devices(),
        b"services" if rest == b"--detail" => services_detail(),
        b"services" => services(mask, fs),
        b"uptime" => uptime(),
        b"cpus" => cpus(),
        b"diag" => diag(fs, mask),
        b"net" => net(net_client),
        b"ping" => ping(net_client, rest),
        b"dns" => dns(net_client, rest),
        b"http" => http_get(net_client, rest),
        b"run" => run(rest),
        b"spawn" => spawn(rest),
        b"repeat" => {
            let (count, inner) = split(rest);
            let Some(times) = core::str::from_utf8(count)
                .ok()
                .and_then(|c| c.parse::<u32>().ok())
            else {
                sys::print(b"uso: repeat N COMANDO\n");
                return false;
            };
            if depth > 0 || inner.is_empty() {
                sys::print(b"repeat: sin anidar y con un comando\n");
                return false;
            }
            for _ in 0..times.min(10_000) {
                if execute(fs, net_client, mask, inner, depth + 1) {
                    return true;
                }
            }
        }
        b"reboot" => {
            aricode_rt::print_error(b"reboot", sys::system_power(SYSTEM, abi::power::REBOOT));
        }
        b"shutdown" => {
            aricode_rt::print_error(b"shutdown", sys::system_power(SYSTEM, abi::power::SHUTDOWN));
        }
        b"exit" => return true,
        other => {
            sys::print(b"comando desconocido: ");
            sys::print(other);
            sys::print(b" (help)\n");
        }
    }
    false
}

aricode_rt::entry!(main);
