//! Lanzamiento de QEMU + OVMF, interactivo o con captura de serie.

use crate::image::{self, OVMF_CODE};
use crate::{build_dir, root, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `esp`: directorio que QEMU expone como disco FAT; `None` cuando la ESP va
/// dentro de la imagen de disco (imagen USB).
fn base_command(
    esp: Option<&Path>,
    vars: &Path,
    disk: &Path,
    disk_readonly: bool,
    cpus: u32,
) -> Command {
    let mut q = Command::new("qemu-system-x86_64");
    q.current_dir(root());
    q.args([
        "-machine",
        "q35",
        "-cpu",
        "qemu64",
        "-smp",
        &cpus.max(1).to_string(),
        "-m",
        "512M",
        "-no-reboot",
    ]);
    // IOMMU VT-d (Fase 33) antes que cualquier dispositivo PCI; los virtio
    // llevan `iommu_platform=on` para que su DMA pase por la traduccion.
    q.args(["-device", "intel-iommu,aw-bits=48"]);
    // Disco virtio-blk moderno (sin interfaz legacy) para el driver de usuario.
    q.args([
        "-drive",
        &format!(
            "if=none,id=disk0,format=raw,file={}{}",
            disk.display(),
            if disk_readonly { ",readonly=on" } else { "" }
        ),
    ]);
    q.args([
        "-device",
        "virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    q.args([
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"),
    ]);
    q.args([
        "-drive",
        &format!("if=pflash,format=raw,file={}", vars.display()),
    ]);
    if let Some(esp) = esp {
        q.args([
            "-drive",
            &format!("format=raw,file=fat:rw:{}", esp.display()),
        ]);
    }
    // Red de usuario (slirp): DHCP en 10.0.2.2, DNS en 10.0.2.3; el anfitrión
    // virtual 10.0.2.2 redirige UDP/TCP al loopback del host, donde escucha
    // el respondedor de pruebas.
    q.args([
        "-netdev",
        &format!(
            "user,id=net0,hostfwd=tcp:127.0.0.1:{}-:{}",
            aricode_proto::net::TEST_TCP_FORWARD_PORT,
            aricode_proto::net::TEST_TCP_LISTEN_PORT
        ),
    ]);
    q.args([
        "-device",
        "virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    // Teclado y ratón virtio con id fijo: QMP inyecta eventos por dispositivo.
    q.args([
        "-device",
        "virtio-keyboard-pci,id=kbd0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    q.args([
        "-device",
        "virtio-mouse-pci,id=mouse0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    q.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    q
}

/// Inyección de entrada por QMP: al ver `marker` en la serie se teclean
/// `keys` (nombres qcode de QEMU) y se mueve y pulsa el ratón.
pub struct InputProbe<'a> {
    pub marker: &'a str,
    pub keys: &'a [String],
    pub mouse_rel: Option<(i32, i32)>,
    pub click: Option<&'a str>,
}

/// Cliente QMP mínimo sobre un socket Unix: negociación y comandos JSON
/// escritos a mano (solo cadenas y números); espera `return` o `error`.
struct Qmp {
    stream: std::os::unix::net::UnixStream,
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
}

impl Qmp {
    fn connect(path: &Path) -> std::io::Result<Self> {
        use std::io::BufRead;
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = std::io::BufReader::new(stream.try_clone()?);
        let mut greeting = String::new();
        reader.read_line(&mut greeting)?;
        let mut q = Self { stream, reader };
        q.execute(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(q)
    }

    fn execute(&mut self, command: &str) -> std::io::Result<()> {
        use std::io::{BufRead, Write};
        self.stream.write_all(command.as_bytes())?;
        self.stream.write_all(b"\n")?;
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(std::io::Error::other("QMP cerrado"));
            }
            if line.contains(r#""return""#) {
                return Ok(());
            }
            if line.contains(r#""error""#) {
                return Err(std::io::Error::other(line.trim().to_string()));
            }
        }
    }

    /// Sin `device`: QMP solo admite ahí una pantalla, no un dispositivo de
    /// entrada; el evento va al manejador activo de su tipo (los virtio se
    /// registran después que el i8042 y tienen prioridad).
    fn send_event(&mut self, event: &str) -> std::io::Result<()> {
        self.execute(&format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{event}]}}}}"#
        ))
    }
}

fn run_input_probe(
    path: &Path,
    keys: &[String],
    mouse_rel: Option<(i32, i32)>,
    click: Option<&str>,
) -> String {
    let mut qmp = match Qmp::connect(path) {
        Ok(q) => q,
        Err(e) => return format!("[host] QMP: sin conexion ({e})"),
    };
    let mut sent = 0;
    let mut step = |qmp: &mut Qmp, event: String| -> Result {
        qmp.send_event(&event).map_err(|e| e.to_string())?;
        sent += 1;
        Ok(())
    };
    let result: Result = (|| {
        // Orden natural: apuntar, pulsar y despues teclear.
        if let Some((dx, dy)) = mouse_rel {
            step(
                &mut qmp,
                format!(r#"{{"type":"rel","data":{{"axis":"x","value":{dx}}}}}"#),
            )?;
            step(
                &mut qmp,
                format!(r#"{{"type":"rel","data":{{"axis":"y","value":{dy}}}}}"#),
            )?;
        }
        // Una pausa breve entre fases: el compositor ordena por instante de
        // llegada y un usuario real no pulsa y teclea en el mismo milisegundo.
        std::thread::sleep(Duration::from_millis(100));
        if let Some(button) = click {
            for down in [true, false] {
                step(
                    &mut qmp,
                    format!(r#"{{"type":"btn","data":{{"down":{down},"button":"{button}"}}}}"#),
                )?;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        for key in keys {
            for down in [true, false] {
                step(
                    &mut qmp,
                    format!(
                        r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"qcode","data":"{key}"}}}}}}"#
                    ),
                )?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => format!("[host] QMP: {sent} eventos de entrada enviados"),
        Err(e) => format!("[host] QMP: fallo tras {sent} eventos ({e})"),
    }
}

/// Respondedor de pruebas en el loopback del host: por UDP devuelve un
/// registro A fijo a las consultas DNS y hace eco de lo demás; por TCP hace
/// eco de cada conexión. Vive mientras exista el valor devuelto.
pub struct Responder {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Servidor HTTP de pruebas: `/hola` (Content-Length), `/chunked`
/// (Transfer-Encoding: chunked) y 404 para el resto.
fn http_serve(mut stream: std::net::TcpStream) {
    use std::io::Write;
    let mut buf = [0u8; 2048];
    let mut have = 0;
    loop {
        match stream.read(&mut buf[have..]) {
            Ok(0) | Err(_) => return,
            Ok(n) => have += n,
        }
        if buf[..have].windows(4).any(|w| w == b"\r\n\r\n") || have == buf.len() {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..have]);
    let path = request.split(' ').nth(1).unwrap_or("/");
    let response: Vec<u8> = match path {
        "/hola" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 19\r\nConnection: close\r\n\r\nHola desde el host\n".to_vec(),
        "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nd\r\nprimer trozo,\r\ne\r\n segundo trozo\r\n0\r\n\r\n".to_vec(),
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nno existe".to_vec(),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

/// Eco de una conexión TCP hasta que el otro extremo cierra.
fn tcp_echo(mut stream: std::net::TcpStream) {
    use std::io::Write;
    let mut buf = [0u8; 4096];
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 || stream.write_all(&buf[..n]).is_err() {
            break;
        }
    }
}

/// Dirección que el respondedor devuelve a toda consulta DNS.
pub const RESPONDER_DNS_ANSWER: aricode_net::Ipv4Addr = aricode_net::Ipv4Addr::new(10, 1, 2, 3);

/// Respuesta a un datagrama recibido por el respondedor.
pub fn responder_reply(datagram: &[u8], out: &mut [u8]) -> usize {
    if let Some(n) = aricode_net::dns::write_answer(datagram, RESPONDER_DNS_ANSWER, 60, out) {
        let mut name = [0u8; aricode_net::dns::MAX_NAME];
        if aricode_net::dns::query_name(datagram, &mut name).is_some() && datagram[2] & 0x80 == 0 {
            return n;
        }
    }
    let n = datagram.len().min(out.len());
    out[..n].copy_from_slice(&datagram[..n]);
    n
}

impl Responder {
    pub fn start() -> Result<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let port = aricode_proto::net::TEST_RESPONDER_PORT;
        let socket = std::net::UdpSocket::bind(("127.0.0.1", port))
            .map_err(|e| format!("respondedor UDP en 127.0.0.1:{port}: {e}"))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let tcp_port = aricode_proto::net::TEST_TCP_ECHO_PORT;
        let listener = std::net::TcpListener::bind(("127.0.0.1", tcp_port))
            .map_err(|e| format!("servidor de eco TCP en 127.0.0.1:{tcp_port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let udp = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            let mut out = [0u8; 2048];
            while !flag.load(Ordering::Relaxed) {
                if let Ok((n, from)) = socket.recv_from(&mut buf) {
                    let m = responder_reply(&buf[..n], &mut out);
                    let _ = socket.send_to(&out[..m], from);
                }
            }
        });
        let http_port = aricode_proto::net::TEST_HTTP_PORT;
        let http_listener = std::net::TcpListener::bind(("127.0.0.1", http_port))
            .map_err(|e| format!("servidor HTTP de pruebas en 127.0.0.1:{http_port}: {e}"))?;
        http_listener
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        let tcp = Self::accept_loop(Arc::clone(&stop), listener, tcp_echo);
        let http = Self::accept_loop(Arc::clone(&stop), http_listener, http_serve);
        Ok(Self {
            stop,
            threads: vec![udp, tcp, http],
        })
    }

    /// Acepta conexiones hasta `stop` y atiende cada una en su hilo.
    fn accept_loop(
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        listener: std::net::TcpListener,
        serve: fn(std::net::TcpStream),
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                        std::thread::spawn(move || serve(stream));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Sonda TCP del host: al ver `marker` en la serie se conecta al puerto
/// redirigido, envía `payload` y comprueba el eco. El resultado se añade a la
/// captura como una línea `[host] ...` para que el escenario lo exija.
pub struct TcpProbe<'a> {
    pub marker: &'a str,
    pub payload: &'a str,
}

fn run_tcp_probe(payload: &str) -> String {
    use std::io::Write;
    let port = aricode_proto::net::TEST_TCP_FORWARD_PORT;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => return format!("[host] sonda TCP: sin conexion ({e})"),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if stream.write_all(payload.as_bytes()).is_err() {
        return String::from("[host] sonda TCP: fallo al enviar");
    }
    let mut got = Vec::new();
    let mut buf = [0u8; 512];
    while got.len() < payload.len() {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
    if got == payload.as_bytes() {
        format!("[host] sonda TCP: eco de {} bytes recibido", got.len())
    } else {
        format!("[host] sonda TCP: eco incorrecto ({} bytes)", got.len())
    }
}

pub fn run_interactive(release: bool, cmdline: &str, cpus: u32, usb_image: bool) -> Result {
    let base = image::build_base_esp(release)?;
    let esp = if cmdline.is_empty() {
        base
    } else {
        image::build_scenario_esp(
            &base,
            &image::scenarios_root(),
            "interactive",
            cmdline,
            &Default::default(),
        )?
    };
    let vars = image::ovmf_vars_for(&build_dir())?;
    let disk = build_dir().join("disk.img");
    let esp_drive = if usb_image {
        // Como en un pendrive: la ESP dentro del disco GPT, sin `fat:`.
        image::write_usb_image(&disk, &esp)?;
        None
    } else {
        image::write_test_disk(&disk, image::DiskVariant::Slots)?;
        Some(esp.as_path())
    };
    let _responder = Responder::start()?;
    let mut q = base_command(esp_drive, &vars, &disk, false, cpus);
    if cmdline.split_whitespace().any(|w| w == "gui") {
        // Con `gui` hace falta ver la pantalla: ventana de QEMU y serie en la terminal.
        println!("[xtask] QEMU con pantalla (serie en la terminal; salir: cerrar la ventana)");
        q.args(["-serial", "stdio"]);
    } else {
        println!("[xtask] QEMU (salir: Ctrl-A X)");
        q.arg("-nographic");
    }
    let st = q.status().map_err(|e| format!("qemu: {e}"))?;
    println!("[xtask] qemu exit: {:?}", st.code());
    Ok(())
}

pub struct Capture {
    pub serial: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed: Duration,
}

/// Entrada guiada por la salida: cada línea se envía cuando la serie ha
/// mostrado `prompt` tantas veces como líneas ya enviadas más una.
pub struct Input<'a> {
    pub prompt: &'a str,
    pub lines: &'a [String],
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Arranca sin pantalla, captura la serie, alimenta `input` y espera hasta
/// `timeout`.
/// Todo lo que un escenario pide al runner además de la ESP y el plazo.
pub struct Headless<'a> {
    pub input: Input<'a>,
    pub probe: Option<TcpProbe<'a>>,
    pub input_probe: Option<InputProbe<'a>>,
    pub disk_variant: image::DiskVariant,
    pub disk_readonly: bool,
    pub cpus: u32,
}

pub fn run_headless(
    esp: &Path,
    workdir: &Path,
    timeout: Duration,
    options: &Headless,
) -> Result<Capture> {
    let input = &options.input;
    let probe = options.probe.as_ref();
    let input_probe = options.input_probe.as_ref();
    let disk_variant = options.disk_variant;
    let disk_readonly = options.disk_readonly;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    let vars = image::ovmf_vars_for(workdir)?;
    let disk = workdir.join("disk.img");
    let esp_drive = if disk_variant == image::DiskVariant::UsbImage {
        image::write_usb_image(&disk, esp)?;
        None
    } else {
        image::write_test_disk(&disk, disk_variant)?;
        Some(esp)
    };
    let _responder = Responder::start()?;
    let qmp_path = workdir.join("qmp.sock");
    let _ = std::fs::remove_file(&qmp_path);
    let start = Instant::now();
    let mut child = base_command(esp_drive, &vars, &disk, disk_readonly, options.cpus)
        .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
        .args([
            "-qmp",
            &format!("unix:{},server=on,wait=off", qmp_path.display()),
        ])
        .stdin(if input.lines.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("qemu: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stdin = child.stdin.take();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().expect("captura").extend_from_slice(&chunk[..n]),
            }
        }
    });
    let mut sent = 0;
    let mut probe_result: Option<String> = None;
    let mut probe_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut input_result: Option<String> = None;
    let mut input_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut timed_out = false;
    let exit_code = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st.code();
        }
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        if sent < input.lines.len() {
            let prompts =
                count_occurrences(&captured.lock().expect("captura"), input.prompt.as_bytes());
            if prompts > sent {
                if let Some(w) = stdin.as_mut() {
                    let _ = w.write_all(format!("{}\n", input.lines[sent]).as_bytes());
                    let _ = w.flush();
                }
                sent += 1;
            }
        }
        if let Some(p) = probe {
            if probe_thread.is_none()
                && probe_result.is_none()
                && count_occurrences(&captured.lock().expect("captura"), p.marker.as_bytes()) > 0
            {
                let payload = p.payload.to_string();
                probe_thread = Some(std::thread::spawn(move || run_tcp_probe(&payload)));
            }
            if probe_thread.as_ref().is_some_and(|t| t.is_finished()) {
                probe_result = probe_thread.take().and_then(|t| t.join().ok());
            }
        }
        if let Some(p) = input_probe {
            if input_thread.is_none()
                && input_result.is_none()
                && count_occurrences(&captured.lock().expect("captura"), p.marker.as_bytes()) > 0
            {
                let path = qmp_path.clone();
                let keys = p.keys.to_vec();
                let rel = p.mouse_rel;
                let click = p.click.map(String::from);
                input_thread = Some(std::thread::spawn(move || {
                    run_input_probe(&path, &keys, rel, click.as_deref())
                }));
            }
            if input_thread.as_ref().is_some_and(|t| t.is_finished()) {
                input_result = input_thread.take().and_then(|t| t.join().ok());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    if let Some(t) = probe_thread.take() {
        probe_result = t.join().ok();
    }
    if let Some(t) = input_thread.take() {
        input_result = t.join().ok();
    }
    let _ = std::fs::remove_file(&qmp_path);
    reader.join().map_err(|_| "lector de serie")?;
    let mut bytes = captured.lock().expect("captura").clone();
    for line in [probe_result, input_result].into_iter().flatten() {
        bytes.extend_from_slice(format!("\n{line}\n").as_bytes());
    }
    Ok(Capture {
        serial: strip_ansi(&String::from_utf8_lossy(&bytes)),
        exit_code,
        timed_out,
        elapsed: start.elapsed(),
    })
}

/// Elimina las secuencias de escape que OVMF emite por la consola serie.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn responder_echoes_and_answers_dns() {
        let mut out = [0u8; 512];
        let n = super::responder_reply(b"hola", &mut out);
        assert_eq!(&out[..n], b"hola");
        let mut q = [0u8; 128];
        let qn = aricode_net::dns::write_query(0x77, "aricodeos.test", &mut q).unwrap();
        let n = super::responder_reply(&q[..qn], &mut out);
        assert_eq!(
            aricode_net::dns::parse_response(0x77, &out[..n]),
            Some(aricode_net::dns::Answer::Address(
                super::RESPONDER_DNS_ANSWER,
                60
            ))
        );
    }

    #[test]
    fn counts_prompts() {
        assert_eq!(
            super::count_occurrences(b"ari> x\nari> y\nari> ", b"ari> "),
            3
        );
        assert_eq!(super::count_occurrences(b"nada", b"ari> "), 0);
        assert_eq!(super::count_occurrences(b"aaaa", b""), 0);
    }

    use super::strip_ansi;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[2J\x1b[01;01Hhola\x1b[=3h"), "hola");
        assert_eq!(strip_ansi("sin escapes"), "sin escapes");
    }
}
