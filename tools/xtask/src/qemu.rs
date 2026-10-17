//! Launches QEMU + OVMF, interactively or capturing the serial output.

use crate::image::{self, OVMF_CODE};
use crate::{build_dir, root, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `esp`: directory QEMU exposes as a FAT disk; `None` when the ESP lives
/// inside the disk image (USB image).
/// USB controllers of the virtual machine.
#[derive(Clone, Copy, Default)]
pub struct UsbOptions {
    /// xHCI with a USB HID keyboard and mouse.
    pub hid: bool,
    /// That xHCI offers MSI only (no MSI-X).
    pub hid_msi: bool,
    /// An extra EHCI (class 0c.03, interface 0x20): not xHCI.
    pub legacy_controller: bool,
    /// The test disk hangs off the xHCI as USB storage (`usb-storage`)
    /// instead of virtio-blk (ADR 0023).
    pub storage: bool,
}

fn base_command(
    esp: Option<&Path>,
    vars: &Path,
    disk: &Path,
    disk_readonly: bool,
    cpus: u32,
    usb: UsbOptions,
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
    // VT-d IOMMU (Phase 33) before any PCI device; the virtio devices carry
    // `iommu_platform=on` so their DMA goes through translation.
    q.args(["-device", "intel-iommu,aw-bits=48"]);
    // Test disk: modern virtio-blk (no legacy interface) for the user-space
    // driver or, with `usb.storage`, a USB stick on the xHCI.
    q.args([
        "-drive",
        &format!(
            "if=none,id=disk0,format=raw,file={}{}",
            disk.display(),
            if disk_readonly { ",readonly=on" } else { "" }
        ),
    ]);
    if !usb.storage {
        q.args([
            "-device",
            "virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off,iommu_platform=on",
        ]);
    }
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
    // User networking (slirp): DHCP at 10.0.2.2, DNS at 10.0.2.3; the virtual
    // host 10.0.2.2 forwards UDP/TCP to the host loopback, where the test
    // responder listens.
    q.args([
        "-netdev",
        &format!(
            "user,id=net0,hostfwd=tcp:127.0.0.1:{}-:{},hostfwd=tcp:127.0.0.1:{}-:22",
            aricode_proto::net::TEST_TCP_FORWARD_PORT,
            aricode_proto::net::TEST_TCP_LISTEN_PORT,
            aricode_proto::net::TEST_SSH_FORWARD_PORT
        ),
    ]);
    q.args([
        "-device",
        "virtio-net-pci,netdev=net0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    // virtio keyboard and mouse with fixed ids: QMP injects events per device.
    q.args([
        "-device",
        "virtio-keyboard-pci,id=kbd0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    q.args([
        "-device",
        "virtio-mouse-pci,id=mouse0,disable-legacy=on,disable-modern=off,iommu_platform=on",
    ]);
    if usb.hid || usb.storage {
        // xHCI (Phase 25, part two) with a USB HID keyboard and mouse and/or
        // the test disk as USB storage.
        let xhci = if usb.hid_msi {
            "qemu-xhci,id=xhci,msi=on,msix=off"
        } else {
            "qemu-xhci,id=xhci"
        };
        q.args(["-device", xhci]);
        if usb.hid {
            q.args(["-device", "usb-kbd,bus=xhci.0"]);
            q.args(["-device", "usb-mouse,bus=xhci.0"]);
        }
        if usb.storage {
            q.args(["-device", "usb-storage,bus=xhci.0,drive=disk0"]);
        }
    }
    if usb.legacy_controller {
        // EHCI (class 0c.03, interface 0x20): not xHCI.
        q.args(["-device", "usb-ehci,id=ehci"]);
    }
    q.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    q
}

/// QMP input injection: on seeing `marker` in the serial output, `keys` (QEMU
/// qcode names) are typed and the mouse is moved and clicked.
pub struct InputProbe<'a> {
    pub marker: &'a str,
    pub keys: &'a [String],
    pub mouse_rel: Option<(i32, i32)>,
    pub click: Option<&'a str>,
}

/// Minimal QMP client over a Unix socket: handshake and hand-written JSON
/// commands (strings and numbers only); waits for `return` or `error`.
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
                return Err(std::io::Error::other("QMP closed"));
            }
            if line.contains(r#""return""#) {
                return Ok(());
            }
            if line.contains(r#""error""#) {
                return Err(std::io::Error::other(line.trim().to_string()));
            }
        }
    }

    /// No `device`: QMP only accepts a display there, not an input device;
    /// the event goes to the active handler of its type (the virtio devices
    /// register after the i8042 and take priority).
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
        Err(e) => return format!("[host] QMP: no connection ({e})"),
    };
    let mut sent = 0;
    let mut step = |qmp: &mut Qmp, event: String| -> Result {
        qmp.send_event(&event).map_err(|e| e.to_string())?;
        sent += 1;
        Ok(())
    };
    let result: Result = (|| {
        // Natural order: point, click, then type.
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
        // A short pause between phases: the compositor orders by arrival time
        // and a real user does not click and type in the same millisecond.
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
        // "a" presses and releases; "shift+dot" presses the modifiers, then the
        // key, and releases them in reverse order.
        for key in keys {
            let parts: Vec<&str> = key.split('+').collect();
            for part in &parts {
                step(
                    &mut qmp,
                    format!(
                        r#"{{"type":"key","data":{{"down":true,"key":{{"type":"qcode","data":"{part}"}}}}}}"#
                    ),
                )?;
            }
            for part in parts.iter().rev() {
                step(
                    &mut qmp,
                    format!(
                        r#"{{"type":"key","data":{{"down":false,"key":{{"type":"qcode","data":"{part}"}}}}}}"#
                    ),
                )?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => format!("[host] QMP: {sent} input events sent"),
        Err(e) => format!("[host] QMP: failed after {sent} events ({e})"),
    }
}

/// Test responder on the host loopback: over UDP it returns a fixed A record
/// for DNS queries and echoes everything else; over TCP it echoes every
/// connection. It lives as long as the returned value exists.
pub struct Responder {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Test HTTP server: `/hola` (Content-Length), `/chunked`
/// (Transfer-Encoding: chunked) and 404 for everything else.
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
        // The scenarios' update channel (ADR 0027), never the real one.
        p if p.starts_with("/update/") => file_response(&image::test_update_dir(), &p["/update/".len()..]),
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nno existe".to_vec(),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

/// `200` with the file `name` of `dir` (plain names only), or `404`.
pub fn file_response(dir: &Path, name: &str) -> Vec<u8> {
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b));
    match plain.then(|| std::fs::read(dir.join(name)).ok()).flatten() {
        Some(data) => {
            let mut r = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            )
            .into_bytes();
            r.extend_from_slice(&data);
            r
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"
                .to_vec()
        }
    }
}

/// `cargo xtask serve-update`: serves the channel directory on `port` (all
/// interfaces) until interrupted; `GET /manifest.txt`, `GET /aricodeos-N.img`.
/// One thread per connection with read and write timeouts: a client that
/// stalls mid-download must not block the next one.
pub fn serve_update(dir: &Path, port: u16) -> Result {
    let listener = std::net::TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    println!(
        "[xtask] serving {} on port {port}; on the machine: update http://<this host>:{port}/",
        dir.display()
    );
    let dir = dir.to_path_buf();
    for stream in listener.incoming().flatten() {
        let dir = dir.clone();
        std::thread::spawn(move || serve_update_connection(stream, &dir));
    }
    Ok(())
}

fn serve_update_connection(mut stream: std::net::TcpStream, dir: &Path) {
    use std::io::Write;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let mut buf = [0u8; 2048];
    let mut have = 0;
    while have < buf.len() {
        match stream.read(&mut buf[have..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => have += n,
        }
        if buf[..have].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf[..have]);
    let path = request.split(' ').nth(1).unwrap_or("/");
    let name = path.trim_start_matches('/').to_string();
    let response = file_response(dir, &name);
    let outcome = match stream.write_all(&response).and_then(|()| stream.flush()) {
        Ok(()) => format!("{} bytes sent", response.len()),
        Err(e) => format!("failed: {e}"),
    };
    println!("[xtask] {peer} GET /{name}: {outcome}");
    let _ = std::io::stdout().flush();
}

/// Echoes a TCP connection until the peer closes it.
fn tcp_echo(mut stream: std::net::TcpStream) {
    use std::io::Write;
    let mut buf = [0u8; 4096];
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 || stream.write_all(&buf[..n]).is_err() {
            break;
        }
    }
}

/// Address the responder returns for every DNS query.
pub const RESPONDER_DNS_ANSWER: aricode_net::Ipv4Addr = aricode_net::Ipv4Addr::new(10, 1, 2, 3);

/// Reply to a datagram received by the responder.
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
            .map_err(|e| format!("UDP responder on 127.0.0.1:{port}: {e}"))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let tcp_port = aricode_proto::net::TEST_TCP_ECHO_PORT;
        let listener = std::net::TcpListener::bind(("127.0.0.1", tcp_port))
            .map_err(|e| format!("TCP echo server on 127.0.0.1:{tcp_port}: {e}"))?;
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
            .map_err(|e| format!("test HTTP server on 127.0.0.1:{http_port}: {e}"))?;
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

    /// Accepts connections until `stop` and serves each one on its own thread.
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

/// Host TCP probe: on seeing `marker` in the serial output it connects to the
/// forwarded port, sends `payload` and checks the echo. The result is appended
/// to the capture as a `[host] ...` line so the scenario can require it.
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
        Err(e) => return format!("[host] TCP probe: no connection ({e})"),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if stream.write_all(payload.as_bytes()).is_err() {
        return String::from("[host] TCP probe: send failed");
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
        format!("[host] TCP probe: echo of {} bytes received", got.len())
    } else {
        format!("[host] TCP probe: wrong echo ({} bytes)", got.len())
    }
}

/// SSH probe: commands run with the host's `ssh` and the scenario's key.
pub struct SshProbe<'a> {
    pub marker: &'a str,
    pub commands: &'a [String],
    pub key: std::path::PathBuf,
    pub idle_client: bool,
}

/// Runs each command over SSH; the result is one block of `[host] ssh`
/// lines: the command, every output line and the exit status.
fn run_ssh_probe(commands: &[String], key: &Path, idle_client: bool) -> String {
    let mut lines = Vec::new();
    // An idle client that holds a connection without identifying itself;
    // kept open for the whole probe (sshd must drop it on its own).
    let idle = idle_client.then(|| {
        let addr =
            std::net::SocketAddr::from(([127, 0, 0, 1], aricode_proto::net::TEST_SSH_FORWARD_PORT));
        match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(s) => {
                lines.push(String::from("[host] ssh idle client connected"));
                Some(s)
            }
            Err(e) => {
                lines.push(format!("[host] ssh idle client: no connection ({e})"));
                None
            }
        }
    });
    for cmd in commands {
        let out = Command::new("ssh")
            .args([
                "-F",
                "/dev/null",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
            ])
            .args([
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=90",
                "-o",
                "LogLevel=ERROR",
            ])
            // Only the scenario's key: the agent's keys would be refused first.
            .args(["-o", "IdentitiesOnly=yes", "-i"])
            .arg(key)
            .args(["-p", &aricode_proto::net::TEST_SSH_FORWARD_PORT.to_string()])
            .arg("ari@127.0.0.1")
            .arg(cmd)
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) => {
                lines.push(format!("[host] ssh $ {cmd}"));
                for l in String::from_utf8_lossy(&o.stdout).lines() {
                    lines.push(format!("[host] ssh| {l}"));
                }
                for l in String::from_utf8_lossy(&o.stderr).lines() {
                    lines.push(format!("[host] ssh stderr: {l}"));
                }
                lines.push(format!("[host] ssh exit {}", o.status.code().unwrap_or(-1)));
            }
            Err(e) => lines.push(format!("[host] ssh: cannot run ssh ({e})")),
        }
    }
    drop(idle);
    lines.join("\n")
}

pub fn run_interactive(
    release: bool,
    cmdline: &str,
    cpus: u32,
    usb_image: bool,
    usb_hid: bool,
) -> Result {
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
        // As on a USB stick: the ESP inside the GPT disk, no `fat:`.
        image::write_usb_image(&disk, &esp, &[])?;
        None
    } else {
        image::write_test_disk(&disk, image::DiskVariant::Slots, &[])?;
        Some(esp.as_path())
    };
    let _responder = Responder::start()?;
    let usb = UsbOptions {
        hid: usb_hid,
        ..UsbOptions::default()
    };
    let mut q = base_command(esp_drive, &vars, &disk, false, cpus, usb);
    if cmdline.split_whitespace().any(|w| w == "gui") {
        // With `gui` the screen must be visible: QEMU window and serial in the terminal.
        println!("[xtask] QEMU with display (serial in the terminal; quit: close the window)");
        q.args(["-serial", "stdio"]);
    } else {
        println!("[xtask] QEMU (quit: Ctrl-A X)");
        q.arg("-nographic");
    }
    let st = q.status().map_err(|e| format!("qemu: {e}"))?;
    println!("[xtask] qemu exit: {:?}", st.code());
    Ok(())
}

pub struct Capture {
    pub serial: String,
    pub exit_code: Option<i32>,
    /// The runner killed QEMU on seeing `kill_on`.
    pub killed: bool,
    pub timed_out: bool,
    pub elapsed: Duration,
}

/// Output-driven input: each line is sent once the serial output has shown
/// `prompt` as many times as the lines already sent plus one.
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

/// Boots headless, captures the serial output, feeds `input` and waits up to
/// `timeout`.
/// Everything a scenario asks of the runner besides the ESP and the timeout.
pub struct Headless<'a> {
    pub input: Input<'a>,
    pub probe: Option<TcpProbe<'a>>,
    pub ssh_probe: Option<SshProbe<'a>>,
    pub input_probe: Option<InputProbe<'a>>,
    pub disk_variant: image::DiskVariant,
    pub disk_readonly: bool,
    /// Reuses the previous boot's test disk instead of building it again
    /// (multi-boot scenarios).
    pub keep_disk: bool,
    /// Kills QEMU (power cut) on seeing this text in the serial output.
    pub kill_on: Option<&'a str>,
    /// Files that go into the test disk's AriFS (installed packages).
    pub installed: &'a [(String, Vec<u8>)],
    pub cpus: u32,
    pub usb: UsbOptions,
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
        if !options.keep_disk {
            image::write_usb_image(&disk, esp, &[])?;
        }
        None
    } else {
        if !options.keep_disk {
            image::write_test_disk(&disk, disk_variant, options.installed)?;
        }
        Some(esp)
    };
    let _responder = Responder::start()?;
    let qmp_path = workdir.join("qmp.sock");
    let _ = std::fs::remove_file(&qmp_path);
    let start = Instant::now();
    let mut child = base_command(
        esp_drive,
        &vars,
        &disk,
        disk_readonly,
        options.cpus,
        options.usb,
    )
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
                Ok(n) => sink.lock().expect("capture").extend_from_slice(&chunk[..n]),
            }
        }
    });
    let mut sent = 0;
    let mut probe_result: Option<String> = None;
    let mut probe_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut input_result: Option<String> = None;
    let mut ssh_result: Option<String> = None;
    let mut ssh_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut input_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut timed_out = false;
    let mut killed = false;
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
        if let Some(marker) = options.kill_on {
            if count_occurrences(&captured.lock().expect("capture"), marker.as_bytes()) > 0 {
                killed = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        if sent < input.lines.len() {
            let prompts =
                count_occurrences(&captured.lock().expect("capture"), input.prompt.as_bytes());
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
                && count_occurrences(&captured.lock().expect("capture"), p.marker.as_bytes()) > 0
            {
                let payload = p.payload.to_string();
                probe_thread = Some(std::thread::spawn(move || run_tcp_probe(&payload)));
            }
            if probe_thread.as_ref().is_some_and(|t| t.is_finished()) {
                probe_result = probe_thread.take().and_then(|t| t.join().ok());
            }
        }
        if let Some(p) = options.ssh_probe.as_ref() {
            if ssh_thread.is_none()
                && ssh_result.is_none()
                && count_occurrences(&captured.lock().expect("capture"), p.marker.as_bytes()) > 0
            {
                let commands = p.commands.to_vec();
                let key = p.key.clone();
                let idle = p.idle_client;
                ssh_thread = Some(std::thread::spawn(move || {
                    run_ssh_probe(&commands, &key, idle)
                }));
            }
            if ssh_thread.as_ref().is_some_and(|t| t.is_finished()) {
                ssh_result = ssh_thread.take().and_then(|t| t.join().ok());
            }
        }
        if let Some(p) = input_probe {
            if input_thread.is_none()
                && input_result.is_none()
                && count_occurrences(&captured.lock().expect("capture"), p.marker.as_bytes()) > 0
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
    if let Some(t) = ssh_thread.take() {
        ssh_result = t.join().ok();
    }
    let _ = std::fs::remove_file(&qmp_path);
    reader.join().map_err(|_| "serial reader")?;
    let mut bytes = captured.lock().expect("capture").clone();
    for line in [probe_result, input_result, ssh_result]
        .into_iter()
        .flatten()
    {
        bytes.extend_from_slice(format!("\n{line}\n").as_bytes());
    }
    Ok(Capture {
        serial: strip_ansi(&String::from_utf8_lossy(&bytes)),
        exit_code,
        timed_out,
        killed,
        elapsed: start.elapsed(),
    })
}

/// Strips the escape sequences OVMF emits on the serial console.
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
        assert_eq!(super::count_occurrences(b"none", b"ari> "), 0);
        assert_eq!(super::count_occurrences(b"aaaa", b""), 0);
    }

    use super::strip_ansi;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[2J\x1b[01;01Hhola\x1b[=3h"), "hola");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }
}
