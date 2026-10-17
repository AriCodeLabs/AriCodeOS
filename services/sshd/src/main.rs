// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! sshd (ADR 0025): the SSH server. It speaks the protocol with
//! `aricode-ssh` over the TCP service and, for each `shell` or `exec`
//! request, asks init for a session shell (it has no SPAWNER). One
//! connection at a time.
//!
//! Handles from init: `[net (WRITE), fs (WRITE), hardware, SYSTEM (READ),
//! sessions (WRITE)]`. In AriFS: `ssh_host_ed25519_key` (the host key seed
//! in hex; installed by `cargo xtask usb-image --authorized-key`, or
//! created on first start), `authorized_keys` (OpenSSH format,
//! `ssh-ed25519` lines; read on every connection) and `random_seed`
//! (entropy carried across boots).
//!
//! Closed by default: without an `authorized_keys` file with at least one
//! key, sshd does not listen at all.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_proto::net::Status;
use aricode_proto::session;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, abi, rights, Error, Handle};
use aricode_ssh::{fingerprint, parse_authorized_keys, Event, Server};

const NET: Handle = Handle(0x0001_0004);
const FS: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
const SESSIONS: Handle = Handle(0x0001_0008);

const PORT: u16 = 22;
/// A connection that has not authenticated after this long is dropped: the
/// server attends one connection at a time, so an idle client must not
/// hold the port.
const AUTH_DEADLINE_MS: u64 = 30_000;
const HOST_KEY_FILE: &str = "ssh_host_ed25519_key";
const AUTHORIZED_KEYS: &str = "authorized_keys";
const SEED_FILE: &str = "random_seed";

fn say(msg: &[u8]) {
    sys::print(b"sshd: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Entropy carried across boots (`random_seed` in AriFS), mixed with the
/// kernel generator: a laptop without `rdrand` has little entropy at boot,
/// so every boot adds to what the previous ones gathered.
struct Entropy {
    state: [u8; 32],
    counter: u64,
}

impl Entropy {
    fn load(fs: &mut FsClient) -> Self {
        let mut h = aricode_crypto::sha256::Sha256::new();
        if let Some(saved) = read_file(fs, SEED_FILE) {
            h.update(&saved);
        }
        let mut fresh = [0u8; 32];
        let _ = sys::random(SYSTEM, &mut fresh);
        h.update(&fresh);
        h.update(&sys::time::uptime_us(SYSTEM).unwrap_or(0).to_le_bytes());
        let state = h.finish();
        Self { state, counter: 0 }
    }

    /// Saves a successor for the next boot (never the state in use). Done
    /// after the start-up reads: some USB sticks stop answering for a
    /// moment right after a write.
    fn save_next(&self, fs: &mut FsClient) {
        let next = self.derive(b"next boot");
        let mut hex = [0u8; 65];
        let n = aricode_crypto::hex(&next, &mut hex[..64]).len();
        hex[n] = b'\n';
        write_file(fs, SEED_FILE, &hex[..n + 1]);
    }

    fn derive(&self, label: &[u8]) -> [u8; 32] {
        let mut h = aricode_crypto::sha256::Sha256::new();
        h.update(&self.state);
        h.update(label);
        h.update(&self.counter.to_le_bytes());
        h.finish()
    }

    /// Fresh 32 bytes: state, kernel randomness and a counter.
    fn next(&mut self) -> [u8; 32] {
        let mut fresh = [0u8; 32];
        let _ = sys::random(SYSTEM, &mut fresh);
        self.counter += 1;
        let mut h = aricode_crypto::sha256::Sha256::new();
        h.update(&self.derive(b"fresh"));
        h.update(&fresh);
        self.state = h.finish();
        self.derive(b"output")
    }
}

fn write_file(fs: &mut FsClient, name: &str, data: &[u8]) -> bool {
    fs.open(name)
        .or_else(|_| fs.create(name))
        .ok()
        .and_then(|fd| {
            let r = fs.replace(fd, data).ok();
            let _ = fs.close(fd);
            r
        })
        .is_some()
}

/// Whole file, or `None` if it does not exist or cannot be read. A read
/// error (a USB stick that fails a transfer) is retried a few times before
/// giving up: a truncated `authorized_keys` must never pass as empty.
fn read_file(fs: &mut FsClient, name: &str) -> Option<Vec<u8>> {
    let fd = fs.open(name).ok()?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 512];
    let mut errors = 0;
    let complete = loop {
        match fs.read(fd, data.len() as u32, &mut chunk) {
            Ok(0) => break true,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(_) if errors < 3 => {
                errors += 1;
                sys::time::sleep_ms(200);
            }
            Err(_) => break false,
        }
        if data.len() > 64 * 1024 {
            break false;
        }
    };
    let _ = fs.close(fd);
    complete.then_some(data)
}

/// The host key seed: read from AriFS, or created and saved on first start.
fn host_key(fs: &mut FsClient, entropy: &mut Entropy) -> [u8; 32] {
    if let Some(text) = read_file(fs, HOST_KEY_FILE) {
        let mut seed = [0u8; 32];
        if aricode_crypto::unhex(core::str::from_utf8(&text).unwrap_or("").trim(), &mut seed)
            .is_some()
        {
            return seed;
        }
        say(b"host key file is invalid; generating a new one");
    }
    let seed = entropy.next();
    let mut hex = [0u8; 65];
    let n = aricode_crypto::hex(&seed, &mut hex[..64]).len();
    hex[n] = b'\n';
    say(if write_file(fs, HOST_KEY_FILE, &hex[..n + 1]) {
        b"new host key generated and saved"
    } else {
        b"new host key generated (could not save it: it changes on every boot)"
    });
    seed
}

/// Our ends of a session: we write what the client types to `input` and
/// read the shell's output from `output`.
struct Session {
    input: Handle,
    output: Handle,
}

impl Session {
    fn hang_up(self) {
        let _ = sys::ipc_send(self.input, &[], &[], 100);
        let _ = sys::handle_close(self.input);
        let _ = sys::handle_close(self.output);
    }
}

/// Asks init for a session shell of `kind`.
fn open_session(kind: session::Kind) -> Result<Session, Error> {
    let input = sys::endpoint_create()?;
    let output = sys::endpoint_create()?;
    let reply = sys::endpoint_create()?;
    let handles = [
        sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER)?,
        sys::handle_duplicate(input, rights::READ | rights::TRANSFER)?,
        sys::handle_duplicate(output, rights::WRITE | rights::TRANSFER)?,
    ];
    let sent = sys::ipc_send(
        SESSIONS,
        &session::Request { kind }.encode(),
        &handles,
        2000,
    );
    let mut msg = [0u8; 8];
    let mut none = [Handle(0); 1];
    let ok = sent.is_ok()
        && sys::ipc_receive(reply, &mut msg, &mut none, 5000)
            .is_ok_and(|(n, _)| session::reply_ok(&msg[..n]));
    let _ = sys::handle_close(reply);
    if !ok {
        let _ = sys::handle_close(input);
        let _ = sys::handle_close(output);
        return Err(Error::Limit);
    }
    Ok(Session { input, output })
}

fn send(net: &mut NetClient, sock: u16, out: &mut Vec<u8>) -> bool {
    let ok = out.is_empty() || net.tcp_send_all(sock, out).is_ok();
    out.clear();
    ok
}

fn serve(
    net: &mut NetClient,
    sock: u16,
    peer: [u8; 4],
    seed: [u8; 32],
    rng: [u8; 32],
    authorized: Vec<[u8; 32]>,
) {
    let mut server = Server::new(seed, rng, authorized);
    let mut out = Vec::new();
    server.start(&mut out);
    if !send(net, sock, &mut out) {
        return;
    }
    let started = now_ms();
    let mut session: Option<Session> = None;
    let mut buf = [0u8; 1400];
    let mut msg = [0u8; abi::IPC_MAX_DATA];
    let mut events = Vec::new();
    'conn: loop {
        match net.tcp_recv(sock, 20, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                events.clear();
                let r = server.feed(&buf[..n], &mut out, &mut events);
                for e in events.drain(..) {
                    match e {
                        Event::Authenticated { user } => {
                            sys::print(b"sshd: user '");
                            sys::print(user.as_bytes());
                            sys::print(b"' authenticated from ");
                            print_ip(peer);
                            sys::print(b"\n");
                        }
                        Event::AuthFailed { user } => {
                            sys::print(b"sshd: refused key for '");
                            sys::print(user.as_bytes());
                            sys::print(b"' from ");
                            print_ip(peer);
                            sys::print(b"\n");
                        }
                        Event::Shell { .. } | Event::Exec { .. } if session.is_some() => {}
                        Event::Shell { .. } => match open_session(session::Kind::Shell) {
                            Ok(s) => session = Some(s),
                            Err(_) => {
                                server.send_data(b"sshd: no session available\n", &mut out);
                                server.exit(1, &mut out);
                            }
                        },
                        Event::Exec { command, .. } => match open_session(session::Kind::Exec) {
                            Ok(s) => {
                                // The quiet shell runs the command, then `exit`.
                                for part in [command.as_slice(), b"\nexit\n"] {
                                    for chunk in part.chunks(abi::IPC_MAX_DATA) {
                                        let _ = sys::ipc_send(s.input, chunk, &[], 2000);
                                    }
                                }
                                session = Some(s);
                            }
                            Err(_) => {
                                server.send_data(b"sshd: no session available\n", &mut out);
                                server.exit(1, &mut out);
                            }
                        },
                        Event::Data(d) => {
                            if let Some(s) = &session {
                                for chunk in d.chunks(abi::IPC_MAX_DATA) {
                                    let _ = sys::ipc_send(s.input, chunk, &[], 2000);
                                }
                            }
                        }
                        Event::Eof => {}
                        Event::Closed => {
                            send(net, sock, &mut out);
                            break 'conn;
                        }
                    }
                }
                if let Err(e) = r {
                    sys::print(b"sshd: connection dropped: ");
                    sys::print(match e {
                        aricode_ssh::Error::Protocol(m) | aricode_ssh::Error::Negotiation(m) => {
                            m.as_bytes()
                        }
                        aricode_ssh::Error::Mac => b"packet authentication failed",
                        aricode_ssh::Error::Disconnected => b"client disconnected",
                    });
                    sys::print(b"\n");
                    send(net, sock, &mut out);
                    break;
                }
            }
            Err(NetError::Status(Status::Timeout)) => {}
            Err(_) => break,
        }
        if !server.is_authenticated() && now_ms().saturating_sub(started) > AUTH_DEADLINE_MS {
            sys::print(b"sshd: authentication timeout from ");
            print_ip(peer);
            sys::print(b"\n");
            server.drop_connection(b"authentication timeout", &mut out);
            send(net, sock, &mut out);
            break;
        }
        // Shell output to the client; an empty message means it exited.
        while let Some(s) = &session {
            let mut none = [Handle(0); 1];
            match sys::ipc_receive(s.output, &mut msg, &mut none, 0) {
                Ok((0, _)) => {
                    server.exit(0, &mut out);
                    if let Some(s) = session.take() {
                        let _ = sys::handle_close(s.input);
                        let _ = sys::handle_close(s.output);
                    }
                }
                Ok((n, _)) => server.send_data(&msg[..n], &mut out),
                Err(_) => break,
            }
        }
        if !send(net, sock, &mut out) || server.is_closed() {
            break;
        }
    }
    if let Some(s) = session.take() {
        s.hang_up();
    }
}

fn ready() {
    // Tell init we are up (listening or not), so it starts the console shell.
    let _ = sys::ipc_send(
        SESSIONS,
        &session::Request {
            kind: session::Kind::Ready,
        }
        .encode(),
        &[],
        1000,
    );
}

/// Keys from `authorized_keys`; `None` if the file cannot be read.
fn authorized(fs: &mut FsClient) -> Option<Vec<[u8; 32]>> {
    read_file(fs, AUTHORIZED_KEYS).map(|t| parse_authorized_keys(&t))
}

fn main(_arg: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FS, HW) else {
        say(b"no file client");
        return 1;
    };
    let Ok(mut net) = NetClient::new(NET, HW) else {
        say(b"no network client");
        return 2;
    };
    let mut entropy = Entropy::load(&mut fs);
    let mut keys = authorized(&mut fs).unwrap_or_default();
    let mut announced = false;
    if keys.is_empty() {
        say(b"no authorized_keys in AriFS: not listening (install a key with cargo xtask usb-image --authorized-key, or write authorized_keys ... from the console)");
        ready();
        announced = true;
        // Closed until the owner installs a key; checked every few seconds.
        while keys.is_empty() {
            sys::time::sleep_ms(5_000);
            keys = authorized(&mut fs).unwrap_or_default();
        }
        say(b"authorized_keys found");
    }
    let seed = host_key(&mut fs, &mut entropy);
    entropy.save_next(&mut fs);
    let listener = match net.tcp_listen(PORT) {
        Ok(l) => l,
        Err(_) => {
            say(b"cannot listen on port 22");
            ready();
            return 3;
        }
    };
    let mut b = [0u8; 20];
    sys::print(b"sshd: listening on port 22, host key ");
    sys::print(fingerprint(&aricode_crypto::ed25519::public_key(&seed)).as_bytes());
    sys::print(b", ");
    sys::print(aricode_rt::fmt_u64(keys.len() as u64, &mut b));
    sys::print(b" authorized key(s)\n");
    if !announced {
        ready();
    }
    loop {
        let (sock, peer, _) = match net.tcp_accept(listener, 60_000) {
            Ok(c) => c,
            Err(_) => continue,
        };
        sys::print(b"sshd: connection from ");
        print_ip(peer);
        sys::print(b"\n");
        if keys.is_empty() {
            // authorized_keys emptied while running: refuse everything.
            say(b"authorized_keys is empty now: connection refused");
            let _ = net.tcp_close(sock);
            continue;
        }
        let rng = entropy.next();
        serve(&mut net, sock, peer, seed, rng, keys.clone());
        let _ = net.tcp_close(sock);
        say(b"connection closed");
        // Reread between connections so a key added or removed from the
        // console counts, without delaying the next client's banner behind
        // a slow disk; a read that fails keeps the last keys read.
        match authorized(&mut fs) {
            Some(k) => keys = k,
            None => say(b"cannot reread authorized_keys; keeping the last keys read"),
        }
    }
}

fn now_ms() -> u64 {
    sys::time::uptime_us(SYSTEM).unwrap_or(0) / 1000
}

fn print_ip(ip: [u8; 4]) {
    let mut b = [0u8; 15];
    sys::print(aricode_net_format(ip, &mut b));
}

/// Dotted quad without pulling in the network library.
fn aricode_net_format(ip: [u8; 4], out: &mut [u8; 15]) -> &[u8] {
    let mut n = 0;
    for (i, part) in ip.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        let mut b = [0u8; 20];
        let s = aricode_rt::fmt_u64(*part as u64, &mut b);
        out[n..n + s.len()].copy_from_slice(s);
        n += s.len();
    }
    &out[..n]
}

aricode_rt::entry!(main);
