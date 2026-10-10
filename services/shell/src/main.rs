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

/// Captura de la salida para `diag NOM`: un solo hilo, así que una celda
/// estática basta.
const CAPTURE_MAX: usize = 16 * 1024;

struct Capture {
    state: core::cell::UnsafeCell<([u8; CAPTURE_MAX], usize, bool)>,
}

// SAFETY: el shell tiene un único hilo; nadie más toca la captura.
unsafe impl Sync for Capture {}

static CAPTURE: Capture = Capture {
    state: core::cell::UnsafeCell::new(([0; CAPTURE_MAX], 0, false)),
};

fn capture_with<R>(f: impl FnOnce(&mut ([u8; CAPTURE_MAX], usize, bool)) -> R) -> R {
    // SAFETY: un solo hilo y sin reentrada: `f` no vuelve a llamar aquí.
    f(unsafe { &mut *CAPTURE.state.get() })
}

/// Escribe en la consola y, con la captura activa, también en el buffer
/// (lo que no cabe se pierde).
fn print(b: &[u8]) {
    sys::print(b);
    capture_with(|(buf, len, active)| {
        if *active {
            let n = b.len().min(CAPTURE_MAX - *len);
            buf[*len..*len + n].copy_from_slice(&b[..n]);
            *len += n;
        }
    });
}

fn num(v: u64) {
    let mut buf = [0u8; 20];
    print(fmt_u64(v, &mut buf));
}

fn hex(v: u64, width: usize) {
    let mut buf = [0u8; 16];
    print(fmt_hex(v, width, &mut buf));
}

/// Lee una línea con eco, retroceso y fin en CR o LF.
/// Últimas líneas ejecutadas, para las flechas arriba y abajo.
const HISTORY: usize = 8;

struct History {
    lines: [[u8; LINE_MAX]; HISTORY],
    lens: [usize; HISTORY],
    /// Líneas guardadas (hasta `HISTORY`); la más reciente es `next - 1`.
    count: usize,
    next: usize,
}

impl History {
    const fn new() -> Self {
        Self {
            lines: [[0; LINE_MAX]; HISTORY],
            lens: [0; HISTORY],
            count: 0,
            next: 0,
        }
    }

    /// Guarda una línea no vacía distinta de la última.
    fn push(&mut self, line: &[u8]) {
        if line.is_empty() || self.back(1) == Some(line) {
            return;
        }
        self.lines[self.next][..line.len()].copy_from_slice(line);
        self.lens[self.next] = line.len();
        self.next = (self.next + 1) % HISTORY;
        self.count = (self.count + 1).min(HISTORY);
    }

    /// La línea `n` posiciones hacia atrás (1 = la más reciente).
    fn back(&self, n: usize) -> Option<&[u8]> {
        if n == 0 || n > self.count {
            return None;
        }
        let i = (self.next + HISTORY - n) % HISTORY;
        Some(&self.lines[i][..self.lens[i]])
    }
}

/// Sustituye lo tecleado por `text`, borrándolo también en pantalla.
fn replace_line(line: &mut [u8; LINE_MAX], len: &mut usize, text: &[u8]) {
    for _ in 0..*len {
        print(b"\x08 \x08");
    }
    let n = text.len().min(LINE_MAX);
    line[..n].copy_from_slice(&text[..n]);
    *len = n;
    print(&line[..n]);
}

/// Lee una línea de la consola con edición mínima: retroceso y, con las
/// flechas arriba y abajo (secuencias `ESC [ A` / `ESC [ B`, como las de un
/// terminal serie), el historial. El resto de secuencias se descartan.
fn read_line(line: &mut [u8; LINE_MAX], history: &History) -> usize {
    let mut len = 0;
    let mut chunk = [0u8; 64];
    // Secuencia de escape en curso: 0 nada, 1 `ESC`, 2 `ESC [`, 3 parámetro
    // numérico pendiente de su final (`~`).
    let mut esc = 0u8;
    // Posición en el historial: 0 = línea nueva, n = `n` hacia atrás.
    let mut back = 0usize;
    loop {
        let Ok(n) = sys::console_read(CONSOLE_IN, &mut chunk, TIMEOUT_INFINITE) else {
            continue;
        };
        for &b in &chunk[..n] {
            match esc {
                1 => {
                    esc = if b == b'[' { 2 } else { 0 };
                    continue;
                }
                2 => {
                    esc = 0;
                    match b {
                        b'A' => {
                            if let Some(text) = history.back(back + 1) {
                                back += 1;
                                replace_line(line, &mut len, text);
                            }
                        }
                        b'B' => {
                            if back > 0 {
                                back -= 1;
                                let text = history.back(back).unwrap_or(b"");
                                replace_line(line, &mut len, text);
                            }
                        }
                        b'0'..=b'9' => esc = 3,
                        _ => {}
                    }
                    continue;
                }
                3 => {
                    if !b.is_ascii_digit() {
                        esc = 0;
                    }
                    continue;
                }
                _ => {}
            }
            match b {
                0x1B => esc = 1,
                b'\r' | b'\n' => {
                    print(b"\n");
                    return len;
                }
                0x08 | 0x7F => {
                    if len > 0 {
                        len -= 1;
                        print(b"\x08 \x08");
                    }
                }
                0x20..=0x7E if len < LINE_MAX => {
                    line[len] = b;
                    len += 1;
                    print(&[b]);
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
    print(
        b"comandos:\n  help      esta ayuda\n  ps        procesos\n  mem       memoria fisica\n  ls        ficheros de AriFS\n  cat NOM   muestra un fichero\n  write NOM TEXTO  guarda TEXTO en el fichero (lo crea o lo reemplaza entero)\n  devices   funciones PCI\n  services  servicios de arranque (--detail: estado del supervisor)\n  uptime    tiempo desde el arranque\n  cpus      procesadores en linea y reposo por CPU\n  diag [NOM]  informe completo (uptime, mem, cpus, ps, services, devices); con NOM lo guarda en AriFS\n  net       configuracion de red\n  ping IP   eco ICMP (4 intentos)\n  dns NOM   resuelve un nombre\n  http URL  GET y muestra el cuerpo\n  run PROG  ejecuta un programa del initrd y espera\n  spawn PROG  lo lanza en segundo plano (no se puede detener)\n  repeat N CMD  repite un comando N veces\n  reboot    reinicia\n  shutdown  apaga\n  exit      termina el shell\n",
    );
}

fn ps() {
    use abi::sysinfo::{ProcessEntry, ProcessState};
    let mut buf = [0u8; ProcessEntry::LEN * 32];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::PROCESSES, &mut buf) else {
        print(b"ps: sin acceso al sistema\n");
        return;
    };
    print(b"PID  ESTADO      NOMBRE\n");
    for chunk in buf[..n].as_chunks::<{ ProcessEntry::LEN }>().0 {
        let Some(e) = ProcessEntry::decode(chunk) else {
            continue;
        };
        let mut b = [0u8; 20];
        let pid = fmt_u64(e.pid as u64, &mut b);
        print(pid);
        for _ in pid.len()..5 {
            print(b" ");
        }
        match e.state {
            ProcessState::Running => print(b"activo      "),
            ProcessState::Exited => {
                print(b"salio(");
                num(e.exit_code as u64);
                print(b")");
                for _ in fmt_u64(e.exit_code as u64, &mut b).len()..5 {
                    print(b" ");
                }
            }
            ProcessState::Killed => print(b"terminado   "),
        }
        print(e.name_str().as_bytes());
        print(b"\n");
    }
}

fn mem() {
    let mut buf = [0u8; 16];
    if sys::system_info(SYSTEM, abi::sysinfo::MEMORY, &mut buf) != Ok(16) {
        print(b"mem: sin acceso al sistema\n");
        return;
    }
    let total = u64::from_le_bytes(buf[..8].try_into().unwrap());
    let free = u64::from_le_bytes(buf[8..].try_into().unwrap());
    print(b"memoria fisica utilizable: ");
    num(total * 4 / 1024);
    print(b" MiB total, ");
    num(free * 4 / 1024);
    print(b" MiB libres, ");
    num((total - free) * 4 / 1024);
    print(b" MiB en uso (");
    num(total);
    print(b" frames de 4 KiB)\n");
}

fn uptime() {
    let mut buf = [0u8; 8];
    if sys::system_info(SYSTEM, abi::sysinfo::UPTIME, &mut buf) != Ok(8) {
        print(b"uptime: sin acceso al sistema\n");
        return;
    }
    let ms = u64::from_le_bytes(buf);
    print(b"uptime: ");
    num(ms / 1000);
    print(b".");
    let frac = (ms % 1000) / 10;
    if frac < 10 {
        print(b"0");
    }
    num(frac);
    print(b" s\n");
}

fn cpus() {
    let mut buf = [0u8; 8];
    if sys::system_info(SYSTEM, abi::sysinfo::CPUS, &mut buf) != Ok(8) {
        print(b"cpus: sin acceso al sistema\n");
        return;
    }
    print(b"cpus: ");
    num(u64::from_le_bytes(buf));
    print(b" en linea\n");
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
        print(b"cpu ");
        num(i as u64);
        print(b": ");
        num(cycles * 100 / elapsed.max(1));
        print(b"% en reposo\n");
    }
}

/// `diag`: informe completo en un bloque, para adjuntar al describir un
/// problema (docs/performance.md).
/// Informe completo; con un nombre, además se guarda en AriFS para leerlo
/// desde el anfitrión (`cargo xtask fs-cat`, docs/hardware.md).
fn diag(fs: &mut FsClient, mask: u64, rest: &[u8]) {
    let name = trim(rest);
    if !name.is_empty() {
        capture_with(|(_, len, active)| {
            *len = 0;
            *active = true;
        });
    }
    print(b"=== diagnostico AriCodeOS ===\n");
    uptime();
    mem();
    cpus();
    ps();
    services(mask, fs);
    services_detail();
    devices();
    events();
    print(b"=== fin del diagnostico ===\n");
    if name.is_empty() {
        return;
    }
    let len = capture_with(|(_, len, active)| {
        *active = false;
        *len
    });
    let Ok(name) = core::str::from_utf8(name) else {
        print(b"diag: nombre invalido\n");
        return;
    };
    // SAFETY: la captura ya está cerrada; se lee lo escrito.
    let data =
        capture_with(|(buf, _, _)| unsafe { core::slice::from_raw_parts(buf.as_ptr(), len) });
    match save_file(fs, name, data) {
        Ok(n) => {
            print(b"diag: ");
            num(n as u64);
            print(b" bytes en ");
            print(name.as_bytes());
            print(b"\n");
        }
        Err(()) => print(b"diag: no se pudo guardar el informe\n"),
    }
}

/// Crea o reemplaza entero el fichero `name` con `data`.
fn save_file(fs: &mut FsClient, name: &str, data: &[u8]) -> Result<usize, ()> {
    let fd = fs.open(name).or_else(|_| fs.create(name)).map_err(|_| ())?;
    let r = fs.replace(fd, data).map_err(|_| ());
    let _ = fs.close(fd);
    r
}

/// Sucesos recientes del kernel (`sysinfo::EVENTS`): un renglon por suceso
/// con su instante.
fn events() {
    use abi::sysinfo::{EventEntry, EventKind};
    let mut buf = [0u8; EventEntry::LEN * 16];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::EVENTS, &mut buf) else {
        print(b"diag: sin acceso a los sucesos\n");
        return;
    };
    print(b"sucesos recientes del kernel:\n");
    for chunk in buf[..n].as_chunks::<{ EventEntry::LEN }>().0 {
        let Some(e) = EventEntry::decode(chunk) else {
            continue;
        };
        print(b"  [");
        num(e.at_us / 1_000_000);
        print(b".");
        let frac = (e.at_us / 1000) % 1000;
        if frac < 100 {
            print(b"0");
        }
        if frac < 10 {
            print(b"0");
        }
        num(frac);
        print(b" s] ");
        match e.kind {
            EventKind::ProcessCreated => {
                print(b"proceso ");
                num(e.pid as u64);
                print(b" '");
                print(e.name_str().as_bytes());
                print(b"' creado");
            }
            EventKind::ProcessExited => {
                print(b"proceso ");
                num(e.pid as u64);
                print(b" '");
                print(e.name_str().as_bytes());
                print(b"' salio con codigo ");
                num(e.code);
            }
            EventKind::ProcessKilled => {
                print(b"proceso ");
                num(e.pid as u64);
                print(b" terminado por fallo: ");
                print(e.name_str().as_bytes());
            }
            EventKind::DmaFault => {
                print(b"fallo de DMA del dispositivo ");
                hex(e.code, 4);
                print(b": ");
                print(e.name_str().as_bytes());
            }
            EventKind::DmaQuota => {
                print(b"proceso ");
                num(e.pid as u64);
                print(b" agoto su cuota de memoria pidiendo ");
                num(e.code);
                print(b" bytes");
            }
            EventKind::None => print(b"?"),
        }
        print(b"\n");
    }
}

fn ls(fs: &FsClient) {
    let mut name = [0u8; 32];
    let mut i = 0;
    loop {
        match fs.list(i, &mut name) {
            Ok(Some((n, size))) => {
                print(&name[..n]);
                for _ in n..20 {
                    print(b" ");
                }
                num(size);
                print(b" bytes\n");
                i += 1;
            }
            Ok(None) => break,
            Err(_) => {
                print(b"ls: el servicio de ficheros no responde\n");
                return;
            }
        }
    }
    num(i as u64);
    print(b" fichero(s)\n");
}

fn cat(fs: &mut FsClient, name: &[u8]) {
    let Ok(name) = core::str::from_utf8(name) else {
        print(b"cat: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        print(b"uso: cat NOMBRE\n");
        return;
    }
    let fd = match fs.open(name) {
        Ok(fd) => fd,
        Err(_) => {
            print(b"cat: no existe ");
            print(name.as_bytes());
            print(b"\n");
            return;
        }
    };
    let mut buf = [0u8; 1024];
    let mut offset = 0u32;
    loop {
        match fs.read(fd, offset, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                print(&buf[..n]);
                offset += n as u32;
            }
            Err(_) => {
                print(b"cat: error de lectura\n");
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
        print(b"write: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        print(b"uso: write NOMBRE TEXTO\n");
        return;
    }
    let mut content = [0u8; LINE_MAX + 1];
    content[..text.len()].copy_from_slice(text);
    content[text.len()] = b'\n';
    match save_file(fs, name, &content[..text.len() + 1]) {
        Ok(n) => {
            print(b"write: ");
            num(n as u64);
            print(b" bytes en ");
            print(name.as_bytes());
            print(b"\n");
        }
        Err(()) => print(b"write: error de escritura\n"),
    }
}

fn devices() {
    let Ok(n) = sys::pci_count(PCI_ROOT) else {
        print(b"devices: sin acceso a PCI\n");
        return;
    };
    for i in 0..n {
        let Ok(v) = sys::pci_info(PCI_ROOT, i) else {
            continue;
        };
        hex(abi::pci_info::bus(v) as u64, 2);
        print(b":");
        hex(abi::pci_info::dev(v) as u64, 2);
        print(b".");
        num(abi::pci_info::func(v) as u64);
        print(b"  ");
        hex(abi::pci_info::vendor(v) as u64, 4);
        print(b":");
        hex(abi::pci_info::device(v) as u64, 4);
        print(b"  clase ");
        hex(abi::pci_info::class(v) as u64, 2);
        print(b".");
        hex(abi::pci_info::subclass(v) as u64, 2);
        print(b"\n");
    }
    num(n);
    print(b" funcion(es) PCI\n");
}

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

fn net_error(e: NetError) {
    match e {
        NetError::Status(abi_status) => {
            print(match abi_status {
                aricode_proto::net::Status::Timeout => {
                    b"sin respuesta (plazo vencido)\n" as &[u8]
                }
                aricode_proto::net::Status::NoRoute => b"sin configuracion de red\n",
                aricode_proto::net::Status::NotFound => b"nombre desconocido\n",
                _ => b"el servicio de red rechazo la peticion\n",
            });
        }
        NetError::Ipc(_) => print(b"el servicio de red no responde\n"),
    }
}

fn net(client: &NetClient) {
    match client.info() {
        Ok(i) => {
            print(b"ip      ");
            print_ip(i.address);
            print(b"\nmascara ");
            print_ip(i.mask);
            print(b"\nrouter  ");
            print_ip(i.router);
            print(b"\ndns     ");
            print_ip(i.dns);
            print(b"\nmac     ");
            for (n, b) in i.mac.iter().enumerate() {
                if n > 0 {
                    print(b":");
                }
                hex(*b as u64, 2);
            }
            print(b"\n");
        }
        Err(e) => net_error(e),
    }
}

fn ping(client: &NetClient, arg: &[u8]) {
    let Some(ip) = core::str::from_utf8(arg)
        .ok()
        .and_then(aricode_net::Ipv4Addr::parse)
    else {
        print(b"uso: ping A.B.C.D\n");
        return;
    };
    let mut received = 0;
    for seq in 1..=4u16 {
        match client.ping(ip.0, seq, 1000) {
            Ok(rtt) => {
                received += 1;
                print(b"respuesta de ");
                print_ip(ip.0);
                print(b": seq=");
                num(seq as u64);
                print(b" tiempo=");
                num(rtt);
                print(b" ms\n");
            }
            Err(e) => {
                print(b"seq=");
                num(seq as u64);
                print(b": ");
                net_error(e);
            }
        }
    }
    num(received);
    print(b" de 4 respuestas\n");
}

fn dns(client: &mut NetClient, arg: &[u8]) {
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"dns: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        print(b"uso: dns NOMBRE\n");
        return;
    }
    match client.resolve(name, None, 3000) {
        Ok(ip) => {
            print(name.as_bytes());
            print(b" -> ");
            print_ip(ip);
            print(b"\n");
        }
        Err(e) => net_error(e),
    }
}

fn http_get(client: &mut NetClient, arg: &[u8]) {
    let Ok(url) = core::str::from_utf8(arg) else {
        print(b"http: url invalida\n");
        return;
    };
    if url.is_empty() {
        print(b"uso: http [http://]host[:puerto][/ruta]\n");
        return;
    }
    let mut body = [0u8; 4096];
    match aricode_rt::sys::http::get(client, url, &mut body, 5000) {
        Ok(f) => {
            print(b"HTTP ");
            num(f.status as u64);
            print(b", ");
            num(f.body_len as u64);
            print(b" bytes\n");
            print(&body[..f.body_len]);
            if f.body_len > 0 && body[f.body_len - 1] != b'\n' {
                print(b"\n");
            }
        }
        Err(aricode_rt::sys::http::HttpError::BadUrl) => print(b"http: url invalida\n"),
        Err(aricode_rt::sys::http::HttpError::Resolve(e)) => net_error(e),
        Err(aricode_rt::sys::http::HttpError::Connect(e)) => {
            print(b"http: no se pudo conectar: ");
            net_error(e);
        }
        Err(aricode_rt::sys::http::HttpError::Io(e)) => net_error(e),
        Err(aricode_rt::sys::http::HttpError::Malformed) => print(b"http: respuesta no valida\n"),
        Err(aricode_rt::sys::http::HttpError::Unsupported) => {
            print(b"http: respuesta con codificacion o protocolo no soportado\n")
        }
        Err(aricode_rt::sys::http::HttpError::TooBig) => {
            print(b"http: respuesta demasiado grande (4 KiB)\n")
        }
    }
}

/// Pide al broker (init) que ejecute un programa del initrd con las
/// capabilities de su manifiesto y espera su salida.
fn run(arg: &[u8]) {
    use aricode_rt::sys::launch::{self, LaunchError, Status};
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"run: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        print(b"uso: run PROGRAMA [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    // El resultado se imprime entero al final: mientras el programa corre,
    // su salida y la del broker se intercalan con la nuestra.
    let result = launch::run(BROKER, prog, num_arg);
    print(b"run: ");
    print(prog.as_bytes());
    match result {
        Ok(code) => {
            print(b" termino con ");
            num(code);
            print(b"\n");
        }
        Err(LaunchError::Status(Status::NotFound)) => {
            print(b": no existe en el initrd ni en AriFS\n")
        }
        Err(LaunchError::Status(Status::Denied)) => {
            print(b": denegado por el broker (pide capacidades del sistema)\n")
        }
        Err(LaunchError::Status(Status::Killed)) => print(b": fue terminado por el kernel\n"),
        Err(LaunchError::Status(Status::BadManifest)) => print(b": manifiesto invalido\n"),
        Err(LaunchError::Status(Status::Limit)) => print(b": limite de programas en marcha\n"),
        Err(LaunchError::Status(_)) => print(b": el broker no pudo lanzarlo\n"),
        Err(LaunchError::BadName) => print(b": nombre invalido\n"),
        Err(LaunchError::Ipc(e)) => aricode_rt::print_error(b"", e),
    }
}

/// `spawn PROGRAMA [ARG]`: lanzamiento en segundo plano por el broker.
fn spawn(arg: &[u8]) {
    use aricode_rt::sys::launch::{self, LaunchError, Status};
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"spawn: nombre invalido\n");
        return;
    };
    if name.is_empty() {
        print(b"uso: spawn PROGRAMA [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    print(b"spawn: ");
    print(prog.as_bytes());
    match launch::spawn(BROKER, prog, num_arg) {
        Ok(()) => print(b" lanzado en segundo plano\n"),
        Err(LaunchError::Status(Status::Limit)) => {
            print(b": limite de programas en marcha (");
            num(4);
            print(b")\n");
        }
        Err(LaunchError::Status(Status::NotFound)) => {
            print(b": no existe en el initrd ni en AriFS\n")
        }
        Err(LaunchError::Status(Status::Denied)) => {
            print(b": denegado por el broker (pide capacidades del sistema)\n")
        }
        Err(LaunchError::Status(Status::BadManifest)) => print(b": manifiesto invalido\n"),
        Err(LaunchError::Status(_)) => print(b": el broker no pudo lanzarlo\n"),
        Err(LaunchError::BadName) => print(b": nombre invalido\n"),
        Err(LaunchError::Ipc(e)) => aricode_rt::print_error(b"", e),
    }
}

fn services(mask: u64, fs: &FsClient) {
    print(b"bloques   virtio_blk  ");
    print(if mask & SERVICE_BLOCK != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    print(b"ficheros  fsd         ");
    if mask & SERVICE_FS == 0 {
        print(b"ausente\n");
    } else {
        let mut name = [0u8; 32];
        print(if fs.list(0, &mut name).is_ok() {
            b"activo, responde\n"
        } else {
            b"activo, no responde\n"
        });
    }
    print(b"red       virtio_net  ");
    print(if mask & SERVICE_NET != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    print(b"entrada   virtio_input ");
    print(if mask & SERVICE_INPUT != 0 {
        b"activo\n"
    } else {
        b"ausente\n"
    });
    print(b"grafico   compositor  ");
    print(if mask & SERVICE_GUI != 0 {
        b"activo\n"
    } else {
        b"ausente (arranca con `gui`)\n"
    });
}

/// `services --detail`: lo que init sabe de cada servicio supervisado.
fn services_detail() {
    use aricode_rt::sys::status::{service_status, LastExit};
    print(b"SERVICIO         ESTADO       GEN  REINICIOS  ULTIMA SALIDA     LISTO DESDE\n");
    let mut index = 0u8;
    loop {
        let entry = match service_status(BROKER, index) {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => {
                print(b"services: init no responde\n");
                return;
            }
        };
        let name = entry.name_str().as_bytes();
        print(name);
        for _ in name.len()..17 {
            print(b" ");
        }
        let state = entry.state.name().as_bytes();
        print(state);
        for _ in state.len()..13 {
            print(b" ");
        }
        let mut b = [0u8; 20];
        let g = aricode_rt::fmt_u64(entry.generation as u64, &mut b);
        print(g);
        for _ in g.len()..5 {
            print(b" ");
        }
        let r = aricode_rt::fmt_u64(entry.restarts as u64, &mut b);
        print(r);
        for _ in r.len()..11 {
            print(b" ");
        }
        match entry.last_exit {
            LastExit::None => print(b"-                 "),
            LastExit::Code => {
                print(b"codigo ");
                let c = aricode_rt::fmt_u64(entry.last_code, &mut b);
                print(c);
                for _ in c.len()..11 {
                    print(b" ");
                }
            }
            LastExit::Killed => print(b"fallo (killed)    "),
            LastExit::StartTimeout => print(b"plazo de arranque "),
        }
        if entry.ready_for_ms > 0 {
            print(b"hace ");
            num(entry.ready_for_ms / 1000);
            print(b" s");
        } else {
            print(b"-");
        }
        if entry.optional {
            print(b"  (opcional)");
        }
        print(b"\n");
        index += 1;
        if index == 255 {
            break;
        }
    }
}

fn main(mask: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        print(b"arish: sin cliente de ficheros\n");
        return 1;
    };
    let Ok(mut net_client) = NetClient::new(NETD, HW) else {
        print(b"arish: sin cliente de red\n");
        return 2;
    };
    print(b"arish: shell de AriCodeOS (help para ayuda)\n");
    let mut line = [0u8; LINE_MAX];
    let mut history = History::new();
    loop {
        print(PROMPT);
        let len = read_line(&mut line, &history);
        history.push(&line[..len]);
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
        b"diag" => diag(fs, mask, rest),
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
                print(b"uso: repeat N COMANDO\n");
                return false;
            };
            if depth > 0 || inner.is_empty() {
                print(b"repeat: sin anidar y con un comando\n");
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
            print(b"comando desconocido: ");
            print(other);
            print(b" (help)\n");
        }
    }
    false
}

aricode_rt::entry!(main);
