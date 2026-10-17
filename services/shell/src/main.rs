// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! arish (Phase 21): interactive shell in ring 3. It only sees what init
//! gives it: 0x10004 = fsd endpoint (WRITE), 0x10005 = HARDWARE (DMA buffer),
//! 0x10006 = SYSTEM (READ|MANAGE), 0x10007 = console input (READ),
//! 0x10008 = PCI root (READ), 0x10009 = netd endpoint (WRITE), 0x1000A =
//! init's launch broker (WRITE) for `run`. `arg` = mask of running
//! services. The shell cannot create processes by itself.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_proto::net::{Status as NetStatus, LINK_DOWN, LINK_UP};
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, abi, fs::FsClient, Handle, TIMEOUT_INFINITE};
use aricode_rt::{fmt_hex, fmt_u64};

const FSD: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const SYSTEM: Handle = Handle(0x0001_0006);
/// Console input, or the session input endpoint in session mode.
const CONSOLE_IN: Handle = Handle(0x0001_0007);
/// Session output endpoint (session mode only, ADR 0025).
const SESSION_OUT: Handle = Handle(0x0001_000B);
const PCI_ROOT: Handle = Handle(0x0001_0008);
const NETD: Handle = Handle(0x0001_0009);
const BROKER: Handle = Handle(0x0001_000A);

/// Bits of `arg`: services that init left running.
const SERVICE_BLOCK: u64 = 1;
const SERVICE_FS: u64 = 2;
const SERVICE_NET: u64 = 4;
const SERVICE_INPUT: u64 = 8;
const SERVICE_GUI: u64 = 16;

const PROMPT: &[u8] = b"ari> ";
const LINE_MAX: usize = 256;

/// Output capture for `diag NAME`: single-threaded, so a static cell is
/// enough.
const CAPTURE_MAX: usize = 16 * 1024;

struct Capture {
    state: core::cell::UnsafeCell<([u8; CAPTURE_MAX], usize, bool)>,
}

// SAFETY: the shell has a single thread; nobody else touches the capture.
unsafe impl Sync for Capture {}

static CAPTURE: Capture = Capture {
    state: core::cell::UnsafeCell::new(([0; CAPTURE_MAX], 0, false)),
};

fn capture_with<R>(f: impl FnOnce(&mut ([u8; CAPTURE_MAX], usize, bool)) -> R) -> R {
    // SAFETY: single thread and no reentrancy: `f` does not call back here.
    f(unsafe { &mut *CAPTURE.state.get() })
}

/// I/O mode, fixed at start: 0 console, 1 session, 2 quiet session.
static MODE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
const MODE_SESSION: u8 = 1;
const MODE_QUIET: u8 = 2;

fn session() -> bool {
    MODE.load(core::sync::atomic::Ordering::Relaxed) != 0
}

/// `fault` is accepted only when the boot command line asks for it
/// (`shell_fault`, tests only; ADR 0007).
fn fault_enabled() -> bool {
    let mut buf = [0u8; 256];
    sys::cmdline(SYSTEM, &mut buf)
        .is_some_and(|c| aricode_cmdline::CmdLine::new(c).has(aricode_cmdline::keys::SHELL_FAULT))
}

/// Deliberate crash (`fault`, tests only): the shell dies by a page fault
/// without ending its session, as a shell with a bug would.
fn fault() -> ! {
    print(b"arish: crashing on purpose (shell_fault)\n");
    // SAFETY: deliberate fault; page 0 is never mapped and the address is
    // hidden from the compiler so that it emits the read.
    let bad = core::hint::black_box(0x40u64) as *const u64;
    let v = unsafe { bad.read_volatile() };
    sys::process_exit(v)
}

fn quiet() -> bool {
    MODE.load(core::sync::atomic::Ordering::Relaxed) == MODE_QUIET
}

/// Sends `b` to the session output in IPC-sized pieces.
fn session_write(b: &[u8]) {
    for chunk in b.chunks(abi::IPC_MAX_DATA) {
        let _ = sys::ipc_send(SESSION_OUT, chunk, &[], 2000);
    }
}

/// Writes to the console (or the session) and, while capture is active,
/// also to the buffer (whatever does not fit is lost).
fn print(b: &[u8]) {
    if session() {
        session_write(b);
    } else {
        sys::print(b);
    }
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

/// Last executed lines, for the up and down arrows.
const HISTORY: usize = 8;

struct History {
    lines: [[u8; LINE_MAX]; HISTORY],
    lens: [usize; HISTORY],
    /// Stored lines (up to `HISTORY`); the most recent is `next - 1`.
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

    /// Stores a non-empty line that differs from the last one.
    fn push(&mut self, line: &[u8]) {
        if line.is_empty() || self.back(1) == Some(line) {
            return;
        }
        self.lines[self.next][..line.len()].copy_from_slice(line);
        self.lens[self.next] = line.len();
        self.next = (self.next + 1) % HISTORY;
        self.count = (self.count + 1).min(HISTORY);
    }

    /// The line `n` positions back (1 = the most recent).
    fn back(&self, n: usize) -> Option<&[u8]> {
        if n == 0 || n > self.count {
            return None;
        }
        let i = (self.next + HISTORY - n) % HISTORY;
        Some(&self.lines[i][..self.lens[i]])
    }
}

impl aricode_lineedit::History for History {
    fn back(&self, n: usize) -> Option<&[u8]> {
        History::back(self, n)
    }
}

/// Reads a line from the console with the editor in `aricode-lineedit`:
/// cursor movement (arrows, Home/End, Ctrl-A/E), insert and delete anywhere
/// (backspace, Delete), Ctrl-U to clear, and the history on the up and down
/// arrows. Keys arrive as a terminal sends them, from any keyboard.
/// Bytes read but not yet consumed: one read can carry the end of a line
/// and the start of the next (a pasted block, or sshd sending a command
/// followed by `exit`).
struct Input {
    buf: [u8; abi::IPC_MAX_DATA],
    start: usize,
    end: usize,
}

impl Input {
    const fn new() -> Self {
        Self {
            buf: [0; abi::IPC_MAX_DATA],
            start: 0,
            end: 0,
        }
    }

    /// Next byte, waiting for more input when empty; `None` when the remote
    /// side of a session hung up.
    fn next(&mut self) -> Option<u8> {
        while self.start == self.end {
            let n = if session() {
                let mut handles = [Handle(0); 1];
                match sys::ipc_receive(CONSOLE_IN, &mut self.buf, &mut handles, TIMEOUT_INFINITE) {
                    Ok((0, _)) | Err(_) => return None,
                    Ok((n, _)) => n,
                }
            } else {
                sys::console_read(CONSOLE_IN, &mut self.buf[..64], TIMEOUT_INFINITE).unwrap_or(0)
            };
            self.start = 0;
            self.end = n;
        }
        let b = self.buf[self.start];
        self.start += 1;
        Some(b)
    }
}

/// Reads a line with the editor in `aricode-lineedit`: cursor movement
/// (arrows, Home/End, Ctrl-A/E), insert and delete anywhere (backspace,
/// Delete), Ctrl-U to clear, and the history on the up and down arrows.
/// Keys arrive as a terminal sends them, from any keyboard or from a
/// session. `None` when a session's remote side hung up; a quiet session
/// does not echo.
fn read_line(input: &mut Input, line: &mut [u8; LINE_MAX], history: &History) -> Option<usize> {
    let mut editor: aricode_lineedit::Editor<LINE_MAX> = aricode_lineedit::Editor::new();
    loop {
        let b = input.next()?;
        let event = if quiet() {
            editor.feed(b, history, &mut |_| {})
        } else {
            editor.feed(b, history, &mut |out| print(out))
        };
        if event == aricode_lineedit::Event::Submit {
            let text = editor.line();
            line[..text.len()].copy_from_slice(text);
            return Some(text.len());
        }
    }
}

/// Splits into (command, rest) at the first space.
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
        b"commands:\n  help      this help\n  ps        processes\n  mem       physical memory\n  ls        AriFS files\n  cat NAME  show a file\n  write NAME TEXT  save TEXT to the file (creates it or replaces it entirely)\n  devices   PCI functions\n  services  boot services (--detail: supervisor state)\n  uptime    time since boot\n  cpus      online processors and idle time per CPU\n  diag [NAME]  full report (uptime, mem, cpus, ps, services, net, devices); with NAME saves it to AriFS\n  net       network configuration\n  ping IP   ICMP echo (4 attempts)\n  dns NAME  resolve a name\n  http URL  GET and show the body (http:// or https://)\n  run PROG  run an initrd program and wait\n\
  update [URL|check|apply]  system update over the network (ADR 0027): downloads the signed image from the channel URL (saved for later runs), installs it in the inactive slot; then `reboot`\n  spawn PROG  launch it in the background (cannot be stopped)\n  repeat N CMD  repeat a command N times\n  reboot    reboot\n  shutdown  power off\n  exit      exit the shell\n",
    );
}

fn ps() {
    use abi::sysinfo::{ProcessEntry, ProcessState};
    let mut buf = [0u8; ProcessEntry::LEN * 32];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::PROCESSES, &mut buf) else {
        print(b"ps: no system access\n");
        return;
    };
    print(b"PID  STATE       NAME\n");
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
            ProcessState::Running => print(b"running     "),
            ProcessState::Exited => {
                print(b"exited(");
                num(e.exit_code as u64);
                print(b")");
                for _ in fmt_u64(e.exit_code as u64, &mut b).len()..4 {
                    print(b" ");
                }
            }
            ProcessState::Killed => print(b"killed      "),
        }
        print(e.name_str().as_bytes());
        print(b"\n");
    }
}

fn mem() {
    let mut buf = [0u8; 16];
    if sys::system_info(SYSTEM, abi::sysinfo::MEMORY, &mut buf) != Ok(16) {
        print(b"mem: no system access\n");
        return;
    }
    let total = u64::from_le_bytes(buf[..8].try_into().unwrap());
    let free = u64::from_le_bytes(buf[8..].try_into().unwrap());
    print(b"usable physical memory: ");
    num(total * 4 / 1024);
    print(b" MiB total, ");
    num(free * 4 / 1024);
    print(b" MiB free, ");
    num((total - free) * 4 / 1024);
    print(b" MiB in use (");
    num(total);
    print(b" 4 KiB frames)\n");
}

fn uptime() {
    let mut buf = [0u8; 8];
    if sys::system_info(SYSTEM, abi::sysinfo::UPTIME, &mut buf) != Ok(8) {
        print(b"uptime: no system access\n");
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
        print(b"cpus: no system access\n");
        return;
    }
    print(b"cpus: ");
    num(u64::from_le_bytes(buf));
    print(b" online\n");
    // Idle per CPU: cycles spent in `hlt` versus elapsed cycles.
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
        print(b"% idle\n");
    }
}

/// `diag`: full report in one block, to attach when describing a problem
/// (docs/performance.md). With a name it is also saved to AriFS, to read it
/// from the host (`cargo xtask fs-cat`, docs/hardware.md).
fn diag(fs: &mut FsClient, net_client: &NetClient, mask: u64, rest: &[u8]) {
    let name = trim(rest);
    if !name.is_empty() {
        capture_with(|(_, len, active)| {
            *len = 0;
            *active = true;
        });
    }
    print(b"=== AriCodeOS diagnostics ===\n");
    uptime();
    mem();
    cpus();
    ps();
    services(mask, fs);
    services_detail();
    if mask & SERVICE_NET != 0 {
        net(net_client);
    }
    devices();
    events();
    print(b"=== end of diagnostics ===\n");
    if name.is_empty() {
        return;
    }
    let len = capture_with(|(_, len, active)| {
        *active = false;
        *len
    });
    let Ok(name) = core::str::from_utf8(name) else {
        print(b"diag: invalid name\n");
        return;
    };
    // SAFETY: the capture is already closed; we read what was written.
    let data =
        capture_with(|(buf, _, _)| unsafe { core::slice::from_raw_parts(buf.as_ptr(), len) });
    match save_file(fs, name, data) {
        Ok(n) => {
            print(b"diag: ");
            num(n as u64);
            print(b" bytes to ");
            print(name.as_bytes());
            print(b"\n");
        }
        Err(()) => print(b"diag: could not save the report\n"),
    }
}

/// Creates the file `name`, or replaces it entirely, with `data`.
fn save_file(fs: &mut FsClient, name: &str, data: &[u8]) -> Result<usize, ()> {
    let fd = fs.open(name).or_else(|_| fs.create(name)).map_err(|_| ())?;
    let r = fs.replace(fd, data).map_err(|_| ());
    let _ = fs.close(fd);
    r
}

/// Recent kernel events (`sysinfo::EVENTS`): one line per event with its
/// timestamp.
fn events() {
    use abi::sysinfo::{EventEntry, EventKind};
    let mut buf = [0u8; EventEntry::LEN * 16];
    let Ok(n) = sys::system_info(SYSTEM, abi::sysinfo::EVENTS, &mut buf) else {
        print(b"diag: no access to events\n");
        return;
    };
    print(b"recent kernel events:\n");
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
                print(b"process ");
                num(e.pid as u64);
                print(b" '");
                print(e.name_str().as_bytes());
                print(b"' created");
            }
            EventKind::ProcessExited => {
                print(b"process ");
                num(e.pid as u64);
                print(b" '");
                print(e.name_str().as_bytes());
                print(b"' exited with code ");
                num(e.code);
            }
            EventKind::ProcessKilled => {
                print(b"process ");
                num(e.pid as u64);
                print(b" killed by fault: ");
                print(e.name_str().as_bytes());
            }
            EventKind::DmaFault => {
                print(b"DMA fault from device ");
                hex(e.code, 4);
                print(b": ");
                print(e.name_str().as_bytes());
            }
            EventKind::DmaQuota => {
                print(b"process ");
                num(e.pid as u64);
                print(b" exhausted its memory quota requesting ");
                num(e.code);
                print(b" bytes");
            }
            EventKind::Fault => {
                print(b"fault reported by process ");
                num(e.pid as u64);
                print(b": ");
                print(e.name_str().as_bytes());
                print(b" (code ");
                num(e.code);
                print(b")");
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
                print(b"ls: file service not responding\n");
                return;
            }
        }
    }
    num(i as u64);
    print(b" file(s)\n");
}

fn cat(fs: &mut FsClient, name: &[u8]) {
    let Ok(name) = core::str::from_utf8(name) else {
        print(b"cat: invalid name\n");
        return;
    };
    if name.is_empty() {
        print(b"usage: cat NAME\n");
        return;
    }
    let fd = match fs.open(name) {
        Ok(fd) => fd,
        Err(_) => {
            print(b"cat: not found: ");
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
                print(b"cat: read error\n");
                break;
            }
        }
    }
    let _ = fs.close(fd);
}

/// `write NAME TEXT`: creates the file if it does not exist and replaces
/// its whole content (plus a newline) atomically and durably (ADR 0014).
fn write(fs: &mut FsClient, rest: &[u8]) {
    let (name, text) = split(rest);
    let Ok(name) = core::str::from_utf8(name) else {
        print(b"write: invalid name\n");
        return;
    };
    if name.is_empty() {
        print(b"usage: write NAME TEXT\n");
        return;
    }
    let mut content = [0u8; LINE_MAX + 1];
    content[..text.len()].copy_from_slice(text);
    content[text.len()] = b'\n';
    match save_file(fs, name, &content[..text.len() + 1]) {
        Ok(n) => {
            print(b"write: ");
            num(n as u64);
            print(b" bytes to ");
            print(name.as_bytes());
            print(b"\n");
        }
        Err(()) => print(b"write: write error\n"),
    }
}

fn devices() {
    let Ok(n) = sys::pci_count(PCI_ROOT) else {
        print(b"devices: no PCI access\n");
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
        print(b"  class ");
        hex(abi::pci_info::class(v) as u64, 2);
        print(b".");
        hex(abi::pci_info::subclass(v) as u64, 2);
        print(b"\n");
    }
    num(n);
    print(b" PCI function(s)\n");
}

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    print(aricode_net::Ipv4Addr(ip).format(&mut b));
}

fn net_error(e: NetError) {
    match e {
        NetError::Status(abi_status) => {
            print(match abi_status {
                aricode_proto::net::Status::Timeout => b"no reply (timed out)\n" as &[u8],
                aricode_proto::net::Status::NoRoute => b"no network configuration\n",
                aricode_proto::net::Status::NotFound => b"unknown name\n",
                _ => b"network service rejected the request\n",
            });
        }
        NetError::Ipc(_) => print(b"network service not responding\n"),
    }
}

fn net(client: &NetClient) {
    print(b"link    ");
    print(match client.link() {
        Ok(LINK_UP) => b"up\n",
        Ok(LINK_DOWN) => b"down (cable?)\n",
        Ok(_) => b"unknown\n",
        Err(_) => b"no reply from network service\n",
    });
    match client.info() {
        Err(NetError::Status(NetStatus::NoRoute)) => {
            print(b"ip      no address (DHCP pending)\n");
        }
        Ok(i) => {
            print(b"ip      ");
            print_ip(i.address);
            print(b"\nmask    ");
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
        print(b"usage: ping A.B.C.D\n");
        return;
    };
    let mut received = 0;
    for seq in 1..=4u16 {
        match client.ping(ip.0, seq, 1000) {
            Ok(rtt) => {
                received += 1;
                print(b"reply from ");
                print_ip(ip.0);
                print(b": seq=");
                num(seq as u64);
                print(b" time=");
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
    print(b" of 4 replies\n");
}

fn dns(client: &mut NetClient, arg: &[u8]) {
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"dns: invalid name\n");
        return;
    };
    if name.is_empty() {
        print(b"usage: dns NAME\n");
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

fn http_get(fs: &mut FsClient, client: &mut NetClient, arg: &[u8]) {
    use aricode_https::HttpsError;
    let Ok(url) = core::str::from_utf8(arg) else {
        print(b"http: invalid url\n");
        return;
    };
    if url.is_empty() {
        print(b"usage: http [http[s]://]host[:port][/path]\n");
        return;
    }
    // An extra trust anchor in AriFS (a private CA), if there is one.
    let mut extra = alloc::vec::Vec::new();
    if let Ok(fd) = fs.open(TRUSTED_CA) {
        let mut der = alloc::vec![0u8; 4096];
        if let Ok(n) = fs.read(fd, 0, &mut der) {
            der.truncate(n);
            extra.push(der);
        }
        let _ = fs.close(fd);
    }
    let mut body = [0u8; 8192];
    match aricode_https::get(client, SYSTEM, &extra, url, &mut body, 8000) {
        Ok(f) => {
            print(b"HTTP ");
            num(f.status as u64);
            print(b", ");
            num(f.body_len as u64);
            print(if f.truncated {
                b" bytes shown (body truncated to 8 KiB)\n"
            } else {
                b" bytes\n"
            });
            print(&body[..f.body_len]);
            if f.body_len > 0 && body[f.body_len - 1] != b'\n' {
                print(b"\n");
            }
        }
        Err(HttpsError::BadUrl) => print(b"http: invalid url\n"),
        Err(HttpsError::Resolve(e)) => net_error(e),
        Err(HttpsError::Connect(e)) => {
            print(b"http: could not connect: ");
            net_error(e);
        }
        Err(HttpsError::Io(e)) => net_error(e),
        Err(HttpsError::Tls(e)) => {
            print(b"http: TLS: ");
            print(tls_error(e));
            print(b"\n");
        }
        Err(HttpsError::Malformed) => print(b"http: invalid response\n"),
        Err(HttpsError::Unsupported) => {
            print(b"http: response uses an unsupported encoding or protocol\n")
        }
        Err(HttpsError::TooBig) => print(b"http: response headers too large\n"),
        Err(HttpsError::Aborted) => print(b"http: download aborted\n"),
    }
}

/// AriFS file with an extra trusted root certificate (DER) for `http`.
const TRUSTED_CA: &str = "trusted_ca.der";

/// Short explanation of a TLS failure.
fn tls_error(e: aricode_tls::Error) -> &'static [u8] {
    use aricode_tls::Error as E;
    use aricode_x509::Error as X;
    match e {
        E::Certificate(X::Expired) => b"certificate expired",
        E::Certificate(X::NotYetValid) => b"certificate not yet valid (check the clock)",
        E::Certificate(X::NameMismatch) => b"certificate does not match the host name",
        E::Certificate(X::UnknownIssuer) => b"certificate signed by an unknown authority",
        E::Certificate(X::BadSignature) => b"invalid certificate signature",
        E::Certificate(X::WrongPurpose) => b"certificate not issued for a TLS server",
        E::Certificate(_) => b"certificate rejected",
        E::Alert(_) => b"the server sent an alert",
        E::Unsupported(_) => b"the server needs an unsupported TLS feature",
        E::Decrypt => b"record authentication failed",
        E::Protocol(_) => b"protocol error",
    }
}

/// `update [URL|check|apply]`: runs the updater (ADR 0027). With a URL,
/// saves it first as the update channel (`updater/update_url`). `check`
/// only reports the installed and available versions; `apply` installs
/// whatever is already in the UPDATE partition.
fn update(fs: &mut FsClient, rest: &[u8]) {
    use aricode_rt::sys::launch;
    let arg = match rest {
        b"" => UPDATER_FETCH,
        b"check" => UPDATER_CHECK,
        b"apply" => UPDATER_APPLY,
        url if url.starts_with(b"http://") || url.starts_with(b"https://") => {
            let mut content = [0u8; LINE_MAX + 1];
            content[..url.len()].copy_from_slice(url);
            content[url.len()] = b'\n';
            if save_file(fs, UPDATE_URL_FILE, &content[..url.len() + 1]).is_err() {
                print(b"update: could not save the channel URL\n");
                return;
            }
            UPDATER_FETCH
        }
        _ => {
            print(b"usage: update [http[s]://host[:port]/path/ | check | apply]\n");
            return;
        }
    };
    let result = launch::run(BROKER, "updater", arg);
    if session() {
        // The updater printed on the console; show its log here.
        show_file(fs, UPDATER_LOG_FILE);
    }
    match result {
        Ok(0) => {}
        Ok(code) => {
            print(b"update: updater exited with ");
            num(code);
            print(b"\n");
        }
        Err(e) => {
            print(b"update: updater");
            launch_failure(e);
        }
    }
}

/// Prints a file, if it exists (no message otherwise).
fn show_file(fs: &mut FsClient, name: &str) {
    let Ok(fd) = fs.open(name) else {
        return;
    };
    let mut buf = [0u8; 512];
    let mut offset = 0u32;
    while let Ok(n) = fs.read(fd, offset, &mut buf) {
        if n == 0 {
            break;
        }
        print(&buf[..n]);
        offset += n as u32;
    }
    let _ = fs.close(fd);
}

/// Explains why the broker did not run a program (after the program name).
fn launch_failure(e: aricode_rt::sys::launch::LaunchError) {
    use aricode_rt::sys::launch::{LaunchError, Status};
    match e {
        LaunchError::Status(Status::NotFound) => print(b": not found in the initrd or AriFS\n"),
        LaunchError::Status(Status::Denied) => {
            print(b": denied by the broker (requests system capabilities)\n")
        }
        LaunchError::Status(Status::Killed) => print(b": was killed by the kernel\n"),
        LaunchError::Status(Status::BadManifest) => print(b": invalid manifest\n"),
        LaunchError::Status(Status::Limit) => print(b": running program limit reached\n"),
        LaunchError::Status(_) => print(b": the broker could not launch it\n"),
        LaunchError::BadName => print(b": invalid name\n"),
        LaunchError::Ipc(e) => aricode_rt::print_error(b"", e),
    }
}

/// Updater arguments (`apps/updater`).
const UPDATER_APPLY: u64 = 0;
const UPDATER_FETCH: u64 = 1;
const UPDATER_CHECK: u64 = 2;
/// Channel URL and last run's log in the updater's private directory.
const UPDATE_URL_FILE: &str = "updater/update_url";
const UPDATER_LOG_FILE: &str = "updater/last.log";

/// Asks the broker (init) to run an initrd program with the capabilities
/// of its manifest and waits for it to exit.
fn run(arg: &[u8]) {
    use aricode_rt::sys::launch;
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"run: invalid name\n");
        return;
    };
    if name.is_empty() {
        print(b"usage: run PROGRAM [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    // The result is printed in one piece at the end: while the program runs,
    // its output and the broker's interleave with ours.
    let result = launch::run(BROKER, prog, num_arg);
    print(b"run: ");
    print(prog.as_bytes());
    match result {
        Ok(code) => {
            print(b" exited with ");
            num(code);
            print(b"\n");
        }
        Err(e) => launch_failure(e),
    }
}

/// `spawn PROGRAM [ARG]`: background launch through the broker.
fn spawn(arg: &[u8]) {
    use aricode_rt::sys::launch::{self, LaunchError, Status};
    let Ok(name) = core::str::from_utf8(arg) else {
        print(b"spawn: invalid name\n");
        return;
    };
    if name.is_empty() {
        print(b"usage: spawn PROGRAM [ARG]\n");
        return;
    }
    let (prog, num_arg) = match name.split_once(' ') {
        Some((p, a)) => (p, a.trim().parse::<u64>().unwrap_or(0)),
        None => (name, 0),
    };
    print(b"spawn: ");
    print(prog.as_bytes());
    match launch::spawn(BROKER, prog, num_arg) {
        Ok(()) => print(b" launched in the background\n"),
        Err(LaunchError::Status(Status::Limit)) => {
            print(b": running program limit reached (");
            num(4);
            print(b")\n");
        }
        Err(LaunchError::Status(Status::NotFound)) => {
            print(b": not found in the initrd or AriFS\n")
        }
        Err(LaunchError::Status(Status::Denied)) => {
            print(b": denied by the broker (requests system capabilities)\n")
        }
        Err(LaunchError::Status(Status::BadManifest)) => print(b": invalid manifest\n"),
        Err(LaunchError::Status(_)) => print(b": the broker could not launch it\n"),
        Err(LaunchError::BadName) => print(b": invalid name\n"),
        Err(LaunchError::Ipc(e)) => aricode_rt::print_error(b"", e),
    }
}

fn services(mask: u64, fs: &FsClient) {
    // By role, not by driver: under QEMU they are virtio; on the laptop, usbd
    // and rtl_net (`services --detail` gives the real names).
    print(b"block     service     ");
    print(if mask & SERVICE_BLOCK != 0 {
        b"active\n"
    } else {
        b"absent\n"
    });
    print(b"files     fsd         ");
    if mask & SERVICE_FS == 0 {
        print(b"absent\n");
    } else {
        let mut name = [0u8; 32];
        print(if fs.list(0, &mut name).is_ok() {
            b"active, responding\n"
        } else {
            b"active, not responding\n"
        });
    }
    print(b"network   service     ");
    print(if mask & SERVICE_NET != 0 {
        b"active\n"
    } else {
        b"absent\n"
    });
    print(b"input     service     ");
    print(if mask & SERVICE_INPUT != 0 {
        b"active\n"
    } else {
        b"absent\n"
    });
    print(b"graphics  compositor  ");
    print(if mask & SERVICE_GUI != 0 {
        b"active\n"
    } else {
        b"absent (start with `gui`)\n"
    });
}

/// `services --detail`: what init knows about each supervised service.
fn services_detail() {
    use aricode_rt::sys::status::{service_status, LastExit};
    print(b"SERVICE          STATE        GEN  RESTARTS   LAST EXIT         READY FOR\n");
    let mut index = 0u8;
    loop {
        let entry = match service_status(BROKER, index) {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => {
                print(b"services: init not responding\n");
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
                print(b"code ");
                let c = aricode_rt::fmt_u64(entry.last_code, &mut b);
                print(c);
                for _ in c.len()..13 {
                    print(b" ");
                }
            }
            LastExit::Killed => print(b"crash (killed)    "),
            LastExit::StartTimeout => print(b"start timeout     "),
        }
        if entry.ready_for_ms > 0 {
            print(b"for ");
            num(entry.ready_for_ms / 1000);
            print(b" s");
        } else {
            print(b"-");
        }
        if entry.optional {
            print(b"  (optional)");
        }
        print(b"\n");
        index += 1;
        if index == 255 {
            break;
        }
    }
}

fn main(mask: u64) -> u64 {
    use aricode_proto::session::{SHELL_QUIET, SHELL_SESSION};
    if mask & SHELL_SESSION != 0 {
        let mode = if mask & SHELL_QUIET != 0 {
            MODE_QUIET
        } else {
            MODE_SESSION
        };
        MODE.store(mode, core::sync::atomic::Ordering::Relaxed);
    }
    let Ok(mut fs) = FsClient::new(FSD, HW) else {
        print(b"arish: no file client\n");
        return 1;
    };
    let Ok(mut net_client) = NetClient::new(NETD, HW) else {
        print(b"arish: no network client\n");
        return 2;
    };
    if !quiet() {
        print(b"arish: AriCodeOS shell (type help for help)\n");
    }
    let mut line = [0u8; LINE_MAX];
    let mut history = History::new();
    let mut input = Input::new();
    loop {
        if !quiet() {
            print(PROMPT);
        }
        let Some(len) = read_line(&mut input, &mut line, &history) else {
            break;
        };
        history.push(&line[..len]);
        if execute(&mut fs, &mut net_client, mask, &line[..len], 0) {
            break;
        }
    }
    if session() {
        // End of stream: sshd closes the channel.
        let _ = sys::ipc_send(SESSION_OUT, &[], &[], 2000);
    }
    0
}

/// Executes a line; returns `true` if the shell must exit. `depth` bounds
/// the recursion of `repeat`.
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
        b"diag" => diag(fs, net_client, mask, rest),
        b"net" => net(net_client),
        b"ping" => ping(net_client, rest),
        b"dns" => dns(net_client, rest),
        b"http" => http_get(fs, net_client, rest),
        b"run" => run(rest),
        b"update" => update(fs, rest),
        b"spawn" => spawn(rest),
        b"repeat" => {
            let (count, inner) = split(rest);
            let Some(times) = core::str::from_utf8(count)
                .ok()
                .and_then(|c| c.parse::<u32>().ok())
            else {
                print(b"usage: repeat N COMMAND\n");
                return false;
            };
            if depth > 0 || inner.is_empty() {
                print(b"repeat: no nesting, and a command is required\n");
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
        b"fault" if fault_enabled() => fault(),
        other => {
            print(b"unknown command: ");
            print(other);
            print(b" (help)\n");
        }
    }
    false
}

aricode_rt::entry!(main);
