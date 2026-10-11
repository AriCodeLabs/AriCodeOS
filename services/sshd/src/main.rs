//! sshd (ADR 0025): the SSH server. It speaks the protocol with
//! `aricode-ssh` over the TCP service and, for each `shell` or `exec`
//! request, asks init for a session shell (it has no SPAWNER). One
//! connection at a time.
//!
//! Handles from init: `[net (WRITE), fs (WRITE), hardware, SYSTEM (READ),
//! sessions (WRITE)]`. In AriFS: `ssh_host_ed25519_key` (the host key seed
//! in hex, created on first start) and `authorized_keys` (OpenSSH format,
//! `ssh-ed25519` lines; read on every connection).
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
const HOST_KEY_FILE: &str = "ssh_host_ed25519_key";
const AUTHORIZED_KEYS: &str = "authorized_keys";

fn say(msg: &[u8]) {
    sys::print(b"sshd: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    let _ = sys::random(SYSTEM, &mut b);
    b
}

fn read_file(fs: &mut FsClient, name: &str) -> Option<Vec<u8>> {
    let fd = fs.open(name).ok()?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match fs.read(fd, data.len() as u32, &mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
        if data.len() > 64 * 1024 {
            break;
        }
    }
    let _ = fs.close(fd);
    Some(data)
}

/// The host key seed: read from AriFS, or created and saved on first start.
fn host_key(fs: &mut FsClient) -> [u8; 32] {
    if let Some(text) = read_file(fs, HOST_KEY_FILE) {
        let mut seed = [0u8; 32];
        if aricode_crypto::unhex(core::str::from_utf8(&text).unwrap_or("").trim(), &mut seed)
            .is_some()
        {
            return seed;
        }
        say(b"host key file is invalid; generating a new one");
    }
    let seed = random32();
    let mut hex = [0u8; 65];
    let text = aricode_crypto::hex(&seed, &mut hex[..64]).len();
    hex[text] = b'\n';
    let saved = fs
        .open(HOST_KEY_FILE)
        .or_else(|_| fs.create(HOST_KEY_FILE))
        .ok()
        .and_then(|fd| {
            let r = fs.replace(fd, &hex[..text + 1]).ok();
            let _ = fs.close(fd);
            r
        });
    say(if saved.is_some() {
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

fn serve(net: &mut NetClient, sock: u16, seed: [u8; 32], authorized: Vec<[u8; 32]>) {
    let mut server = Server::new(seed, random32(), authorized);
    let mut out = Vec::new();
    server.start(&mut out);
    if !send(net, sock, &mut out) {
        return;
    }
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
                            sys::print(b"' authenticated\n");
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

fn main(_arg: u64) -> u64 {
    let Ok(mut fs) = FsClient::new(FS, HW) else {
        say(b"no file client");
        return 1;
    };
    let Ok(mut net) = NetClient::new(NET, HW) else {
        say(b"no network client");
        return 2;
    };
    let seed = host_key(&mut fs);
    let listener = match net.tcp_listen(PORT) {
        Ok(l) => l,
        Err(_) => {
            say(b"cannot listen on port 22");
            return 3;
        }
    };
    let keys = read_file(&mut fs, AUTHORIZED_KEYS)
        .map(|t| parse_authorized_keys(&t))
        .unwrap_or_default();
    let mut b = [0u8; 20];
    sys::print(b"sshd: listening on port 22, host key ");
    sys::print(fingerprint(&aricode_crypto::ed25519::public_key(&seed)).as_bytes());
    sys::print(b", ");
    sys::print(aricode_rt::fmt_u64(keys.len() as u64, &mut b));
    sys::print(b" authorized key(s)\n");
    if keys.is_empty() {
        say(b"no keys in authorized_keys: every login will be refused");
    }
    // Tell init we are up, so it can start the console shell.
    let _ = sys::ipc_send(
        SESSIONS,
        &session::Request {
            kind: session::Kind::Ready,
        }
        .encode(),
        &[],
        1000,
    );
    loop {
        let (sock, peer, _) = match net.tcp_accept(listener, 60_000) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut ip = [0u8; 15];
        sys::print(b"sshd: connection from ");
        sys::print(aricode_net_format(peer, &mut ip));
        sys::print(b"\n");
        let keys = read_file(&mut fs, AUTHORIZED_KEYS)
            .map(|t| parse_authorized_keys(&t))
            .unwrap_or_default();
        serve(&mut net, sock, seed, keys);
        let _ = net.tcp_close(sock);
        say(b"connection closed");
    }
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
