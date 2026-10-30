// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Launches QEMU + OVMF, interactively or capturing the serial output.

use crate::image::{self, OVMF_CODE};
use crate::{build_dir, root, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
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
    /// That xHCI offers both MSI and MSI-X.
    pub hid_msi_and_msix: bool,
    /// No virtio keyboard and mouse: QMP input reaches the USB HID devices
    /// (xHCI or EHCI).
    pub hid_only_input: bool,
    /// An extra EHCI (class 0c.03, interface 0x20): not xHCI.
    pub legacy_controller: bool,
    /// The test disk hangs off the xHCI as USB storage (`usb-storage`)
    /// instead of virtio-blk (ADR 0023).
    pub storage: bool,
    /// An EHCI with a USB keyboard and mouse (high speed) for the EHCI
    /// driver (ADR 0031).
    pub ehci_hid: bool,
    /// The test disk hangs off an EHCI as USB storage instead of virtio-blk.
    pub ehci_storage: bool,
    /// An ICH9 EHCI with a UHCI companion and a full-speed keyboard: the
    /// EHCI driver must hand the port over (EHCI 4.2.2).
    pub ehci_companion_kbd: bool,
}

/// Processors of the machine.
#[derive(Clone, Copy)]
struct Cpu {
    /// `-smp N` (at least 1).
    count: u32,
    /// `-cpu qemu64,+rdrand` instead of `qemu64` (TCG implements `rdrand`
    /// from the host's randomness; ADR 0039).
    rdrand: bool,
    /// [`SIMD_FEATURES`] added to the model (ADR 0065).
    simd: bool,
}

/// What `cpu_simd` adds to `qemu64`: XSAVE with AVX, so the kernel takes
/// its `XSAVE` path, plus the extensions vector and AES code looks for.
/// TCG implements all of them.
pub const SIMD_FEATURES: &str =
    "+xsave,+xsaveopt,+avx,+avx2,+aes,+pclmulqdq,+ssse3,+sse4.1,+sse4.2,+popcnt";

impl Cpu {
    /// The `-cpu` argument.
    fn model(&self) -> String {
        let mut model = String::from("qemu64");
        if self.rdrand {
            model.push_str(",+rdrand");
        }
        if self.simd {
            model.push(',');
            model.push_str(SIMD_FEATURES);
        }
        model
    }
}

/// Network cards of the machine and the host ports forwarded into it.
#[derive(Clone, Copy)]
struct Nics {
    /// The virtio-net card on the user network.
    virtio_net: bool,
    /// An unused igb, for the function that advertises FLR.
    igb_flr: bool,
    /// The user network's `hostfwd` ports.
    forwards: Forwards,
}

/// Host ports QEMU forwards (`hostfwd`) into the guest: chosen per
/// scenario run (`ports::PortLease`), fixed for `cargo xtask run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forwards {
    /// Host port forwarded to the guest's `TEST_TCP_LISTEN_PORT`.
    pub tcp: u16,
    /// Host port forwarded to the guest's SSH port 22.
    pub ssh: u16,
}

impl Forwards {
    /// `cargo xtask run`: fixed, so `ssh -p 17022 ari@127.0.0.1` reaches
    /// the interactive machine.
    pub const INTERACTIVE: Self = Self {
        tcp: 17009,
        ssh: 17022,
    };

    /// Host port forwarded to `guest_port`, if it has one.
    pub fn host_port(&self, guest_port: u16) -> Option<u16> {
        match guest_port {
            22 => Some(self.ssh),
            aricode_proto::net::TEST_TCP_LISTEN_PORT => Some(self.tcp),
            _ => None,
        }
    }
}

fn base_command(
    esp: Option<&Path>,
    vars: &Path,
    disk: &Path,
    disk_readonly: bool,
    cpu: Cpu,
    usb: UsbOptions,
    nics: Nics,
) -> Command {
    let mut q = Command::new("qemu-system-x86_64");
    q.current_dir(root());
    q.args([
        "-machine",
        "q35",
        "-cpu",
        &cpu.model(),
        "-smp",
        &cpu.count.max(1).to_string(),
        "-m",
        "512M",
        "-no-reboot",
    ]);
    // VT-d IOMMU (Phase 33) before any PCI device; the virtio devices carry
    // `iommu_platform=on` so their DMA goes through translation.
    q.args(["-device", "intel-iommu,aw-bits=48"]);
    // Test disk: modern virtio-blk (no legacy interface) for the user-space
    // driver or, with `usb.storage` / `usb.ehci_storage`, a USB stick on the
    // xHCI / the EHCI.
    q.args([
        "-drive",
        &format!(
            "if=none,id=disk0,format=raw,file={}{}",
            disk.display(),
            if disk_readonly { ",readonly=on" } else { "" }
        ),
    ]);
    if !usb.storage && !usb.ehci_storage {
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
    // host 10.0.2.2 forwards UDP/TCP to the same port of the host loopback,
    // where the test responder listens.
    q.args([
        "-netdev",
        &format!(
            "user,id=net0,hostfwd=tcp:127.0.0.1:{}-:{},hostfwd=tcp:127.0.0.1:{}-:22",
            nics.forwards.tcp,
            aricode_proto::net::TEST_TCP_LISTEN_PORT,
            nics.forwards.ssh
        ),
    ]);
    // Without `nic` the netdev stays (the capture filter hangs off it) but
    // the guest sees no network card: init runs without a network service.
    if nics.virtio_net {
        q.args([
            "-device",
            &format!(
                "virtio-net-pci,netdev=net0,mac={},disable-legacy=on,disable-modern=off,iommu_platform=on",
                GUEST_MAC.map(|b| format!("{b:02x}")).join(":")
            ),
        ]);
    }
    // A function that advertises FLR and that no driver claims (incident
    // 0004): QEMU's igb has the PCI Express capability with FLR. No
    // netdev: it is never used as a network card.
    if nics.igb_flr {
        q.args(["-device", "igb"]);
    }
    // virtio keyboard and mouse with fixed ids: QMP injects events per device.
    // Left out when the USB HID devices must be the only input (QEMU sends
    // QMP input to the most recently activated handler, and a virtio
    // keyboard activates when its guest driver starts).
    if !usb.hid_only_input {
        q.args([
            "-device",
            "virtio-keyboard-pci,id=kbd0,disable-legacy=on,disable-modern=off,iommu_platform=on",
        ]);
        q.args([
            "-device",
            "virtio-mouse-pci,id=mouse0,disable-legacy=on,disable-modern=off,iommu_platform=on",
        ]);
    }
    if usb.hid || usb.storage {
        // xHCI (Phase 25, part two) with a USB HID keyboard and mouse and/or
        // the test disk as USB storage.
        let xhci = match (usb.hid_msi, usb.hid_msi_and_msix) {
            (true, _) => "qemu-xhci,id=xhci,msi=on,msix=off",
            (false, true) => "qemu-xhci,id=xhci,msi=on,msix=on",
            (false, false) => "qemu-xhci,id=xhci",
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
    if usb.legacy_controller || usb.ehci_hid || usb.ehci_storage {
        // EHCI (class 0c.03, interface 0x20, ADR 0031); QEMU's is ICH4-like,
        // with no companion controllers and no MSI.
        q.args(["-device", "usb-ehci,id=ehci"]);
        if usb.ehci_hid {
            q.args(["-device", "usb-kbd,bus=ehci.0"]);
            q.args(["-device", "usb-mouse,bus=ehci.0"]);
        }
        if usb.ehci_storage {
            q.args(["-device", "usb-storage,bus=ehci.0,drive=disk0"]);
        }
    }
    if usb.ehci_companion_kbd {
        q.args(["-device", "ich9-usb-ehci1,id=ehcic"]);
        q.args(["-device", "ich9-usb-uhci1,masterbus=ehcic.0,firstport=0"]);
        q.args(["-device", "usb-kbd,bus=ehcic.0,usb_version=1"]);
    }
    q.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    q
}

/// QMP hot-plug: `delay` after `marker` appears in the serial output, the
/// device `driver` is added on `bus` with `id` (`device_add`).
pub struct Hotplug<'a> {
    pub marker: &'a str,
    pub delay: Duration,
    pub driver: &'a str,
    pub bus: &'a str,
    pub id: &'a str,
}

fn run_hotplug(path: &Path, driver: &str, bus: &str, id: &str) -> String {
    // Names checked by the scenario loader: plain names, safe in JSON.
    let command = format!(
        r#"{{"execute":"device_add","arguments":{{"driver":"{driver}","bus":"{bus}","id":"{id}"}}}}"#
    );
    match Qmp::connect(path).and_then(|mut q| q.execute(&command)) {
        Ok(()) => format!("[host] hotplug: {driver} added on {bus} as {id}"),
        Err(e) => format!("[host] hotplug: {driver} on {bus} failed: {e}"),
    }
}

/// QMP input injection: on seeing `marker` in the serial output, `keys` (QEMU
/// qcode names) are typed and the mouse is moved and clicked.
pub struct InputProbe<'a> {
    pub marker: &'a str,
    pub keys: &'a [String],
    pub mouse_rel: Option<(i32, i32)>,
    pub click: Option<&'a str>,
    /// Minimum pause after each key event (ms); none by default.
    pub key_delay_ms: u64,
    /// The guest prints this before reading each line and echoes every
    /// typed character: each line waits for it and each key for its echo
    /// before the next one goes out (`typing`, incident 0022).
    pub echo_prompt: Option<&'a str>,
}

/// What typing with echo waits needs: the prompt, the serial capture to
/// watch, the flag that QEMU has exited and QEMU's pid (a stopped QEMU,
/// paused by the thermal guard, does not use up the wait).
struct EchoWait {
    prompt: String,
    capture: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
    qemu_pid: u32,
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
    /// Several events go in one command: QEMU queues them all in the
    /// device before it answers, with nothing of the guest's in between.
    fn send_events(&mut self, events: &[String]) -> std::io::Result<()> {
        self.execute(&format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{}]}}}}"#,
            events.join(",")
        ))
    }
}

fn run_input_probe(
    path: &Path,
    keys: &[String],
    mouse_rel: Option<(i32, i32)>,
    click: Option<&str>,
    key_delay: Duration,
    echo: Option<EchoWait>,
) -> String {
    use crate::typing::{self, Step};
    let mut qmp = match Qmp::connect(path) {
        Ok(q) => q,
        Err(e) => return format!("[host] QMP: no connection ({e})"),
    };
    let mut sent = 0;
    let mut step = |qmp: &mut Qmp, events: &[String]| -> Result {
        qmp.send_events(events).map_err(|e| e.to_string())?;
        sent += events.len();
        Ok(())
    };
    let result: Result = (|| {
        // Natural order: point, click, then type.
        if let Some((dx, dy)) = mouse_rel {
            step(
                &mut qmp,
                &[format!(
                    r#"{{"type":"rel","data":{{"axis":"x","value":{dx}}}}}"#
                )],
            )?;
            step(
                &mut qmp,
                &[format!(
                    r#"{{"type":"rel","data":{{"axis":"y","value":{dy}}}}}"#
                )],
            )?;
        }
        // A short pause between phases: the compositor orders by arrival time
        // and a real user does not click and type in the same millisecond.
        std::thread::sleep(Duration::from_millis(100));
        if let Some(button) = click {
            for down in [true, false] {
                step(
                    &mut qmp,
                    &[format!(
                        r#"{{"type":"btn","data":{{"down":{down},"button":"{button}"}}}}"#
                    )],
                )?;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        // Without an echo to follow, every key is only paced by the delay.
        let steps = match echo {
            Some(_) => typing::plan(keys),
            None => keys.iter().map(|k| Step::Key(k)).collect(),
        };
        let mut cursor = typing::Cursor::default();
        for s in steps {
            let needle = match s {
                Step::Key(key) => {
                    // "a" presses and releases; "shift+dot" presses the
                    // modifiers, then the key, and releases them in reverse
                    // order. The whole key is one QMP command (incident
                    // 0031): a guest that acts on the press (Enter on
                    // `shutdown`) may power off at once, and a release sent
                    // as a command of its own then found QEMU gone ("failed
                    // after 29 events, Broken pipe"). The device queues
                    // them in order, so the guest still sees press before
                    // release, and the pause after the key is the same
                    // `key_delay` per event as before.
                    let parts: Vec<&str> = key.split('+').collect();
                    let presses = parts.iter().map(|p| (p, true));
                    let releases = parts.iter().rev().map(|p| (p, false));
                    let events: Vec<String> = presses
                        .chain(releases)
                        .map(|(part, down)| {
                            format!(
                                r#"{{"type":"key","data":{{"down":{down},"key":{{"type":"qcode","data":"{part}"}}}}}}"#
                            )
                        })
                        .collect();
                    step(&mut qmp, &events)?;
                    std::thread::sleep(key_delay * events.len() as u32);
                    continue;
                }
                Step::Prompt => echo.as_ref().map(|e| e.prompt.as_bytes().to_vec()),
                Step::Echo(c) => Some(vec![c]),
            };
            if let (Some(e), Some(needle)) = (echo.as_ref(), needle) {
                let stopped = || typing::process_stopped(e.qemu_pid);
                let guest = typing::Guest {
                    capture: &e.capture,
                    gone: &e.gone,
                    stopped: &stopped,
                };
                typing::wait_past(&mut cursor, &guest, &needle, typing::ECHO_TIMEOUT)?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => format!("[host] QMP: {sent} input events sent"),
        Err(e) => format!("[host] QMP: failed after {sent} events ({e})"),
    }
}

/// Test responder on the host loopback, at the ports the guest's test
/// programs have compiled in (`aricode_proto::net::TEST_*`): over UDP it
/// returns a fixed A record for DNS queries and echoes everything else;
/// over TCP it echoes every connection; the test HTTP server serves
/// [`http_serve`]'s routes. Those ports cannot be chosen per run (see
/// `ports`), so one responder serves every scenario of a suite, and a
/// second suite in the same network namespace is refused at start. It
/// lives as long as the returned value exists.
pub struct Responder {
    _servers: Servers,
}

/// Server threads on the host loopback, stopped and joined when dropped.
struct Servers {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// One `103 Early Hints` of the `/early-hints` routes.
const EARLY_HINTS: &[u8] = b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n";
/// Pause between two interim responses of `/early-hints`: under the
/// shell's per-operation timeout (8 s) and slow enough that its 30 s budget
/// runs out before the interim-response limit (8) is reached.
const EARLY_HINTS_PAUSE: Duration = Duration::from_secs(5);

/// Test HTTP server: `/hola` (Content-Length), `/chunked`
/// (Transfer-Encoding: chunked), `/big-headers` (5 KiB of headers, the
/// size github.com sends), `/early-hints` (a `103` every five seconds and
/// never a final response: the server of incident 0009, which the
/// client's budget must cut), `/early-hints-flood` (twenty `103` at once,
/// then a 200: the interim-response limit), `/update/NAME` from `update`
/// (a scenario's own update source; 404 without one), the same source
/// under the public source's URL shape ([`mirror_route`], ADR 0048), the
/// redirect limits (`/redirect-loop` redirects to itself and `/to-ftp` to
/// an `ftp://` URL) and 404 for everything else.
fn http_serve(mut stream: std::net::TcpStream, update: Option<&Path>) {
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
    let Some((_, target)) = request_line(&buf[..have]) else {
        let _ = stream.write_all(BAD_REQUEST);
        return;
    };
    // Routing ignores the query, as a file server does.
    let path = target.split('?').next().unwrap_or("/");
    if path == "/early-hints" {
        // Until the client gives up and closes (the write then fails).
        while stream
            .write_all(EARLY_HINTS)
            .and_then(|()| stream.flush())
            .is_ok()
        {
            std::thread::sleep(EARLY_HINTS_PAUSE);
        }
        return;
    }
    let response: Vec<u8> = match path {
        "/hola" => b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 19\r\nConnection: close\r\n\r\nHola desde el host\n".to_vec(),
        "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nd\r\nprimer trozo,\r\ne\r\n segundo trozo\r\n0\r\n\r\n".to_vec(),
        "/early-hints-flood" => {
            let mut r = EARLY_HINTS.repeat(20);
            r.extend(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nflooded\n");
            r
        }
        "/big-headers" => {
            let mut r = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n".to_vec();
            for i in 0..40 {
                r.extend(format!("X-Filler-{i:02}: {}\r\n", "x".repeat(110)).as_bytes());
            }
            r.extend(b"Content-Length: 15\r\nConnection: close\r\n\r\nbig headers ok\n");
            r
        }
        // The scenario's source under the public source's URL shape, with
        // GitHub's redirects (ADR 0048), as `serve-update` serves it.
        p if update.is_some() && mirror_route(p).is_some() => {
            match mirror_route(p).expect("checked") {
                MirrorRoute::Redirect(r) => r,
                MirrorRoute::Asset(name) => file_response(update.expect("checked"), name),
            }
        }
        "/redirect-loop" => redirect("302 Found", "/redirect-loop", 0),
        "/to-ftp" => redirect("301 Moved Permanently", "ftp://10.0.2.2/index", 0),
        // The scenario's update channel (ADR 0027), never the real one.
        p if p.starts_with("/update/") && update.is_some() => {
            file_response(update.expect("checked"), &p["/update/".len()..])
        }
        // Its development source beside it (ADR 0045).
        p if p.starts_with("/dev/") && update.is_some() => file_response(
            &update.expect("checked").with_file_name(DEV_SOURCE_DIR),
            &p["/dev/".len()..],
        ),
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nno existe".to_vec(),
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
    // The mirror behaves like a server across a real network: its close
    // comes after the client's, so the machine closes first and holds the
    // connection in TIME-WAIT (incident 0027).
    if update.is_some() && mirror_route(path).is_some() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut rest = [0u8; 64];
        while matches!(stream.read(&mut rest), Ok(n) if n > 0) {}
    }
}

/// The answer to a request whose first line is not one [`request_line`]
/// accepts.
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 11\r\nConnection: close\r\n\r\nbad request";

/// The method and target of an HTTP request, from its first line: `METHOD
/// SP target SP HTTP/x.y CRLF`, the method upper-case letters, the target
/// visible ASCII only (RFC 9112, 3.2: no space, no control byte). `None`
/// for anything else, which is answered 400: no byte a client sends
/// reaches a response header or a log line raw (incident 0045).
fn request_line(request: &[u8]) -> Option<(&str, &str)> {
    let line = request.split(|&b| b == b'\n').next()?.strip_suffix(b"\r")?;
    let mut parts = line.split(|&b| b == b' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    let visible = |b: &u8| (0x21..=0x7E).contains(b);
    let valid = parts.next().is_none()
        && !method.is_empty()
        && method.iter().all(u8::is_ascii_uppercase)
        && version.starts_with(b"HTTP/")
        && version.iter().all(visible)
        && !target.is_empty()
        && target.iter().all(visible);
    let text = |b| std::str::from_utf8(b).ok();
    valid
        .then(|| Some((text(method)?, text(target)?)))
        .flatten()
}

/// `bytes` for a log line: printable ASCII as it is (a backslash
/// doubled), any other byte as `\xNN`, so a client cannot write control
/// sequences or line breaks into the host's terminal or log (incident
/// 0045).
fn log_escaped(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            b'\\' => String::from("\\\\"),
            0x20..=0x7E => char::from(b).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect()
}

/// A file name the update servers serve: letters, digits, `-`, `.` and
/// `_`, never a path. Only such a name goes into a response, as a file or
/// in a `Location`.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

/// The public source's path ([`crate::image::PUBLIC_SOURCE`]), served
/// locally by [`mirror_route`].
pub const MIRROR_LATEST: &str = "/AriCodeLabs/AriCodeOS/releases/latest/download/";
/// The release a local mirror's `latest` points to.
const MIRROR_TAG: &str = "/AriCodeLabs/AriCodeOS/releases/download/local/";
/// The local mirror's stand-in for GitHub's asset host.
const MIRROR_ASSETS: &str = "/release-assets/";

/// What the local mirror answers for a path.
enum MirrorRoute<'a> {
    Redirect(Vec<u8>),
    /// A published file, by name.
    Asset(&'a str),
}

/// The local mirror of the public source (ADR 0048): a publication served
/// at [`MIRROR_LATEST`] with GitHub's two redirects, so a machine or a
/// scenario follows the very path it follows to GitHub without anything
/// published there. `latest` answers 302 to the release, with a
/// 9,243-byte header line as github.com sends; the release answers 302 to
/// the asset with a 900-byte signed query, as GitHub's asset URLs; the
/// asset is the file. `None` for other paths, and for names that are not
/// [`plain_name`]s: a redirect carries nothing else in its `Location`.
fn mirror_route(path: &str) -> Option<MirrorRoute<'_>> {
    let named = |prefix: &str| path.strip_prefix(prefix).filter(|n| plain_name(n));
    if let Some(name) = named(MIRROR_LATEST) {
        return Some(MirrorRoute::Redirect(redirect(
            "302 Found",
            &format!("{MIRROR_TAG}{name}"),
            9243,
        )));
    }
    if let Some(name) = named(MIRROR_TAG) {
        return Some(MirrorRoute::Redirect(redirect(
            "302 Found",
            &format!("{MIRROR_ASSETS}{name}?sig={}", "s".repeat(900)),
            0,
        )));
    }
    named(MIRROR_ASSETS).map(MirrorRoute::Asset)
}

/// A redirect to `location` with a body that must not reach the client's
/// sink and, when `filler` is not zero, one header line of that many bytes.
fn redirect(status: &str, location: &str, filler: usize) -> Vec<u8> {
    let mut r = format!("HTTP/1.1 {status}\r\n").into_bytes();
    if filler > 0 {
        r.extend(b"Content-Security-Policy: ");
        r.extend(std::iter::repeat_n(b'p', filler));
        r.extend(b"\r\n");
    }
    r.extend(
        format!(
            "Location: {location}\r\nContent-Length: 16\r\nConnection: close\r\n\r\nredirect body!!\n"
        )
        .as_bytes(),
    );
    r
}

/// A scenario's development source: this directory beside its `update/`.
pub const DEV_SOURCE_DIR: &str = "dev";

/// `200` with the file `name` of `dir` (plain names only), or `404`.
pub fn file_response(dir: &Path, name: &str) -> Vec<u8> {
    match plain_name(name)
        .then(|| std::fs::read(dir.join(name)).ok())
        .flatten()
    {
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
        "[xtask] serving {} on port {port}; on the machine: update add http://<this host>:{port}/ \
         (or http://<this host>:{port}{MIRROR_LATEST} for the public source's path and redirects, ADR 0048)",
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
    let request = &buf[..have];
    let (response, what) = match request_line(request) {
        Some((method, target)) => {
            let path = target.split('?').next().unwrap_or("/");
            let response = match mirror_route(path) {
                Some(MirrorRoute::Redirect(r)) => r,
                Some(MirrorRoute::Asset(asset)) => file_response(dir, asset),
                None => file_response(dir, path.trim_start_matches('/')),
            };
            (response, format!("{method} {target}"))
        }
        None => {
            let line = request.split(|&b| b == b'\n').next().unwrap_or_default();
            let shown = log_escaped(&line[..line.len().min(200)]);
            (
                BAD_REQUEST.to_vec(),
                format!("bad request line \"{shown}\""),
            )
        }
    };
    let outcome = match stream.write_all(&response).and_then(|()| stream.flush()) {
        Ok(()) => format!("{} bytes sent", response.len()),
        Err(e) => format!("failed: {e}"),
    };
    println!("[xtask] {peer} {what}: {outcome}");
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

/// The bind error of a fixed test port, naming the likely holder.
fn fixed_port_error(what: &str, port: u16, e: std::io::Error) -> String {
    format!(
        "{what} on 127.0.0.1:{port}: {e}; the guest's test programs have this port compiled in, \
         so one test suite runs per network namespace: is another suite running here? \
         Run this one in Docker (README.md, \"Docker\") or wait for the other"
    )
}

impl Responder {
    pub fn start() -> Result<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let port = aricode_proto::net::TEST_RESPONDER_PORT;
        let socket = std::net::UdpSocket::bind(("127.0.0.1", port))
            .map_err(|e| fixed_port_error("UDP responder", port, e))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let tcp_port = aricode_proto::net::TEST_TCP_ECHO_PORT;
        let listener = std::net::TcpListener::bind(("127.0.0.1", tcp_port))
            .map_err(|e| fixed_port_error("TCP echo server", tcp_port, e))?;
        let http_port = aricode_proto::net::TEST_HTTP_PORT;
        let http_listener = std::net::TcpListener::bind(("127.0.0.1", http_port))
            .map_err(|e| fixed_port_error("test HTTP server", http_port, e))?;
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
        let tcp = accept_loop(Arc::clone(&stop), listener, Arc::new(tcp_echo))?;
        let http = accept_loop(
            Arc::clone(&stop),
            http_listener,
            Arc::new(|s| http_serve(s, None)),
        )?;
        Ok(Self {
            _servers: Servers {
                stop,
                threads: vec![udp, tcp, http],
            },
        })
    }
}

/// A scenario's own test HTTP server (`host_update`): [`http_serve`]'s
/// routes plus `/update/` from the scenario's channel directory, on a port
/// the kernel picks, so concurrent scenarios each serve their own channel.
pub struct UpdateServer {
    port: u16,
    _servers: Servers,
    /// Keeps the port out of every other lease while the server runs.
    _lease: crate::ports::PortLease,
}

impl UpdateServer {
    pub fn start(channel: &Path) -> Result<Self> {
        let (listener, lease) =
            crate::ports::PortLease::listener().map_err(|e| format!("update server: {e}"))?;
        let port = lease.ports()[0];
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let channel = channel.to_path_buf();
        let thread = accept_loop(
            std::sync::Arc::clone(&stop),
            listener,
            std::sync::Arc::new(move |s| http_serve(s, Some(&channel))),
        )?;
        Ok(Self {
            port,
            _servers: Servers {
                stop,
                threads: vec![thread],
            },
            _lease: lease,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Accepts connections until `stop` and serves each one on its own thread.
fn accept_loop(
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    listener: std::net::TcpListener,
    serve: std::sync::Arc<dyn Fn(std::net::TcpStream) + Send + Sync>,
) -> Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(std::thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let serve = std::sync::Arc::clone(&serve);
                    std::thread::spawn(move || serve(stream));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }))
}

impl Drop for Servers {
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

fn run_tcp_probe(port: u16, payload: &str) -> String {
    use std::io::Write;
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

/// Host connection flood (incident 0013): on seeing `marker` the runner
/// opens `connections` plain TCP connections to the host port forwarded to
/// the guest's `port`, never sends a byte on them, counts the ones the
/// guest resets within [`FLOOD_SETTLE`] and holds the rest until QEMU
/// exits (their closing at exit is QEMU's and is not counted). QEMU's
/// user networking completes the TCP handshake with the guest itself, so
/// from the guest's view these are established connections nobody accepts
/// (its listener's backlog), never half-open ones: a true SYN flood cannot
/// be sent through `hostfwd`. The scenario's console lines are typed only
/// once the connections are open and settled, so the guest commands run
/// with the table in that state.
pub struct FloodProbe<'a> {
    pub marker: &'a str,
    /// Guest port: 22 or `TEST_TCP_LISTEN_PORT`, the two with a `hostfwd`
    /// ([`Forwards`]).
    pub port: u16,
    pub connections: usize,
}

/// How long the flood watches its fresh connections for a reset by the
/// guest before releasing the console: the guest answers a refused SYN at
/// once and user networking relays the reset within milliseconds.
const FLOOD_SETTLE: Duration = Duration::from_secs(2);

/// The flood towards the guest's `guest_port` through its `hostfwd`.
fn run_flood_probe(
    forwards: Forwards,
    guest_port: u16,
    connections: usize,
    ready: &std::sync::atomic::AtomicBool,
    stop: &std::sync::atomic::AtomicBool,
) -> String {
    let Some(host_port) = forwards.host_port(guest_port) else {
        ready.store(true, std::sync::atomic::Ordering::SeqCst);
        return format!("[host] flood: guest port {guest_port} has no hostfwd");
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], host_port));
    hold_connections(addr, guest_port, connections, FLOOD_SETTLE, ready, stop)
}

/// Opens `connections` to `addr`, reads each one for `settle` to count the
/// ones the peer closes or resets, raises `ready`, then holds the rest
/// until `stop`. The result is one block of `[host] flood` lines.
fn hold_connections(
    addr: std::net::SocketAddr,
    guest_port: u16,
    connections: usize,
    settle: Duration,
    ready: &std::sync::atomic::AtomicBool,
    stop: &std::sync::atomic::AtomicBool,
) -> String {
    use std::sync::atomic::Ordering;
    let mut lines = Vec::new();
    let mut streams: Vec<Option<std::net::TcpStream>> = Vec::new();
    for _ in 0..connections {
        match std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(s) => {
                let _ = s.set_nonblocking(true);
                streams.push(Some(s));
            }
            Err(e) => {
                lines.push(format!(
                    "[host] flood: connection refused by the host ({e})"
                ));
                break;
            }
        }
    }
    let opened = streams.len();
    lines.push(format!(
        "[host] flood: {opened} connections to guest port {guest_port} opened"
    ));
    let mut closed = 0;
    let mut buf = [0u8; 256];
    let settle_end = Instant::now() + settle;
    while Instant::now() < settle_end {
        for slot in streams.iter_mut() {
            let Some(s) = slot else { continue };
            match s.read(&mut buf) {
                // Data (a banner from the accepting service) keeps it open.
                Ok(n) if n > 0 => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Ok(_) | Err(_) => {
                    closed += 1;
                    *slot = None;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    lines.push(format!(
        "[host] flood: {closed} of {opened} connections reset by the guest within {} ms",
        settle.as_millis()
    ));
    ready.store(true, Ordering::SeqCst);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(streams);
    lines.join("\n")
}

/// SSH probe: commands run with the host's `ssh` and the scenario's key.
pub struct SshProbe<'a> {
    pub marker: &'a str,
    pub commands: &'a [String],
    pub key: std::path::PathBuf,
    pub idle_client: bool,
    pub retry_connect_secs: u64,
}

/// Runs each command over SSH; the result is one block of `[host] ssh`
/// lines: the command, every output line and the exit status.
fn run_ssh_probe(
    port: u16,
    commands: &[String],
    key: &Path,
    idle_client: bool,
    retry_connect_secs: u64,
) -> String {
    let mut lines = Vec::new();
    // An idle client that holds a connection without identifying itself;
    // kept open for the whole probe (sshd must drop it on its own).
    let idle = idle_client.then(|| {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
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
        // ssh exits with 255 when it could not connect or log in: tried
        // again for `retry_connect_secs` (sshd being relaunched).
        let deadline = Instant::now() + Duration::from_secs(retry_connect_secs);
        let mut retries = 0;
        let out = loop {
            let out = ssh_command(port, key, cmd);
            match &out {
                Ok(o) if o.status.code() == Some(255) && Instant::now() < deadline => {
                    retries += 1;
                    std::thread::sleep(Duration::from_secs(1));
                }
                _ => break out,
            }
        };
        if retries > 0 {
            lines.push(format!("[host] ssh connection retried {retries} time(s)"));
        }
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

/// One command through the host's OpenSSH client with the scenario's key.
fn ssh_command(port: u16, key: &Path, cmd: &str) -> std::io::Result<std::process::Output> {
    Command::new("ssh")
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
        .args(["-p", &port.to_string()])
        .arg("ari@127.0.0.1")
        .arg(cmd)
        .stdin(Stdio::null())
        .output()
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
        println!(
            "{}",
            image::write_usb_image(&disk, &esp, &[], aricode_gpt::TEST_DISK_GUID)?
        );
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
    let mut q = base_command(
        esp_drive,
        &vars,
        &disk,
        false,
        Cpu {
            count: cpus,
            rdrand: false,
            simd: false,
        },
        usb,
        Nics {
            virtio_net: true,
            igb_flr: false,
            forwards: Forwards::INTERACTIVE,
        },
    );
    println!(
        "[xtask] host 127.0.0.1:{} -> guest port 22 (ssh -p {} ari@127.0.0.1)",
        Forwards::INTERACTIVE.ssh,
        Forwards::INTERACTIVE.ssh
    );
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

/// MAC address of the guest's virtio-net (QEMU's default for the first
/// NIC, pinned so the network capture can tell the guest's frames apart).
pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// Frames the guest sent in a pcap capture of its network (QEMU
/// `filter-dump`), of any ethertype (IPv4, IPv6, VLAN-tagged or anything
/// else), except ARP and DHCP (IPv4 UDP 68 -> 67). A boot without
/// `selftest` must send none of them (incident 0005): every one is a
/// contact with the test hosts or with something else. `None` if the
/// capture is not a readable Ethernet pcap; a truncated last record (QEMU
/// killed) ends the count.
pub fn guest_frames(pcap: &[u8], guest: [u8; 6]) -> Option<usize> {
    let header = pcap.get(..24)?;
    let little = match header[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
        _ => return None,
    };
    let u32_at = |b: &[u8]| {
        let b: [u8; 4] = b.try_into().expect("4 bytes");
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    if u32_at(&header[20..24]) != 1 {
        return None; // not Ethernet
    }
    let mut count = 0;
    let mut rest = &pcap[24..];
    while rest.len() >= 16 {
        let len = u32_at(&rest[8..12]) as usize;
        let Some(frame) = rest.get(16..16 + len) else {
            break;
        };
        rest = &rest[16 + len..];
        if frame.get(6..12) == Some(&guest[..]) && !is_arp_or_dhcp(frame) {
            count += 1;
        }
    }
    Some(count)
}

/// ARP or DHCP (IPv4, UDP 68 -> 67), after any 802.1Q/802.1ad tags.
fn is_arp_or_dhcp(frame: &[u8]) -> bool {
    let mut at = 12;
    while let Some(&[a, b]) = frame.get(at..at + 2) {
        match [a, b] {
            [0x81, 0x00] | [0x88, 0xA8] => at += 4,
            [0x08, 0x06] => return true,
            [0x08, 0x00] => {
                let ip = &frame[at + 2..];
                let Some(&first) = ip.first() else {
                    return false;
                };
                let ihl = (first & 0x0F) as usize * 4;
                return ip.get(9) == Some(&17) && ip.get(ihl..ihl + 4) == Some(&[0, 68, 0, 67][..]);
            }
            _ => return false,
        }
    }
    false
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

/// Everything a scenario asks of the runner besides the ESP and the timeout.
pub struct Headless<'a> {
    pub input: Input<'a>,
    pub probe: Option<TcpProbe<'a>>,
    pub ssh_probe: Option<SshProbe<'a>>,
    pub flood: Option<FloodProbe<'a>>,
    pub input_probe: Option<InputProbe<'a>>,
    pub hotplug: Option<Hotplug<'a>>,
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
    /// `-cpu qemu64,+rdrand` instead of `qemu64` (scenario `cpu_rdrand`).
    pub cpu_rdrand: bool,
    /// [`SIMD_FEATURES`] added to the CPU model (scenario `cpu_simd`).
    pub cpu_simd: bool,
    pub usb: UsbOptions,
    /// The virtio-net card is present (every scenario but `no_nic` ones).
    pub nic: bool,
    /// An unused function that advertises FLR (`flr_device`).
    pub flr_device: bool,
    /// This run's `hostfwd` ports.
    pub forwards: Forwards,
}

/// Boots headless, captures the serial output, feeds `input` and waits up to
/// `timeout`. QEMU's own messages (stderr) go to `stderr_log`. The fixed
/// test hosts (`Responder`) must be running: the suite starts them once.
pub fn run_headless(
    esp: &Path,
    workdir: &Path,
    stderr_log: &Path,
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
            image::write_usb_image(&disk, esp, &[], aricode_gpt::TEST_DISK_GUID)?;
        }
        None
    } else {
        if !options.keep_disk {
            image::write_test_disk(&disk, disk_variant, options.installed)?;
        }
        Some(esp)
    };
    let qmp = QmpSocket::new(workdir);
    let stderr =
        std::fs::File::create(stderr_log).map_err(|e| format!("{}: {e}", stderr_log.display()))?;
    // Every frame on the guest's network, for the count of guest
    // frames appended to the capture.
    let pcap_path = workdir.join("net.pcap");
    let _ = std::fs::remove_file(&pcap_path);
    let start = Instant::now();
    let mut child = base_command(
        esp_drive,
        &vars,
        &disk,
        disk_readonly,
        Cpu {
            count: options.cpus,
            rdrand: options.cpu_rdrand,
            simd: options.cpu_simd,
        },
        options.usb,
        Nics {
            virtio_net: options.nic,
            igb_flr: options.flr_device,
            forwards: options.forwards,
        },
    )
    .args([
        "-object",
        &format!(
            "filter-dump,id=netdump,netdev=net0,file={}",
            pcap_path.display()
        ),
    ])
    .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
    .args([
        "-qmp",
        &format!("unix:{},server=on,wait=off", qmp.path().display()),
    ])
    .stdin(if input.lines.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    })
    .stdout(Stdio::piped())
    .stderr(stderr)
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
    let mut hotplug_result: Option<String> = None;
    // When the hot-plug marker was first seen.
    let mut hotplug_seen: Option<Instant> = None;
    let mut ssh_result: Option<String> = None;
    let mut ssh_thread: Option<std::thread::JoinHandle<String>> = None;
    let mut input_thread: Option<std::thread::JoinHandle<String>> = None;
    // The flood holds its connections until QEMU is gone (`flood_stop`);
    // the console lines wait for `flood_ready`.
    let flood_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flood_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Typing that waits for echoes stops waiting once QEMU is gone.
    let qemu_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut flood_thread: Option<std::thread::JoinHandle<String>> = None;
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
        let input_held =
            options.flood.is_some() && !flood_ready.load(std::sync::atomic::Ordering::SeqCst);
        if sent < input.lines.len() && !input_held {
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
                let port = options.forwards.tcp;
                probe_thread = Some(std::thread::spawn(move || run_tcp_probe(port, &payload)));
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
                let retry = p.retry_connect_secs;
                let port = options.forwards.ssh;
                ssh_thread = Some(std::thread::spawn(move || {
                    run_ssh_probe(port, &commands, &key, idle, retry)
                }));
            }
            if ssh_thread.as_ref().is_some_and(|t| t.is_finished()) {
                ssh_result = ssh_thread.take().and_then(|t| t.join().ok());
            }
        }
        if let Some(f) = options.flood.as_ref() {
            if flood_thread.is_none()
                && count_occurrences(&captured.lock().expect("capture"), f.marker.as_bytes()) > 0
            {
                let (port, n) = (f.port, f.connections);
                let (ready, stop) = (Arc::clone(&flood_ready), Arc::clone(&flood_stop));
                let forwards = options.forwards;
                flood_thread = Some(std::thread::spawn(move || {
                    run_flood_probe(forwards, port, n, &ready, &stop)
                }));
            }
        }
        if let (Some(h), None) = (options.hotplug.as_ref(), &hotplug_result) {
            if hotplug_seen.is_none()
                && count_occurrences(&captured.lock().expect("capture"), h.marker.as_bytes()) > 0
            {
                hotplug_seen = Some(Instant::now());
            }
            if hotplug_seen.is_some_and(|t| t.elapsed() >= h.delay) {
                hotplug_result = Some(run_hotplug(qmp.path(), h.driver, h.bus, h.id));
            }
        }
        if let Some(p) = input_probe {
            if input_thread.is_none()
                && input_result.is_none()
                && count_occurrences(&captured.lock().expect("capture"), p.marker.as_bytes()) > 0
            {
                let path = qmp.path().to_path_buf();
                let keys = p.keys.to_vec();
                let rel = p.mouse_rel;
                let click = p.click.map(String::from);
                let delay = Duration::from_millis(p.key_delay_ms);
                let echo = p.echo_prompt.map(|prompt| EchoWait {
                    prompt: prompt.to_string(),
                    capture: Arc::clone(&captured),
                    gone: Arc::clone(&qemu_gone),
                    qemu_pid: child.id(),
                });
                input_thread = Some(std::thread::spawn(move || {
                    run_input_probe(&path, &keys, rel, click.as_deref(), delay, echo)
                }));
            }
            if input_thread.as_ref().is_some_and(|t| t.is_finished()) {
                input_result = input_thread.take().and_then(|t| t.join().ok());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    qemu_gone.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(t) = probe_thread.take() {
        probe_result = t.join().ok();
    }
    if let Some(t) = input_thread.take() {
        input_result = t.join().ok();
    }
    if let Some(t) = ssh_thread.take() {
        ssh_result = t.join().ok();
    }
    flood_stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let flood_result = match flood_thread.take() {
        Some(t) => t.join().ok(),
        None => options.flood.as_ref().map(|f| {
            format!(
                "[host] flood: marker '{}' never seen; no connections opened",
                f.marker
            )
        }),
    };
    drop(qmp);
    reader.join().map_err(|_| "serial reader")?;
    let mut bytes = captured.lock().expect("capture").clone();
    let hotplug_result = hotplug_result.or_else(|| {
        options.hotplug.as_ref().map(|h| {
            if hotplug_seen.is_some() {
                return format!(
                    "[host] hotplug: marker '{}' seen, but QEMU ended before the delay; nothing added",
                    h.marker
                );
            }
            format!(
                "[host] hotplug: marker '{}' never seen; nothing added",
                h.marker
            )
        })
    });
    for line in [
        probe_result,
        input_result,
        ssh_result,
        flood_result,
        hotplug_result,
    ]
    .into_iter()
    .flatten()
    {
        bytes.extend_from_slice(format!("\n{line}\n").as_bytes());
    }
    let frames = std::fs::read(&pcap_path)
        .ok()
        .and_then(|pcap| guest_frames(&pcap, GUEST_MAC));
    bytes.extend_from_slice(
        match frames {
            Some(n) => format!("\n[host] guest frames other than ARP and DHCP: {n}\n"),
            None => String::from("\n[host] guest frames: no readable capture\n"),
        }
        .as_bytes(),
    );
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

/// Longest UNIX socket path Linux and QEMU accept: `sun_path` holds 108
/// bytes with the terminator.
const SUN_PATH_MAX: usize = 107;

/// Where the QMP socket of a scenario run in `workdir` goes: next to its
/// logs, unless that path does not fit a socket address (a deep checkout,
/// such as a git worktree under `.claude/worktrees/`: QEMU refuses it and
/// the host cannot connect, so the scenario fails before or without its
/// input events), in which case a short unique path in the system's
/// temporary directory. `QmpSocket` removes it after the run either way.
pub fn qmp_socket_path(workdir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let local = workdir.join("qmp.sock");
    if local.as_os_str().len() <= SUN_PATH_MAX {
        return local;
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("aricode-qmp-{}-{n}.sock", std::process::id()))
}

/// The QMP socket of one run: QEMU creates it at `path()`, the guard
/// removes it when dropped, so every exit of the run (`?`, a panic)
/// cleans up a fallback socket in the temporary directory too. Only a
/// killed process leaves one behind.
struct QmpSocket(PathBuf);

impl QmpSocket {
    fn new(workdir: &Path) -> Self {
        let path = qmp_socket_path(workdir);
        let _ = std::fs::remove_file(&path);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for QmpSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{qmp_socket_path, QmpSocket};
    use std::path::Path;

    #[test]
    fn qmp_socket_guard_removes_the_file() {
        let deep = Path::new("/")
            .join("d".repeat(99))
            .join("usb.ehci_fault_csw");
        let guard = QmpSocket::new(&deep);
        let path = guard.path().to_path_buf();
        assert!(path.starts_with(std::env::temp_dir()));
        std::fs::write(&path, b"").expect("fallback socket path is writable");
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists(), "{} left behind", path.display());
    }

    #[test]
    fn qmp_socket_fits_a_unix_address() {
        // Next to the logs when it fits.
        let short = Path::new("/home/u/AriCodeOS/build/scenarios/usb.short_csw");
        assert_eq!(short.join("qmp.sock"), qmp_socket_path(short));
        // A deep worktree: 100 bytes of directory plus "/qmp.sock" is 109.
        let deep = Path::new("/")
            .join("d".repeat(99))
            .join("usb.ehci_fault_csw");
        assert!(deep.join("qmp.sock").as_os_str().len() > super::SUN_PATH_MAX);
        let a = qmp_socket_path(&deep);
        let b = qmp_socket_path(&deep);
        assert!(
            a.as_os_str().len() <= super::SUN_PATH_MAX,
            "{}",
            a.display()
        );
        assert!(a.starts_with(std::env::temp_dir()));
        assert_ne!(a, b, "two runs must not share a socket");
    }

    /// The flood reaches the guest only through the two `hostfwd` rules of
    /// `base_command`; any other guest port is refused at once, not probed.
    #[test]
    fn flood_knows_the_forwarded_ports_only() {
        use aricode_proto::net::TEST_TCP_LISTEN_PORT;
        let forwards = super::Forwards {
            tcp: 40001,
            ssh: 40002,
        };
        assert_eq!(forwards.host_port(22), Some(40002));
        assert_eq!(forwards.host_port(TEST_TCP_LISTEN_PORT), Some(40001));
        assert_eq!(forwards.host_port(80), None);
        let ready = std::sync::atomic::AtomicBool::new(false);
        let stop = std::sync::atomic::AtomicBool::new(true);
        let out = super::run_flood_probe(forwards, 80, 3, &ready, &stop);
        assert_eq!(out, "[host] flood: guest port 80 has no hostfwd");
        assert!(
            ready.load(std::sync::atomic::Ordering::SeqCst),
            "the console is released"
        );
    }

    /// Against a local listener standing in for the guest: the flood opens
    /// its connections, counts the one the peer closes during the settle
    /// window, releases the console (`ready`) and holds the rest until
    /// `stop`; the peer closing them afterwards is not counted.
    #[test]
    fn flood_counts_resets_within_the_settle_window_only() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (Arc::clone(&ready), Arc::clone(&stop));
        let flood = std::thread::spawn(move || {
            super::hold_connections(addr, 22, 3, Duration::from_millis(300), &r, &s)
        });
        let mut kept = Vec::new();
        for (i, conn) in listener.incoming().take(3).enumerate() {
            let conn = conn.unwrap();
            if i == 1 {
                drop(conn); // the "guest" resets this one
            } else {
                kept.push(conn);
            }
        }
        let start = std::time::Instant::now();
        while !ready.load(Ordering::SeqCst) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "ready never raised"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The peer closes the rest while they are held: not a reset.
        drop(kept);
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::SeqCst);
        let out = flood.join().unwrap();
        assert_eq!(
            out,
            "[host] flood: 3 connections to guest port 22 opened\n\
             [host] flood: 1 of 3 connections reset by the guest within 300 ms"
        );
    }

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

    /// The hostfwd rules carry the run's ports, never fixed ones.
    #[test]
    fn netdev_forwards_the_runs_ports() {
        let q = super::base_command(
            None,
            Path::new("vars.fd"),
            Path::new("disk.img"),
            false,
            super::Cpu {
                count: 1,
                rdrand: false,
                simd: false,
            },
            super::UsbOptions::default(),
            super::Nics {
                virtio_net: true,
                igb_flr: false,
                forwards: super::Forwards {
                    tcp: 40001,
                    ssh: 40002,
                },
            },
        );
        let args: Vec<String> = q
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let netdev = &args[args.iter().position(|a| a == "-netdev").unwrap() + 1];
        assert_eq!(
            netdev,
            "user,id=net0,hostfwd=tcp:127.0.0.1:40001-:7,hostfwd=tcp:127.0.0.1:40002-:22"
        );
    }

    /// A scenario's update server serves its own channel; the fixed one
    /// (no channel) answers 404 under `/update/`.
    #[test]
    fn update_server_serves_its_own_channel() {
        use std::io::{Read, Write};
        let tmp = crate::test_util::TempDir::new("update-server");
        std::fs::write(tmp.path().join("index"), b"serial = 7\n").unwrap();
        let get = |port: u16, path: &str| {
            let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(c, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        let a = super::UpdateServer::start(tmp.path()).unwrap();
        let b = super::UpdateServer::start(&tmp.path().join("missing")).unwrap();
        assert_ne!(a.port(), b.port());
        let r = get(a.port(), "/update/index");
        assert!(
            r.starts_with("HTTP/1.1 200 OK") && r.ends_with("serial = 7\n"),
            "{r}"
        );
        assert!(get(b.port(), "/update/index").starts_with("HTTP/1.1 404"));
        assert!(get(a.port(), "/hola").ends_with("Hola desde el host\n"));
        assert!(get(a.port(), "/update/../index").starts_with("HTTP/1.1 404"));
        // The public source's shape (ADR 0048): two redirects, then the file.
        let latest = get(a.port(), &format!("{}index", super::MIRROR_LATEST));
        assert!(latest.starts_with("HTTP/1.1 302"), "{latest}");
        let tag = location(&latest);
        assert_eq!(tag, format!("{}index", super::MIRROR_TAG));
        let asset = get(a.port(), &tag);
        let file = location(&asset);
        assert!(file.starts_with("/release-assets/index?sig=") && file.len() > 900);
        assert!(get(a.port(), &file).ends_with("serial = 7\n"));
        // Without a channel the redirects lead to nothing.
        assert!(get(b.port(), &file).starts_with("HTTP/1.1 404"));
    }

    /// Incident 0045: `serve-update` took the target as whatever followed
    /// the first space, CR and LF included, and put it in a `Location` (a
    /// `Set-Cookie` line of the client's choice in the response) and in
    /// its log line.
    #[test]
    fn serve_update_keeps_request_bytes_out_of_headers() {
        use std::io::{Read, Write};
        let tmp = crate::test_util::TempDir::new("serve-update-target");
        std::fs::write(tmp.path().join("index"), b"serial = 7\n").unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let latest = super::MIRROR_LATEST;
        let requests = [
            format!("GET {latest}index\r\nSet-Cookie:%20a=1 HTTP/1.1\r\n\r\n"),
            format!("GET {latest}index\rSet-Cookie:%20a=1 HTTP/1.1\r\n\r\n"),
            format!("GET {latest}a%0d%0aSet-Cookie:x HTTP/1.1\r\n\r\n"),
            format!("GET {latest}index HTTP/1.1\r\n\r\n"),
        ];
        let dir = tmp.path().to_path_buf();
        let count = requests.len();
        let server = std::thread::spawn(move || {
            for _ in 0..count {
                let (s, _) = listener.accept().unwrap();
                super::serve_update_connection(s, &dir);
            }
        });
        let answers: Vec<String> = requests
            .iter()
            .map(|request| {
                let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                c.write_all(request.as_bytes()).unwrap();
                let mut out = String::new();
                c.read_to_string(&mut out).unwrap();
                out
            })
            .collect();
        server.join().unwrap();
        for out in &answers {
            assert!(!out.lines().any(|l| l.starts_with("Set-Cookie")), "{out:?}");
        }
        assert!(
            answers[0].starts_with("HTTP/1.1 400"),
            "CR LF in the target"
        );
        assert!(answers[1].starts_with("HTTP/1.1 400"), "a bare CR");
        // Escaped, the name is no plain file name: no redirect to it.
        assert!(answers[2].starts_with("HTTP/1.1 404"), "{:?}", answers[2]);
        assert_eq!(location(&answers[3]), format!("{}index", super::MIRROR_TAG));
    }

    #[test]
    fn request_targets_and_log_text_are_checked() {
        use super::{log_escaped, request_line};
        use std::io::{Read, Write};
        assert_eq!(
            request_line(b"GET /index HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(("GET", "/index"))
        );
        assert_eq!(
            request_line(b"HEAD /a?b=c HTTP/1.0\r\n"),
            Some(("HEAD", "/a?b=c"))
        );
        for bad in [
            &b"GET /index HTTP/1.1"[..],
            b"GET /index HTTP/1.1\n",
            b"GET /in\x1bdex HTTP/1.1\r\n",
            b"GET /in\x7fdex HTTP/1.1\r\n",
            b"GET /\xc3\xa9 HTTP/1.1\r\n",
            b"GET  /index HTTP/1.1\r\n",
            b"GET /index HTTP/1.1 x\r\n",
            b"GET /index\r\n",
            b"get /index HTTP/1.1\r\n",
            b"GET /index FTP/1.1\r\n",
            b"",
        ] {
            assert_eq!(request_line(bad), None, "{bad:?}");
        }
        assert_eq!(log_escaped(b"/a\r\n\x1b[2J\\x"), r"/a\x0d\x0a\x1b[2J\\x");
        // The scenarios' server refuses such a line too.
        let server = super::UpdateServer::start(std::path::Path::new("missing")).unwrap();
        let mut c = std::net::TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        c.write_all(b"GET /hola\r\nX: y HTTP/1.1\r\n\r\n").unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }

    /// The `Location` of a response.
    fn location(response: &str) -> String {
        response
            .lines()
            .find_map(|l| l.strip_prefix("Location: "))
            .expect("a Location header")
            .to_string()
    }

    /// Ethernet frame from `src` with ethertype `kind` and `payload`.
    fn frame(src: [u8; 6], kind: [u8; 2], payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0xFF; 6];
        f.extend_from_slice(&src);
        f.extend_from_slice(&kind);
        f.extend_from_slice(payload);
        f
    }

    /// IPv4 header (20 bytes) with `proto`, then 4 bytes of ports and padding.
    fn ipv4(proto: u8, sport: u16, dport: u16) -> Vec<u8> {
        let mut p = vec![0x45, 0, 0, 40, 0, 0, 0, 0, 64, proto, 0, 0];
        p.extend_from_slice(&[10, 0, 2, 15, 10, 0, 2, 2]);
        p.extend_from_slice(&sport.to_be_bytes());
        p.extend_from_slice(&dport.to_be_bytes());
        p.extend_from_slice(&[0; 16]);
        p
    }

    fn pcap(little: bool, frames: &[Vec<u8>]) -> Vec<u8> {
        let w = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let mut out = Vec::new();
        // magic, version 2.4 (two u16 read here as one u32), zone, sigfigs,
        // snaplen, link type 1 (Ethernet).
        let version = if little { 0x0004_0002 } else { 0x0002_0004 };
        for v in [0xa1b2_c3d4, version, 0, 0, 65_535, 1] {
            out.extend_from_slice(&w(v));
        }
        for f in frames {
            out.extend_from_slice(&w(0));
            out.extend_from_slice(&w(0));
            out.extend_from_slice(&w(f.len() as u32));
            out.extend_from_slice(&w(f.len() as u32));
            out.extend_from_slice(f);
        }
        out
    }

    #[test]
    fn counts_guest_frames_except_arp_and_dhcp() {
        use super::{guest_frames, GUEST_MAC};
        let slirp = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
        let ip = [0x08, 0x00];
        let tagged = |kind: [u8; 2], payload: &[u8]| {
            let mut p = vec![0x00, 0x05];
            p.extend_from_slice(&kind);
            p.extend_from_slice(payload);
            frame(GUEST_MAC, [0x81, 0x00], &p)
        };
        let frames = [
            frame(GUEST_MAC, ip, &ipv4(17, 68, 67)),  // DHCP: not counted
            frame(GUEST_MAC, [0x08, 0x06], &[0; 28]), // ARP: not counted
            frame(slirp, ip, &ipv4(17, 67, 68)),      // from slirp: not counted
            tagged([0x08, 0x06], &[0; 28]),           // ARP in a VLAN: not counted
            tagged(ip, &ipv4(17, 68, 67)),            // DHCP in a VLAN: not counted
            frame(GUEST_MAC, ip, &ipv4(17, 7, 17_007)), // UDP to the responder
            frame(GUEST_MAC, ip, &ipv4(1, 0, 0)),     // ICMP
            frame(GUEST_MAC, ip, &ipv4(6, 49_152, 17_010)), // TCP to the HTTP server
            frame(GUEST_MAC, [0x86, 0xDD], &[0x60; 40]), // IPv6
            tagged(ip, &ipv4(17, 7, 17_007)),         // UDP in a VLAN
            frame(GUEST_MAC, [0x88, 0xCC], &[0; 20]), // LLDP: any other ethertype
            frame(GUEST_MAC, ip, &[]),                // truncated IPv4 header
        ];
        for little in [true, false] {
            assert_eq!(guest_frames(&pcap(little, &frames), GUEST_MAC), Some(7));
            assert_eq!(
                guest_frames(&pcap(little, &frames[..5]), GUEST_MAC),
                Some(0)
            );
        }
        // A truncated last record ends the count; garbage is unreadable.
        let mut cut = pcap(true, &frames);
        cut.truncate(cut.len() - 10);
        assert_eq!(guest_frames(&cut, GUEST_MAC), Some(6));
        assert_eq!(
            guest_frames(b"not a pcap at all, not at all", GUEST_MAC),
            None
        );
        assert_eq!(guest_frames(&[], GUEST_MAC), None);
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
