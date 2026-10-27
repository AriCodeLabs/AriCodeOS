// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! sshd (ADR 0025): the SSH server. It speaks the protocol with
//! `aricode-ssh` over the TCP service and, for each `shell` or `exec`
//! request, asks init for a session shell (it has no SPAWNER). One
//! connection at a time.
//!
//! Handles from init: `[net (WRITE), fs (its directory `sshd/`, WRITE),
//! hardware, SYSTEM (READ), sessions (WRITE), authorized_keys (read-only)]`
//! (ADR 0050). In `sshd/`: `ssh_host_ed25519_key` (the host key seed in
//! hex; installed by `cargo xtask usb-image --authorized-key`, or created
//! on first start) and `random_seed`; in the AriFS root, read-only:
//! `authorized_keys` (OpenSSH format,
//! `ssh-ed25519` lines; reread after every connection, or at most once a
//! minute while the file service does not answer; a key removed during
//! such an outage stays valid until the file service answers again).
//! `random_seed` is entropy carried across boots.
//!
//! Closed by default: without an `authorized_keys` file with at least one
//! key, sshd does not listen at all.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_crypto::SecretBytes;
use aricode_proto::fs::Status as FsStatus;
use aricode_proto::net::Status;
use aricode_proto::session;
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::net::{NetClient, NetError};
use aricode_rt::sys::{self, abi, Error, Handle};
use aricode_ssh::keys_refresh::{Change, KeyRefresh};
use aricode_ssh::{fingerprint, log_safe, parse_authorized_keys, Event, Server};

const NET: Handle = Handle(0x0001_0004);
const FS: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
const SESSIONS: Handle = Handle(0x0001_0008);
/// `authorized_keys`, read-only (ADR 0050); `FS` is sshd's own directory.
const KEYS: Handle = Handle(0x0001_0009);

/// Given to sshd's network session by init: no other program may listen
/// on it (audit 2026-10-25 E, L1).
const PORT: u16 = aricode_proto::net::SSH_PORT;
/// A connection that has not authenticated after this long is dropped: the
/// server attends one connection at a time, so an idle client must not
/// hold the port.
const AUTH_DEADLINE_MS: u64 = 30_000;
const HOST_KEY_FILE: &str = "ssh_host_ed25519_key";
const AUTHORIZED_KEYS: &str = "authorized_keys";
const SEED_FILE: &str = "random_seed";
/// Fault event when a host key is drawn from a weak kernel generator with
/// no `random_seed` carried from an earlier boot (ADR 0025, ADR 0039); its
/// code is the `EntropySource` number, 0 when the kernel does not say.
const ENTROPY_WEAK_FAULT: &str = "ssh entropy weak";
const _: () = assert!(ENTROPY_WEAK_FAULT.len() <= abi::sysinfo::EVENT_NAME_LEN);

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
    /// A non-empty `random_seed` from an earlier boot was read and mixed in.
    carried: bool,
    /// A draw from the kernel gave fewer bytes than asked: whatever the
    /// kernel says of its generator, this boot's keys are weak unless a
    /// seed was carried (audit of incident 0036, M1).
    draw_failed: bool,
}

impl Entropy {
    fn load(fs: &mut FsClient) -> Self {
        let mut h = aricode_crypto::sha256::Sha256::new();
        let mut saved = read_file(fs, SEED_FILE);
        if let Ok(saved) = &saved {
            h.update(saved);
        }
        let mut fresh = [0u8; 32];
        let draw_failed = sys::random(SYSTEM, &mut fresh) != Ok(fresh.len());
        h.update(&fresh);
        aricode_crypto::wipe(&mut fresh);
        h.update(&sys::time::uptime_us(SYSTEM).unwrap_or(0).to_le_bytes());
        let state = h.finish();
        let carried = saved.as_ref().is_ok_and(|s| !s.is_empty());
        if let Ok(saved) = &mut saved {
            aricode_crypto::wipe(saved);
        }
        Self {
            state,
            counter: 0,
            carried,
            draw_failed,
        }
    }

    /// Before a host key is drawn: if the kernel generator was not seeded
    /// from a hardware source and got no kernel seed carried from the
    /// previous boot (ADR 0043), or does not say, and no `random_seed` of
    /// sshd's own came from an earlier boot, the key is only as good as
    /// this boot's cycle counter and interrupt timing. That is recorded (`ENTROPY_WEAK_FAULT`) and
    /// said, not refused: refusing would lock the owner out, and the key
    /// still mixes everything there is. With a carried seed the boots
    /// before this one add to it, so nothing is reported.
    fn check_for_host_key(&self) {
        let source = sys::random_source(SYSTEM);
        if self.carried || (!self.draw_failed && source.is_some_and(|r| r.is_strong())) {
            return;
        }
        let _ = sys::event_record(source.map_or(0, |r| r.source as u64), ENTROPY_WEAK_FAULT);
        let mut text = [0u8; abi::sysinfo::RandomSource::SUMMARY_MAX];
        let why = match source {
            _ if self.draw_failed => {
                alloc::string::String::from("the kernel gave too few random bytes")
            }
            Some(r) => alloc::format!(
                "kernel generator seeded from {}, {}",
                r.summary(&mut text),
                r.carried.reason()
            ),
            None => {
                alloc::string::String::from("the kernel does not report what seeded its generator")
            }
        };
        // One write, so the line is never split by another program's output.
        sys::print(
            alloc::format!(
                "sshd: fault ssh entropy weak: {why} and no random_seed from an earlier boot: the new host key is weak (cargo xtask usb-image --authorized-key installs one made on the build host)\n"
            )
            .as_bytes(),
        );
    }

    /// Saves a successor for the next boot (never the state in use). Done
    /// before randomness is drawn for something that lasts (a new host key,
    /// a connection's keys), not at start: a boot nobody connects to writes
    /// nothing (incident 0005), and a boot that used its randomness never
    /// leaves the same seed to the next one. A failed save is logged and
    /// returns `false`, so the caller tries again next time.
    fn save_next(&self, fs: &mut FsClient) -> bool {
        let next = SecretBytes(self.derive(b"next boot"));
        let mut hex = [0u8; 65];
        let n = aricode_crypto::hex(&next.0, &mut hex[..64]).len();
        hex[n] = b'\n';
        let saved = write_file(fs, SEED_FILE, &hex[..n + 1]);
        aricode_crypto::wipe(&mut hex);
        if !saved {
            say(b"could not save random_seed; trying again on the next connection");
        }
        saved
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
        self.draw_failed |= sys::random(SYSTEM, &mut fresh) != Ok(fresh.len());
        self.counter += 1;
        let mut h = aricode_crypto::sha256::Sha256::new();
        h.update(&SecretBytes(self.derive(b"fresh")).0);
        h.update(&fresh);
        aricode_crypto::wipe(&mut fresh);
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

/// Why a file could not be read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadError {
    /// The file service answered: there is no such file.
    Absent,
    /// The file service did not answer (died mid-request, or too slow).
    Unavailable,
    /// The session can never answer again: its fsd generation is gone
    /// (sessions die with it, ADR 0050). sshd exits so init relaunches it
    /// with new sessions; keeping the last keys would keep a key removed
    /// since valid until reboot.
    Gone,
    /// It answered, but the content could not be read whole.
    Incomplete,
}

/// Whole file. A read error the file service reports (a USB stick that
/// fails a transfer) is retried a few times before giving up: a truncated
/// `authorized_keys` must never pass as empty. A service that does not
/// answer is not retried: each attempt would wait the file client's whole
/// reply wait.
fn read_file(fs: &mut FsClient, name: &str) -> Result<Vec<u8>, ReadError> {
    let fd = fs.open(name).map_err(|e| match e {
        FsError::Status(FsStatus::NotFound) => ReadError::Absent,
        FsError::Ipc(Error::ServiceGone) => ReadError::Gone,
        e if e.service_unavailable() => ReadError::Unavailable,
        _ => ReadError::Incomplete,
    })?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 512];
    let mut errors = 0;
    let result = loop {
        match fs.read(fd, data.len() as u32, &mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => data.extend_from_slice(&chunk[..n]),
            Err(FsError::Ipc(Error::ServiceGone)) => break Err(ReadError::Gone),
            Err(e) if e.service_unavailable() => break Err(ReadError::Unavailable),
            Err(_) if errors < 3 => {
                errors += 1;
                sys::time::sleep_ms(200);
            }
            Err(_) => break Err(ReadError::Incomplete),
        }
        if data.len() > 64 * 1024 {
            break Err(ReadError::Incomplete);
        }
    };
    let _ = fs.close(fd);
    // The host key and random_seed pass through here.
    aricode_crypto::wipe(&mut chunk);
    result.map(|()| data)
}

/// The host key seed: read from AriFS, or created and saved on first start.
/// A file that could not be read is never overwritten: the boot uses a
/// key of its own and leaves the saved one alone (audit robustness-1, L4).
/// Before drawing a key that will be saved, the next-boot seed is saved, so
/// a boot that dies before saving the key never hands the same entropy to
/// the next one; `seed_saved` records whether that save succeeded.
fn host_key(fs: &mut FsClient, entropy: &mut Entropy, seed_saved: &mut bool) -> [u8; 32] {
    match read_file(fs, HOST_KEY_FILE) {
        Ok(mut text) => {
            let mut seed = [0u8; 32];
            let parsed =
                aricode_crypto::unhex(core::str::from_utf8(&text).unwrap_or("").trim(), &mut seed);
            aricode_crypto::wipe(&mut text);
            if parsed.is_some() {
                return seed;
            }
            say(b"host key file is invalid; generating a new one");
        }
        Err(ReadError::Absent) => {}
        Err(_) => {
            say(b"host key file cannot be read: using a key for this boot only (the saved one is kept)");
            entropy.check_for_host_key();
            return entropy.next();
        }
    }
    entropy.check_for_host_key();
    *seed_saved = entropy.save_next(fs);
    let seed = entropy.next();
    let mut hex = [0u8; 65];
    let n = aricode_crypto::hex(&seed, &mut hex[..64]).len();
    hex[n] = b'\n';
    let saved = write_file(fs, HOST_KEY_FILE, &hex[..n + 1]);
    aricode_crypto::wipe(&mut hex);
    say(if saved {
        b"new host key generated and saved"
    } else {
        b"new host key generated (could not save it: it changes on every boot)"
    });
    seed
}

/// Output held for a client whose window is closed: past this sshd stops
/// taking the session's messages, so the programs writing them wait in the
/// kernel queue instead of growing sshd's memory (audit of ADR 0047, M1).
const OUTPUT_PENDING_MAX: usize = 64 * 1024;
/// Session messages taken per pass, so the client's input and window
/// adjusts are read in between.
const OUTPUT_BURST: usize = 16;
/// Typed input the shell has not taken yet (it reads none while a program
/// runs); sent without waiting, so output keeps flowing meanwhile (audit
/// of ADR 0047, L4). Input past this is dropped and said.
const INPUT_BACKLOG_MAX: usize = 64 * 1024;

/// Exit status reported to the client when the session shell died without
/// ending the session (its output endpoint closed with no end-of-stream
/// message): 128 + SIGSEGV, as POSIX shells report a command killed by a
/// fault.
const SHELL_DIED_STATUS: u32 = 139;

/// Our ends of a session: we write what the client types to `input` (send
/// side only) and read the shell's output from `output` (receive side
/// only). Holding only one side of each (ADR 0030) is what lets a shell
/// that dies show up as `PeerClosed` on `output` instead of a silent hang.
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

/// Asks init for a session shell of `kind`. Every endpoint is a pair: the
/// shell gets the receive side of `input` and the send side of `output`,
/// sshd keeps the other two, and init's reply comes on a pair too.
fn open_session(kind: session::Kind) -> Result<Session, Error> {
    let (in_r, input) = sys::endpoint_create_pair()?;
    let Ok((output, out_w)) = sys::endpoint_create_pair() else {
        let _ = sys::handle_close(in_r);
        let _ = sys::handle_close(input);
        return Err(Error::Limit);
    };
    let Ok((reply, reply_w)) = sys::endpoint_create_pair() else {
        for h in [in_r, input, output, out_w] {
            let _ = sys::handle_close(h);
        }
        return Err(Error::Limit);
    };
    let handles = [reply_w, in_r, out_w];
    let sent = sys::ipc_send(
        SESSIONS,
        &session::Request { kind }.encode(),
        &handles,
        2000,
    );
    if sent.is_err() {
        // Not transferred: still ours.
        for h in handles {
            let _ = sys::handle_close(h);
        }
    }
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

/// Queues `data` for the shell's input; `false` if it did not fit.
fn queue_input(backlog: &mut Vec<u8>, data: &[u8]) -> bool {
    if backlog.len() + data.len() > INPUT_BACKLOG_MAX {
        return false;
    }
    backlog.extend_from_slice(data);
    true
}

/// Sends what the shell's input queue takes now, without waiting.
fn pump_input(s: &Session, backlog: &mut Vec<u8>) {
    let mut sent = 0;
    for chunk in backlog.chunks(abi::IPC_MAX_DATA) {
        if sys::ipc_send(s.input, chunk, &[], 0).is_err() {
            break;
        }
        sent += chunk.len();
    }
    backlog.drain(..sent);
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
    seed: &[u8; 32],
    rng: &[u8; 32],
    authorized: Vec<[u8; 32]>,
) {
    // The server's copies of the host key and of the generator seed are
    // wiped when it is dropped (incident 0044).
    let mut server = Server::new(*seed, *rng, authorized);
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
    let mut input = Vec::new();
    // More session output was waiting when the last pass stopped taking it.
    let mut more_output = false;
    'conn: loop {
        let wait_ms = if more_output { 1 } else { 20 };
        match net.tcp_recv(sock, wait_ms, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                events.clear();
                let r = server.feed(&buf[..n], &mut out, &mut events);
                for e in events.drain(..) {
                    match e {
                        Event::Authenticated { user } => {
                            let mut safe = [0u8; 32];
                            sys::print(b"sshd: user '");
                            sys::print(log_safe(user.as_bytes(), &mut safe));
                            sys::print(b"' authenticated from ");
                            print_ip(peer);
                            sys::print(b"\n");
                        }
                        Event::AuthFailed { user } => {
                            let mut safe = [0u8; 32];
                            sys::print(b"sshd: refused key for '");
                            sys::print(log_safe(user.as_bytes(), &mut safe));
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
                                input.clear();
                                if !queue_input(&mut input, &command)
                                    || !queue_input(&mut input, b"\nexit\n")
                                {
                                    say(b"exec command too long: dropped");
                                    input.clear();
                                    let _ = queue_input(&mut input, b"exit\n");
                                }
                                session = Some(s);
                            }
                            Err(_) => {
                                server.send_data(b"sshd: no session available\n", &mut out);
                                server.exit(1, &mut out);
                            }
                        },
                        Event::Data(d) => {
                            if session.is_some() && !queue_input(&mut input, &d) {
                                say(b"session input dropped: the shell is not reading it");
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
        if let Some(s) = &session {
            pump_input(s, &mut input);
        }
        // Shell output to the client; an empty message means it exited.
        // The output endpoint closing without one (`PeerClosed`: sshd holds
        // only its receive side) means the shell died: the session ends
        // with `SHELL_DIED_STATUS` instead of hanging until the client
        // disconnects. At most `OUTPUT_BURST` messages per pass, and none
        // while the client's window holds `OUTPUT_PENDING_MAX` back.
        let mut taken = 0;
        more_output = false;
        while let Some(s) = &session {
            if taken == OUTPUT_BURST || server.pending_len() >= OUTPUT_PENDING_MAX {
                more_output = server.pending_len() < OUTPUT_PENDING_MAX;
                break;
            }
            let mut none = [Handle(0); 1];
            let status = match sys::ipc_receive(s.output, &mut msg, &mut none, 0) {
                Ok((0, _)) => 0,
                Ok((n, _)) => {
                    server.send_data(&msg[..n], &mut out);
                    taken += 1;
                    continue;
                }
                Err(Error::PeerClosed) => {
                    say(b"session shell died without ending the session");
                    SHELL_DIED_STATUS
                }
                Err(_) => break,
            };
            server.exit(status, &mut out);
            input.clear();
            if let Some(s) = session.take() {
                let _ = sys::handle_close(s.input);
                let _ = sys::handle_close(s.output);
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

/// Keys from `authorized_keys`: none if the file does not exist (closed by
/// default); an error if it could not be read.
fn authorized(fs: &mut FsClient) -> Result<Vec<[u8; 32]>, ReadError> {
    match read_file(fs, AUTHORIZED_KEYS) {
        Ok(text) => Ok(parse_authorized_keys(&text)),
        Err(ReadError::Absent) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Rereads `authorized_keys` after a connection when `refresh` says it is
/// due: a missing file empties the keys (connections are refused); a file
/// service that does not answer keeps the last keys and is retried at most
/// once a minute (incident 0001: each attempt would hold the next banner
/// for the file client's whole wait), logged once when it starts failing
/// and once when it answers again.
/// `Err(Gone)`: the session is gone for good (see `ReadError::Gone`).
fn reread_keys(
    fs: &mut FsClient,
    refresh: &mut KeyRefresh,
    keys: &mut Vec<[u8; 32]>,
) -> Result<(), ReadError> {
    if !refresh.due(now_ms()) {
        return Ok(());
    }
    // Only a complete read (or "no such file") is an answer; a file service
    // that does not answer or a read that breaks off keeps the last keys and
    // is retried at most once a minute (audit robustness-1, L5).
    let result = authorized(fs);
    if result == Err(ReadError::Gone) {
        return Err(ReadError::Gone);
    }
    let answered = result.is_ok();
    if let Ok(k) = result {
        *keys = k;
    }
    let mut b = [0u8; 20];
    match refresh.record(now_ms(), answered) {
        Some(Change::Failing) => {
            sys::print(b"sshd: cannot reread authorized_keys (file service not answering or read incomplete): keeping the last ");
            sys::print(aricode_rt::fmt_u64(keys.len() as u64, &mut b));
            sys::print(b" key(s); rereading authorized_keys at most once a minute\n");
        }
        Some(Change::Recovered) => {
            sys::print(b"sshd: file service answers again: ");
            sys::print(aricode_rt::fmt_u64(keys.len() as u64, &mut b));
            sys::print(b" authorized key(s)\n");
        }
        None => {}
    }
    Ok(())
}

/// Exit code when the file sessions are gone (`ReadError::Gone`).
const EXIT_FILES_GONE: u64 = 5;

fn files_gone() -> u64 {
    say(b"file sessions gone (the file service was restarted); exiting for new ones");
    EXIT_FILES_GONE
}

/// Keys for a start: none if the file does not exist or could not be read
/// (closed until a reread finds some); `Err(Gone)` as `reread_keys`.
fn initial_keys(fs: &mut FsClient) -> Result<Vec<[u8; 32]>, ReadError> {
    match authorized(fs) {
        Err(ReadError::Gone) => Err(ReadError::Gone),
        r => Ok(r.unwrap_or_default()),
    }
}

fn main(_arg: u64) -> u64 {
    let (Ok(mut fs), Ok(mut keys_fs)) = (FsClient::new(FS, HW), FsClient::new(KEYS, HW)) else {
        say(b"no file client");
        return 1;
    };
    let Ok(mut net) = NetClient::new(NET, HW) else {
        say(b"no network client");
        return 2;
    };
    let mut entropy = Entropy::load(&mut fs);
    let Ok(mut keys) = initial_keys(&mut keys_fs) else {
        return files_gone();
    };
    let mut refresh = KeyRefresh::new();
    let mut announced = false;
    if keys.is_empty() {
        say(b"no authorized_keys in AriFS: not listening (install a key with cargo xtask usb-image --authorized-key, or write authorized_keys ... from the console)");
        ready();
        announced = true;
        // Closed until the owner installs a key; checked every few seconds.
        while keys.is_empty() {
            sys::time::sleep_ms(5_000);
            let Ok(k) = initial_keys(&mut keys_fs) else {
                return files_gone();
            };
            keys = k;
        }
        say(b"authorized_keys found");
    }
    let mut seed_saved = false;
    let seed = host_key(&mut fs, &mut entropy, &mut seed_saved);
    let mut listener = match net.tcp_listen(PORT) {
        Ok(l) => l,
        Err(e) => {
            sys::print(b"sshd: cannot listen on port 22 (");
            sys::print(e.describe().as_bytes());
            sys::print(b")\n");
            ready();
            return 3;
        }
    };
    let mut relisten_refused = false;
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
            // Nobody can receive on the network service endpoint any more
            // (ADR 0030): every accept would fail at once; its supervisor
            // decides what follows.
            Err(NetError::Ipc(Error::ServiceGone)) => {
                say(b"network service gone; exiting");
                return 4;
            }
            // The network driver died holding the accept, or was relaunched
            // and no longer knows the socket: its sockets went with it.
            // Listen again on the next generation after a pause.
            Err(NetError::Ipc(Error::ReplyLost)) | Err(NetError::Status(Status::BadSock)) => {
                sys::time::sleep_ms(500);
                match net.tcp_listen(PORT) {
                    Ok(l) => {
                        listener = l;
                        relisten_refused = false;
                        say(b"network service restarted: listening on port 22 again");
                    }
                    // Said once, not every 500 ms: a refusal is never silent
                    // (owner's rule), nor a flood.
                    Err(e) if !relisten_refused => {
                        relisten_refused = true;
                        sys::print(b"sshd: cannot listen on port 22 again (");
                        sys::print(e.describe().as_bytes());
                        sys::print(b"); retrying\n");
                    }
                    Err(_) => {}
                }
                continue;
            }
            Err(_) => continue,
        };
        sys::print(b"sshd: connection from ");
        print_ip(peer);
        sys::print(b"\n");
        if keys.is_empty() {
            // authorized_keys emptied while running: reread first, so a key
            // installed since counts (audit robustness-1, M1); refuse if
            // there is still none.
            if reread_keys(&mut keys_fs, &mut refresh, &mut keys).is_err() {
                let _ = net.tcp_close(sock);
                return files_gone();
            }
            if keys.is_empty() {
                say(b"authorized_keys is empty now: connection refused");
                let _ = net.tcp_close(sock);
                continue;
            }
        }
        if !seed_saved {
            seed_saved = entropy.save_next(&mut fs);
        }
        let rng = SecretBytes(entropy.next());
        serve(&mut net, sock, peer, &seed, &rng.0, keys.clone());
        let _ = net.tcp_close(sock);
        say(b"connection closed");
        // Reread between connections so a key added or removed from the
        // console counts, without delaying the next client's banner behind
        // a file service that does not answer.
        if reread_keys(&mut keys_fs, &mut refresh, &mut keys).is_err() {
            return files_gone();
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
