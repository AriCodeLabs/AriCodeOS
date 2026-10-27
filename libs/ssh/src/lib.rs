// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! SSH-2 server as a state machine (ADR 0025): bytes from the client go in
//! through [`Server::feed`], bytes for the client and [`Event`]s come out.
//! No I/O and no threads, so it runs the same in the AriCodeOS `sshd`
//! service and in host tests against the real OpenSSH client.
//!
//! Supported (and only): key exchange `curve25519-sha256` with strict KEX
//! (`kex-strict-s-v00@openssh.com`), host key `ssh-ed25519`, cipher
//! `chacha20-poly1305@openssh.com`, no compression, `publickey`
//! authentication with `ssh-ed25519` keys, and one `session` channel with
//! `pty-req`, `shell`, `exec`, `env` and `window-change`.

#![no_std]

extern crate alloc;

pub mod base64;
pub mod keys_refresh;
pub mod wire;

use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::chacha20::Rng;
use aricode_crypto::sha256::Sha256;
use aricode_crypto::ssh_chachapoly::{ChaChaPoly, TAG_LEN};
use aricode_crypto::{ed25519, x25519};
use wire::{list_has, negotiate, Reader, Writer};

pub const VERSION: &[u8] = b"SSH-2.0-AriCodeOS_sshd_1.0";

mod msg {
    pub const DISCONNECT: u8 = 1;
    pub const IGNORE: u8 = 2;
    pub const UNIMPLEMENTED: u8 = 3;
    pub const DEBUG: u8 = 4;
    pub const SERVICE_REQUEST: u8 = 5;
    pub const SERVICE_ACCEPT: u8 = 6;
    pub const EXT_INFO: u8 = 7;
    pub const KEXINIT: u8 = 20;
    pub const NEWKEYS: u8 = 21;
    pub const KEX_ECDH_INIT: u8 = 30;
    pub const KEX_ECDH_REPLY: u8 = 31;
    pub const USERAUTH_REQUEST: u8 = 50;
    pub const USERAUTH_FAILURE: u8 = 51;
    pub const USERAUTH_SUCCESS: u8 = 52;
    pub const USERAUTH_PK_OK: u8 = 60;
    pub const GLOBAL_REQUEST: u8 = 80;
    pub const REQUEST_FAILURE: u8 = 82;
    pub const CHANNEL_OPEN: u8 = 90;
    pub const CHANNEL_OPEN_CONFIRMATION: u8 = 91;
    pub const CHANNEL_OPEN_FAILURE: u8 = 92;
    pub const CHANNEL_WINDOW_ADJUST: u8 = 93;
    pub const CHANNEL_DATA: u8 = 94;
    pub const CHANNEL_EOF: u8 = 96;
    pub const CHANNEL_CLOSE: u8 = 97;
    pub const CHANNEL_REQUEST: u8 = 98;
    pub const CHANNEL_SUCCESS: u8 = 99;
    pub const CHANNEL_FAILURE: u8 = 100;
}

const KEX: &[u8] = b"curve25519-sha256";
const KEX_LIBSSH: &[u8] = b"curve25519-sha256@libssh.org";
const STRICT_S: &[u8] = b"kex-strict-s-v00@openssh.com";
/// Packets either way a connection may carry: half the sequence space,
/// far from its wrap (no rekey is supported).
const MAX_PACKETS: u32 = 1 << 31;
const STRICT_C: &[u8] = b"kex-strict-c-v00@openssh.com";
const HOSTKEY: &[u8] = b"ssh-ed25519";
const CIPHER: &[u8] = b"chacha20-poly1305@openssh.com";
/// Ignored with an AEAD cipher, but the lists must have a common name.
const MAC: &[u8] = b"hmac-sha2-256";
const NONE: &[u8] = b"none";

/// Window we grant the client, and when to top it up.
const RECV_WINDOW: u32 = 1 << 20;
const MAX_PACKET: u32 = 32 * 1024;
/// Authentication requests a connection may make without succeeding
/// (incident 0046): refused keys and methods, key queries (`publickey`
/// without a signature, answered `PK_OK` for an authorized key) and `none`
/// requests all count, except a first `none`, the client asking for the
/// methods (RFC 4252, 5.2). A client can neither probe keys nor keep the
/// server answering without bound.
const MAX_AUTH_ATTEMPTS: u32 = 10;

/// What happened, for the caller to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The client proved it holds an authorized key.
    Authenticated { user: String },
    /// A public-key attempt was refused (key not authorized, bad
    /// signature, or another method); the connection goes on until
    /// `MAX_AUTH_ATTEMPTS`.
    AuthFailed { user: String },
    /// The connection used up `MAX_AUTH_ATTEMPTS` requests without
    /// authenticating and is disconnected.
    TooManyAuthAttempts { user: String },
    /// Interactive shell requested (`pty` if a terminal was asked for).
    Shell { pty: bool },
    /// A single command.
    Exec { command: Vec<u8>, pty: bool },
    /// Bytes typed or sent by the client on the session channel.
    Data(Vec<u8>),
    /// The client will send no more data.
    Eof,
    /// The session channel is closed; the connection can be dropped.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The client sent something that is not SSH or breaks the protocol.
    Protocol(&'static str),
    /// No algorithm in common.
    Negotiation(&'static str),
    /// A packet failed authentication (tampering or desynchronization).
    Mac,
    /// The client disconnected.
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Version,
    Kex,
    Auth,
    Connected,
    Closed,
}

struct Channel {
    remote_id: u32,
    remote_window: u32,
    remote_max_packet: u32,
    local_consumed: u32,
    pty: bool,
    /// A shell or command was started on it: one per channel.
    started: bool,
    sent_eof: bool,
    sent_close: bool,
    pending: Vec<u8>,
    /// `exit` was requested while `pending` could not be sent: finished
    /// once the client widens its window.
    exit_code: Option<u32>,
}

pub struct Server {
    phase: Phase,
    rng: Rng,
    host_seed: [u8; 32],
    host_public: [u8; 32],
    authorized: Vec<[u8; 32]>,
    input: Vec<u8>,
    client_version: Vec<u8>,
    our_kexinit: Vec<u8>,
    client_kexinit: Vec<u8>,
    strict: bool,
    session_id: Option<[u8; 32]>,
    /// Keys derived but not yet in use (until NEWKEYS in each direction).
    next_send: Option<ChaChaPoly>,
    next_recv: Option<ChaChaPoly>,
    send: Option<ChaChaPoly>,
    recv: Option<ChaChaPoly>,
    send_seq: u32,
    recv_seq: u32,
    /// Authentication requests counted against `MAX_AUTH_ATTEMPTS`.
    auth_attempts: u32,
    /// The client already asked for the methods (a first `none`).
    methods_asked: bool,
    user: Option<String>,
    channel: Option<Channel>,
}

/// Refused lines of one `authorized_keys` reported by number; more are
/// only counted.
pub const MAX_REPORTED_LINES: usize = 16;

/// Why a line of `authorized_keys` gives no key (incident 0046).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyLineError {
    /// It starts with options (`from=`, `command=`, `restrict`, ...).
    /// None is supported, and the key without them would have more power
    /// than its line gives it: the line is refused, never taken without
    /// its options.
    Options,
    /// Another key type (`ssh-rsa`, `ecdsa-sha2-nistp256`, a certificate):
    /// only `ssh-ed25519` keys log in.
    KeyType,
    /// Not a key line, or an `ssh-ed25519` key that does not decode.
    Malformed,
}

impl KeyLineError {
    /// Why, for the log.
    pub fn describe(self) -> &'static str {
        match self {
            KeyLineError::Options => {
                "it has options (from=, command=, restrict, ...), which are not supported"
            }
            KeyLineError::KeyType => "only ssh-ed25519 keys are supported",
            KeyLineError::Malformed => "not a valid ssh-ed25519 key line",
        }
    }
}

/// What an `authorized_keys` file gives: its keys and the lines it refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizedKeys {
    pub keys: Vec<[u8; 32]>,
    /// The first refused lines (number from 1, why), at most
    /// [`MAX_REPORTED_LINES`].
    pub refused: Vec<(usize, KeyLineError)>,
    /// Every refused line, reported or not.
    pub refused_total: usize,
}

/// A key type name of the OpenSSH format, supported or not.
fn is_key_type(field: &[u8]) -> bool {
    [b"ssh-".as_slice(), b"ecdsa-sha2-", b"sk-"]
        .iter()
        .any(|p| field.starts_with(p))
}

/// `authorized_keys` content (the OpenSSH format without options): the
/// `ssh-ed25519` keys it lists, and every other line that is not blank or
/// a `#` comment as refused, with the reason, so the owner learns that
/// its key does not log in.
pub fn parse_authorized_keys(text: &[u8]) -> AuthorizedKeys {
    let mut parsed = AuthorizedKeys::default();
    for (i, line) in text.split(|&b| b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut fields = line
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|f| !f.is_empty());
        let first = match fields.next() {
            None => continue,
            Some(f) if f.starts_with(b"#") => continue,
            Some(f) => f,
        };
        let key = if first == HOSTKEY {
            fields
                .next()
                .and_then(base64::decode)
                .and_then(|blob| {
                    let mut r = Reader::new(&blob);
                    (r.string() == Some(HOSTKEY))
                        .then(|| r.string().and_then(|k| <[u8; 32]>::try_from(k).ok()))
                        .flatten()
                })
                .ok_or(KeyLineError::Malformed)
        } else if is_key_type(first) {
            Err(KeyLineError::KeyType)
        } else if fields.any(is_key_type) {
            Err(KeyLineError::Options)
        } else {
            Err(KeyLineError::Malformed)
        };
        match key {
            Ok(k) => parsed.keys.push(k),
            Err(e) => {
                parsed.refused_total += 1;
                if parsed.refused.len() < MAX_REPORTED_LINES {
                    parsed.refused.push((i + 1, e));
                }
            }
        }
    }
    parsed
}

/// `string "ssh-ed25519" + string key`, the wire form of an Ed25519 key.
pub fn ed25519_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(HOSTKEY).string(public);
    w.0
}

/// `name` as it may go to a log: printable ASCII kept, every other byte
/// (newlines, escape sequences) as `?`, at most `out.len()` bytes. The user
/// name of an authentication attempt comes from the network before anyone
/// is authenticated (audit 2026-10-25 E, L2: forged log lines, terminal
/// escapes).
pub fn log_safe<'a>(name: &[u8], out: &'a mut [u8; 32]) -> &'a [u8] {
    let n = name.len().min(out.len());
    for (o, &b) in out.iter_mut().zip(&name[..n]) {
        *o = if (0x20..0x7f).contains(&b) { b } else { b'?' };
    }
    &out[..n]
}

/// `SHA256:...` fingerprint of a host key, as `ssh` prints it.
pub fn fingerprint(public: &[u8; 32]) -> String {
    let digest = aricode_crypto::sha256::sha256(&ed25519_blob(public));
    let mut s = String::from("SHA256:");
    s.push_str(core::str::from_utf8(&base64::encode(&digest, false)).unwrap_or(""));
    s
}

impl Server {
    /// `host_seed`: the Ed25519 host key; `rng_seed`: fresh randomness for
    /// this connection; `authorized`: keys allowed to log in.
    pub fn new(host_seed: [u8; 32], rng_seed: [u8; 32], authorized: Vec<[u8; 32]>) -> Self {
        Self {
            phase: Phase::Version,
            rng: Rng::new(rng_seed),
            host_public: ed25519::public_key(&host_seed),
            host_seed,
            authorized,
            input: Vec::new(),
            client_version: Vec::new(),
            our_kexinit: Vec::new(),
            client_kexinit: Vec::new(),
            strict: false,
            session_id: None,
            next_send: None,
            next_recv: None,
            send: None,
            recv: None,
            send_seq: 0,
            recv_seq: 0,
            auth_attempts: 0,
            methods_asked: false,
            user: None,
            channel: None,
        }
    }

    pub fn host_public(&self) -> [u8; 32] {
        self.host_public
    }

    /// The client has proved it holds an authorized key.
    pub fn is_authenticated(&self) -> bool {
        matches!(self.phase, Phase::Connected)
    }

    /// Drops the connection with a DISCONNECT (reason 2, protocol error)
    /// carrying `text`; used for clients that do not progress.
    pub fn drop_connection(&mut self, text: &[u8], out: &mut Vec<u8>) {
        self.disconnect(2, text, out);
    }

    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Sends the version line and our KEXINIT.
    pub fn start(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(VERSION);
        out.extend_from_slice(b"\r\n");
        let mut cookie = [0u8; 16];
        self.rng.fill(&mut cookie);
        let mut w = Writer::new(msg::KEXINIT);
        w.bytes(&cookie);
        let mut kex = Vec::new();
        for (i, n) in [KEX, KEX_LIBSSH, STRICT_S].iter().enumerate() {
            if i > 0 {
                kex.push(b',');
            }
            kex.extend_from_slice(n);
        }
        w.string(&kex).string(HOSTKEY);
        w.string(CIPHER)
            .string(CIPHER)
            .string(MAC)
            .string(MAC)
            .string(NONE)
            .string(NONE);
        w.string(b"").string(b"").bool(false).u32(0);
        self.our_kexinit = w.0.clone();
        self.send_packet(&w.0, out);
    }

    /// Processes bytes from the client.
    pub fn feed(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let r = self.feed_packets(data, out, events);
        // The client hears why it is refused (KEY_EXCHANGE_FAILED or
        // PROTOCOL_ERROR, RFC 4253 11.1), not just a closed connection
        // (audit of incident 0035, M1).
        match r {
            Err(Error::Negotiation(m)) if self.phase != Phase::Closed => {
                self.disconnect(3, m.as_bytes(), out)
            }
            Err(Error::Protocol(m)) if self.phase != Phase::Closed => {
                self.disconnect(2, m.as_bytes(), out)
            }
            _ => {}
        }
        r
    }

    fn feed_packets(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        self.input.extend_from_slice(data);
        if self.phase == Phase::Version {
            // Lines before "SSH-" are allowed (RFC 4253 section 4.2).
            loop {
                let Some(nl) = self.input.iter().position(|&b| b == b'\n') else {
                    if self.input.len() > 8192 {
                        return Err(Error::Protocol("no SSH version line"));
                    }
                    return Ok(());
                };
                let line: Vec<u8> = self.input.drain(..=nl).collect();
                let line = line.strip_suffix(b"\n").unwrap_or(&line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.starts_with(b"SSH-") {
                    if !line.starts_with(b"SSH-2.0-") && !line.starts_with(b"SSH-1.99-") {
                        return Err(Error::Protocol("unsupported SSH protocol version"));
                    }
                    self.client_version = line.to_vec();
                    self.phase = Phase::Kex;
                    break;
                }
            }
        }
        while let Some(payload) = self.next_packet()? {
            self.handle(&payload, out, events)?;
            if self.phase == Phase::Closed {
                break;
            }
        }
        Ok(())
    }

    /// Sends data to the client on the session channel (respecting its
    /// window; what does not fit waits for a window adjust). Newlines become
    /// CR LF when the client asked for a terminal.
    pub fn send_data(&mut self, data: &[u8], out: &mut Vec<u8>) {
        let Some(ch) = self.channel.as_mut() else {
            return;
        };
        if ch.sent_eof {
            return;
        }
        if ch.pty {
            for &b in data {
                if b == b'\n' {
                    ch.pending.push(b'\r');
                }
                ch.pending.push(b);
            }
        } else {
            ch.pending.extend_from_slice(data);
        }
        self.flush_channel(out);
    }

    /// Bytes given to `send_data` that still wait for the client's window.
    pub fn pending_len(&self) -> usize {
        self.channel.as_ref().map_or(0, |ch| ch.pending.len())
    }

    /// Ends the session: exit status, EOF and close, once every byte given
    /// to `send_data` has gone out (the rest waits for the client's window).
    pub fn exit(&mut self, code: u32, out: &mut Vec<u8>) {
        self.flush_channel(out);
        let Some(ch) = self.channel.as_mut() else {
            return;
        };
        if ch.sent_close {
            return;
        }
        if !ch.pending.is_empty() {
            ch.exit_code = Some(code);
            return;
        }
        let id = ch.remote_id;
        ch.sent_eof = true;
        ch.sent_close = true;
        let mut w = Writer::new(msg::CHANNEL_REQUEST);
        w.u32(id).string(b"exit-status").bool(false).u32(code);
        self.send_packet(&w.0, out);
        let mut w = Writer::new(msg::CHANNEL_EOF);
        w.u32(id);
        self.send_packet(&w.0, out);
        let mut w = Writer::new(msg::CHANNEL_CLOSE);
        w.u32(id);
        self.send_packet(&w.0, out);
    }

    fn flush_channel(&mut self, out: &mut Vec<u8>) {
        loop {
            let Some(ch) = self.channel.as_mut() else {
                return;
            };
            let n = (ch.pending.len() as u32)
                .min(ch.remote_window)
                .min(ch.remote_max_packet.saturating_sub(64).max(1)) as usize;
            if n == 0 {
                return;
            }
            let chunk: Vec<u8> = ch.pending.drain(..n).collect();
            ch.remote_window -= n as u32;
            let id = ch.remote_id;
            let mut w = Writer::new(msg::CHANNEL_DATA);
            w.u32(id).string(&chunk);
            self.send_packet(&w.0, out);
        }
    }

    fn disconnect(&mut self, reason: u32, text: &[u8], out: &mut Vec<u8>) {
        let mut w = Writer::new(msg::DISCONNECT);
        w.u32(reason).string(text).string(b"");
        self.send_packet(&w.0, out);
        self.phase = Phase::Closed;
    }

    // ------------------------------------------------------------ packets

    fn send_packet(&mut self, payload: &[u8], out: &mut Vec<u8>) {
        // The nonce is the sequence number and keys are never exchanged
        // again: past `MAX_PACKETS` nothing more is sent, whatever window
        // the client granted (audit of incident 0035, L1).
        if self.send_seq >= MAX_PACKETS {
            self.phase = Phase::Closed;
            return;
        }
        // Block size 8; with the AEAD the length field is not counted.
        let base = if self.send.is_some() { 1 } else { 5 } + payload.len();
        let mut pad = 8 - base % 8;
        if pad < 4 {
            pad += 8;
        }
        let mut packet = Vec::with_capacity(4 + 1 + payload.len() + pad + TAG_LEN);
        packet.extend_from_slice(&((1 + payload.len() + pad) as u32).to_be_bytes());
        packet.push(pad as u8);
        packet.extend_from_slice(payload);
        let start = packet.len();
        packet.resize(start + pad, 0);
        self.rng.fill(&mut packet[start..]);
        if let Some(c) = &self.send {
            let tag = c.seal(self.send_seq, &mut packet);
            packet.extend_from_slice(&tag);
        }
        self.send_seq = self.send_seq.wrapping_add(1);
        out.extend_from_slice(&packet);
    }

    /// Next complete packet's payload, if the input holds one.
    fn next_packet(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if self.phase == Phase::Version || self.input.len() < 4 {
            return Ok(None);
        }
        let first: [u8; 4] = self.input[..4].try_into().unwrap();
        let len = match &self.recv {
            Some(c) => c.decrypt_length(self.recv_seq, &first),
            None => u32::from_be_bytes(first),
        } as usize;
        if !(5..=256 * 1024).contains(&len) {
            return Err(Error::Protocol("bad packet length"));
        }
        let tag_len = if self.recv.is_some() { TAG_LEN } else { 0 };
        if self.input.len() < 4 + len + tag_len {
            return Ok(None);
        }
        let mut packet: Vec<u8> = self.input.drain(..4 + len + tag_len).collect();
        if let Some(c) = &self.recv {
            let tag: [u8; TAG_LEN] = packet[4 + len..].try_into().unwrap();
            packet.truncate(4 + len);
            if !c.open(self.recv_seq, &mut packet, &tag) {
                return Err(Error::Mac);
            }
        }
        self.recv_seq = self.recv_seq.wrapping_add(1);
        // The ChaCha20 nonce is the sequence number and keys are never
        // exchanged again: a counter must never wrap (audit 2026-10-25 C,
        // L10; D, L6). `send_packet` bounds the other direction.
        if self.recv_seq >= MAX_PACKETS {
            return Err(Error::Protocol(
                "too many packets without a key re-exchange",
            ));
        }
        let pad = packet[4] as usize;
        if pad + 1 > len {
            return Err(Error::Protocol("bad padding"));
        }
        Ok(Some(packet[5..4 + len - pad].to_vec()))
    }

    // ----------------------------------------------------------- dispatch

    fn handle(
        &mut self,
        payload: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let Some(&kind) = payload.first() else {
            return Err(Error::Protocol("empty packet"));
        };
        let mut r = Reader::new(&payload[1..]);
        match kind {
            msg::DISCONNECT => {
                self.phase = Phase::Closed;
                return Err(Error::Disconnected);
            }
            // Strict KEX forbids anything but KEX messages during the first
            // exchange; afterwards these are harmless.
            msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED | msg::EXT_INFO
                if !self.in_strict_kex() => {}
            msg::KEXINIT if self.phase == Phase::Kex && self.client_kexinit.is_empty() => {
                self.client_kexinit = payload.to_vec();
                self.negotiate(&mut r)?;
            }
            msg::KEX_ECDH_INIT
                if self.phase == Phase::Kex
                    && !self.client_kexinit.is_empty()
                    && self.next_recv.is_none() =>
            {
                let q_c = r.string().ok_or(Error::Protocol("bad ECDH_INIT"))?;
                self.key_exchange(q_c, out)?;
            }
            msg::NEWKEYS if self.phase == Phase::Kex && self.next_recv.is_some() => {
                self.recv = self.next_recv.take();
                if self.strict {
                    self.recv_seq = 0;
                }
                self.phase = Phase::Auth;
            }
            _ if self.in_strict_kex() => {
                return Err(Error::Protocol(
                    "unexpected message during strict key exchange",
                ))
            }
            msg::KEXINIT => return Err(Error::Protocol("key re-exchange is not supported")),
            msg::SERVICE_REQUEST if self.phase == Phase::Auth => {
                let name = r.string().ok_or(Error::Protocol("bad SERVICE_REQUEST"))?;
                if name != b"ssh-userauth" {
                    self.disconnect(7, b"service not available", out);
                    return Ok(());
                }
                let mut w = Writer::new(msg::SERVICE_ACCEPT);
                w.string(b"ssh-userauth");
                self.send_packet(&w.0, out);
            }
            msg::USERAUTH_REQUEST if self.phase == Phase::Auth => {
                self.userauth(&mut r, out, events)?
            }
            msg::USERAUTH_REQUEST => {} // after success: ignore (RFC 4252 section 5.1)
            msg::GLOBAL_REQUEST => {
                let _name = r.string();
                if r.bool() == Some(true) {
                    self.send_packet(&[msg::REQUEST_FAILURE], out);
                }
            }
            msg::CHANNEL_OPEN if self.phase == Phase::Connected => {
                self.channel_open(&mut r, out)?
            }
            msg::CHANNEL_REQUEST if self.phase == Phase::Connected => {
                self.channel_request(&mut r, out, events)?
            }
            msg::CHANNEL_DATA if self.phase == Phase::Connected => {
                let _id = r.u32();
                let data = r.string().ok_or(Error::Protocol("bad CHANNEL_DATA"))?;
                if let Some(ch) = self.channel.as_mut() {
                    ch.local_consumed += data.len() as u32;
                    events.push(Event::Data(data.to_vec()));
                    if ch.local_consumed >= RECV_WINDOW / 2 {
                        let add = ch.local_consumed;
                        ch.local_consumed = 0;
                        let id = ch.remote_id;
                        let mut w = Writer::new(msg::CHANNEL_WINDOW_ADJUST);
                        w.u32(id).u32(add);
                        self.send_packet(&w.0, out);
                    }
                }
            }
            msg::CHANNEL_WINDOW_ADJUST if self.phase == Phase::Connected => {
                let _id = r.u32();
                let add = r.u32().ok_or(Error::Protocol("bad WINDOW_ADJUST"))?;
                if let Some(ch) = self.channel.as_mut() {
                    ch.remote_window = ch.remote_window.saturating_add(add);
                }
                self.flush_channel(out);
                // A deferred exit completes once nothing is pending.
                if let Some(code) = self
                    .channel
                    .as_ref()
                    .filter(|c| c.pending.is_empty())
                    .and_then(|c| c.exit_code)
                {
                    self.exit(code, out);
                }
            }
            msg::CHANNEL_EOF if self.phase == Phase::Connected => events.push(Event::Eof),
            msg::CHANNEL_CLOSE if self.phase == Phase::Connected => {
                if let Some(ch) = self.channel.as_mut() {
                    if !ch.sent_close {
                        ch.sent_close = true;
                        let mut w = Writer::new(msg::CHANNEL_CLOSE);
                        w.u32(ch.remote_id);
                        self.send_packet(&w.0, out);
                    }
                }
                self.channel = None;
                events.push(Event::Closed);
            }
            _ if self.phase == Phase::Auth || self.phase == Phase::Connected => {
                let mut w = Writer::new(msg::UNIMPLEMENTED);
                w.u32(self.recv_seq.wrapping_sub(1));
                self.send_packet(&w.0, out);
            }
            _ => return Err(Error::Protocol("unexpected message")),
        }
        Ok(())
    }

    /// During the initial key exchange with strict KEX, only KEX messages
    /// are allowed (Terrapin mitigation).
    /// Strict key exchange is required of every client, so it holds from
    /// the first packet: only KEX messages until NEWKEYS (audit of incident
    /// 0035, L4).
    fn in_strict_kex(&self) -> bool {
        self.phase == Phase::Kex
    }

    fn negotiate(&mut self, r: &mut Reader) -> Result<(), Error> {
        r.take(16).ok_or(Error::Protocol("bad KEXINIT"))?;
        let mut lists: [&[u8]; 10] = [&[]; 10];
        for l in lists.iter_mut() {
            *l = r.string().ok_or(Error::Protocol("bad KEXINIT"))?;
        }
        negotiate(lists[0], &[KEX, KEX_LIBSSH])
            .ok_or(Error::Negotiation("no common key exchange"))?;
        negotiate(lists[1], &[HOSTKEY]).ok_or(Error::Negotiation(
            "client does not accept ssh-ed25519 host keys",
        ))?;
        for l in &lists[2..4] {
            negotiate(l, &[CIPHER]).ok_or(Error::Negotiation(
                "client does not offer chacha20-poly1305@openssh.com",
            ))?;
        }
        for l in &lists[6..8] {
            negotiate(l, &[NONE]).ok_or(Error::Negotiation("client requires compression"))?;
        }
        // Strict key exchange is required: without it a man in the middle
        // can delete packets at the start of the encrypted stream
        // (Terrapin, CVE-2023-48795) with the cipher used here. OpenSSH 9.6
        // and PuTTY 0.80 and later offer it (audit 2026-10-25 D, L7).
        if !list_has(lists[0], STRICT_C) {
            return Err(Error::Negotiation(
                "client does not offer strict key exchange (kex-strict-c-v00@openssh.com)",
            ));
        }
        self.strict = true;
        if r.bool() == Some(true) {
            return Err(Error::Negotiation(
                "guessed key exchange packets are not supported",
            ));
        }
        Ok(())
    }

    fn key_exchange(&mut self, q_c: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        let q_c: [u8; 32] = q_c
            .try_into()
            .map_err(|_| Error::Protocol("bad client public key"))?;
        let mut secret = [0u8; 32];
        self.rng.fill(&mut secret);
        let q_s = x25519::public_key(&secret);
        let k = x25519::shared(&secret, &q_c).ok_or(Error::Protocol("low-order client key"))?;
        let k_s = ed25519_blob(&self.host_public);
        // H = HASH(V_C || V_S || I_C || I_S || K_S || Q_C || Q_S || K).
        let mut h = Writer::default();
        h.string(&self.client_version)
            .string(VERSION)
            .string(&self.client_kexinit)
            .string(&self.our_kexinit)
            .string(&k_s)
            .string(&q_c)
            .string(&q_s)
            .mpint(&k);
        let hash = aricode_crypto::sha256::sha256(&h.0);
        let session_id = *self.session_id.get_or_insert(hash);
        let sig = ed25519::sign(&self.host_seed, &hash);
        let mut sig_blob = Writer::default();
        sig_blob.string(HOSTKEY).string(&sig);
        let mut w = Writer::new(msg::KEX_ECDH_REPLY);
        w.string(&k_s).string(&q_s).string(&sig_blob.0);
        self.send_packet(&w.0, out);
        self.send_packet(&[msg::NEWKEYS], out);
        // Keys: C = client to server, D = server to client (RFC 4253 7.2).
        let derive = |letter: u8| -> [u8; 64] {
            let mut mk = Writer::default();
            mk.mpint(&k);
            let mut first = Sha256::new();
            first.update(&mk.0);
            first.update(&hash);
            first.update(&[letter]);
            first.update(&session_id);
            let k1 = first.finish();
            let mut second = Sha256::new();
            second.update(&mk.0);
            second.update(&hash);
            second.update(&k1);
            let k2 = second.finish();
            let mut key = [0u8; 64];
            key[..32].copy_from_slice(&k1);
            key[32..].copy_from_slice(&k2);
            key
        };
        self.send = Some(ChaChaPoly::new(&derive(b'D')));
        if self.strict {
            self.send_seq = 0;
        }
        self.next_recv = Some(ChaChaPoly::new(&derive(b'C')));
        self.next_send = None;
        Ok(())
    }

    fn userauth(
        &mut self,
        r: &mut Reader,
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let bad = Error::Protocol("bad USERAUTH_REQUEST");
        let user = r.string().ok_or(bad)?;
        let service = r.string().ok_or(bad)?;
        let method = r.string().ok_or(bad)?;
        if service != b"ssh-connection" {
            self.disconnect(7, b"service not available", out);
            return Ok(());
        }
        let mut reply = Writer::new(msg::USERAUTH_FAILURE);
        reply.string(b"publickey").bool(false);
        // A key or method refused, as opposed to a query answered.
        let mut refused = true;
        if method == NONE {
            // A query for the supported methods (RFC 4252 5.2): the first
            // one is free, a repeated one costs an attempt.
            refused = false;
            if !self.methods_asked {
                self.methods_asked = true;
                self.send_packet(&reply.0, out);
                return Ok(());
            }
        } else if method == b"publickey" {
            let has_sig = r.bool().ok_or(bad)?;
            let alg = r.string().ok_or(bad)?;
            let blob = r.string().ok_or(bad)?;
            let key = if alg == HOSTKEY {
                let mut kr = Reader::new(blob);
                (kr.string() == Some(HOSTKEY))
                    .then(|| kr.string().and_then(|k| <[u8; 32]>::try_from(k).ok()))
                    .flatten()
                    .filter(|k| self.authorized.contains(k))
            } else {
                None
            };
            match key {
                // "Would this key be accepted?": answered, and counted.
                Some(_) if !has_sig => {
                    reply = Writer::new(msg::USERAUTH_PK_OK);
                    reply.string(alg).string(blob);
                    refused = false;
                }
                Some(key) => {
                    let sig_blob = r.string().ok_or(bad)?;
                    let mut sr = Reader::new(sig_blob);
                    let ok = sr.string() == Some(HOSTKEY)
                        && sr
                            .string()
                            .and_then(|s| <[u8; 64]>::try_from(s).ok())
                            .is_some_and(|sig| {
                                let mut data = Writer::default();
                                data.string(&self.session_id.unwrap_or_default())
                                    .u8(msg::USERAUTH_REQUEST)
                                    .string(user)
                                    .string(service)
                                    .string(b"publickey")
                                    .bool(true)
                                    .string(alg)
                                    .string(blob);
                                ed25519::verify(&key, &data.0, &sig)
                            });
                    if ok {
                        self.send_packet(&[msg::USERAUTH_SUCCESS], out);
                        let user = String::from_utf8_lossy(user).into_owned();
                        self.user = Some(user.clone());
                        self.phase = Phase::Connected;
                        events.push(Event::Authenticated { user });
                        return Ok(());
                    }
                }
                None => {}
            }
        }
        let user = String::from_utf8_lossy(user).into_owned();
        self.auth_attempts += 1;
        if refused {
            events.push(Event::AuthFailed { user: user.clone() });
        }
        if self.auth_attempts >= MAX_AUTH_ATTEMPTS {
            events.push(Event::TooManyAuthAttempts { user });
            self.disconnect(14, b"too many authentication attempts", out);
            return Ok(());
        }
        self.send_packet(&reply.0, out);
        Ok(())
    }

    fn channel_open(&mut self, r: &mut Reader, out: &mut Vec<u8>) -> Result<(), Error> {
        let bad = Error::Protocol("bad CHANNEL_OPEN");
        let kind = r.string().ok_or(bad)?;
        let sender = r.u32().ok_or(bad)?;
        let window = r.u32().ok_or(bad)?;
        let max_packet = r.u32().ok_or(bad)?;
        if kind != b"session" || self.channel.is_some() {
            let mut w = Writer::new(msg::CHANNEL_OPEN_FAILURE);
            w.u32(sender)
                .u32(if kind == b"session" { 4 } else { 3 })
                .string(if kind == b"session" {
                    b"only one session"
                } else {
                    b"unknown channel type"
                })
                .string(b"");
            self.send_packet(&w.0, out);
            return Ok(());
        }
        self.channel = Some(Channel {
            remote_id: sender,
            remote_window: window,
            remote_max_packet: max_packet.max(256),
            local_consumed: 0,
            pty: false,
            started: false,
            sent_eof: false,
            sent_close: false,
            pending: Vec::new(),
            exit_code: None,
        });
        let mut w = Writer::new(msg::CHANNEL_OPEN_CONFIRMATION);
        w.u32(sender).u32(0).u32(RECV_WINDOW).u32(MAX_PACKET);
        self.send_packet(&w.0, out);
        Ok(())
    }

    fn channel_request(
        &mut self,
        r: &mut Reader,
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let bad = Error::Protocol("bad CHANNEL_REQUEST");
        let _id = r.u32().ok_or(bad)?;
        let kind = r.string().ok_or(bad)?;
        let want_reply = r.bool().ok_or(bad)?;
        let Some(ch) = self.channel.as_mut() else {
            return Ok(());
        };
        let remote = ch.remote_id;
        let ok = match kind {
            b"pty-req" => {
                ch.pty = true;
                true
            }
            b"env" | b"window-change" => true,
            // One shell or command per channel, never on one this side has
            // already closed (audit 2026-10-25 C, L8).
            b"shell" | b"exec" if ch.started || ch.sent_eof || ch.sent_close => false,
            b"shell" => {
                ch.started = true;
                events.push(Event::Shell { pty: ch.pty });
                true
            }
            b"exec" => {
                let command = r.string().ok_or(bad)?.to_vec();
                ch.started = true;
                events.push(Event::Exec {
                    command,
                    pty: ch.pty,
                });
                true
            }
            _ => false,
        };
        if want_reply {
            let mut w = Writer::new(if ok {
                msg::CHANNEL_SUCCESS
            } else {
                msg::CHANNEL_FAILURE
            });
            w.u32(remote);
            self.send_packet(&w.0, out);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_keys_parsing() {
        let pk = [7u8; 32];
        let line = [
            b"ssh-ed25519 ".as_slice(),
            &base64::encode(&ed25519_blob(&pk), true),
            b" me@host\r\n",
        ]
        .concat();
        let text = [
            b"# comment\nssh-rsa AAAA x\n".as_slice(),
            &line,
            b"ssh-ed25519 !!!bad\n",
        ]
        .concat();
        let parsed = parse_authorized_keys(&text);
        assert_eq!(parsed.keys, alloc::vec![pk]);
        assert_eq!(
            parsed.refused,
            alloc::vec![(2, KeyLineError::KeyType), (4, KeyLineError::Malformed)]
        );
        assert!(fingerprint(&pk).starts_with("SHA256:"));
    }

    /// Incident 0046: a line with options was skipped without a word, so
    /// the owner never learned that its key does not log in.
    #[test]
    fn lines_with_options_are_refused_by_number() {
        let key = base64::encode(&ed25519_blob(&[7u8; 32]), true);
        let with = |prefix: &str| [prefix.as_bytes(), &key, b" me@host\n"].concat();
        let text = [
            with("ssh-ed25519 "),
            b"\n   \n\t# indented comment\n".to_vec(),
            with("from=\"10.0.0.1\",command=\"ls -l\" ssh-ed25519 "),
            with("restrict ssh-ed25519 "),
            with("no-pty\tssh-ed25519 "),
            b"ecdsa-sha2-nistp256 AAAA x\nssh-ed25519-cert-v01@openssh.com AAAA x\n".to_vec(),
            b"just some words\n".to_vec(),
        ]
        .concat();
        let parsed = parse_authorized_keys(&text);
        assert_eq!(parsed.keys, alloc::vec![[7u8; 32]]);
        use KeyLineError::{KeyType, Malformed, Options};
        assert_eq!(
            parsed.refused,
            alloc::vec![
                (5, Options),
                (6, Options),
                (7, Options),
                (8, KeyType),
                (9, KeyType),
                (10, Malformed)
            ]
        );
        assert_eq!(parsed.refused_total, 6);
        // Reported by number up to a bound, all counted.
        let many = with("restrict ssh-ed25519 ").repeat(MAX_REPORTED_LINES + 5);
        let parsed = parse_authorized_keys(&many);
        assert!(parsed.keys.is_empty());
        assert_eq!(parsed.refused.len(), MAX_REPORTED_LINES);
        assert_eq!(parsed.refused_total, MAX_REPORTED_LINES + 5);
        assert_eq!(parsed.refused[MAX_REPORTED_LINES - 1].0, MAX_REPORTED_LINES);
    }

    /// One USERAUTH_REQUEST (after its message byte) to a server in the
    /// authentication phase: the message type of the reply, and the events.
    fn auth_request(s: &mut Server, method: &[u8], rest: impl Fn(&mut Writer)) -> (u8, Vec<Event>) {
        let mut w = Writer::default();
        w.string(b"ari").string(b"ssh-connection").string(method);
        rest(&mut w);
        let mut out = Vec::new();
        let mut events = Vec::new();
        s.userauth(&mut Reader::new(&w.0), &mut out, &mut events)
            .unwrap();
        (out[5], events)
    }

    /// Incident 0046: `none` requests and key queries were not counted, so
    /// a client could probe keys and keep the server answering for ever.
    #[test]
    fn every_auth_request_but_the_first_none_counts() {
        let pk = ed25519::public_key(&[5; 32]);
        let blob = ed25519_blob(&pk);
        let other = ed25519_blob(&[9; 32]);
        let query = |key: &[u8]| {
            let key = key.to_vec();
            move |w: &mut Writer| {
                w.bool(false).string(HOSTKEY).string(&key);
            }
        };
        let fresh = || {
            let mut s = Server::new([1; 32], [2; 32], alloc::vec![pk]);
            s.phase = Phase::Auth;
            s
        };
        let user = || String::from("ari");
        // Repeated `none`: the first is free, nine more are answered, the
        // tenth counted one disconnects, said once.
        let mut s = fresh();
        for _ in 0..MAX_AUTH_ATTEMPTS {
            assert_eq!(
                auth_request(&mut s, NONE, |_| {}),
                (msg::USERAUTH_FAILURE, alloc::vec![])
            );
        }
        assert_eq!(
            auth_request(&mut s, NONE, |_| {}),
            (
                msg::DISCONNECT,
                alloc::vec![Event::TooManyAuthAttempts { user: user() }]
            )
        );
        assert!(s.is_closed());
        // Queries for an authorized key: answered PK_OK, and counted.
        let mut s = fresh();
        for _ in 1..MAX_AUTH_ATTEMPTS {
            assert_eq!(
                auth_request(&mut s, b"publickey", query(&blob)).0,
                msg::USERAUTH_PK_OK
            );
        }
        assert_eq!(
            auth_request(&mut s, b"publickey", query(&blob)).0,
            msg::DISCONNECT
        );
        // A refused key is reported, and counted with the rest.
        let mut s = fresh();
        assert_eq!(auth_request(&mut s, NONE, |_| {}).0, msg::USERAUTH_FAILURE);
        assert_eq!(
            auth_request(&mut s, b"publickey", query(&other)),
            (
                msg::USERAUTH_FAILURE,
                alloc::vec![Event::AuthFailed { user: user() }]
            )
        );
        assert_eq!(
            auth_request(&mut s, b"password", |_| {}).1,
            alloc::vec![Event::AuthFailed { user: user() }]
        );
        for _ in 3..MAX_AUTH_ATTEMPTS {
            assert_eq!(auth_request(&mut s, NONE, |_| {}).0, msg::USERAUTH_FAILURE);
        }
        assert_eq!(
            auth_request(&mut s, b"publickey", query(&blob)).0,
            msg::DISCONNECT
        );
        // A usual login (none, a query, the signed request) stays well
        // within the limit.
        let mut s = fresh();
        s.session_id = Some([3; 32]);
        assert_eq!(auth_request(&mut s, NONE, |_| {}).0, msg::USERAUTH_FAILURE);
        assert_eq!(
            auth_request(&mut s, b"publickey", query(&blob)).0,
            msg::USERAUTH_PK_OK
        );
        let mut signed = Writer::default();
        signed
            .string(&[3; 32])
            .u8(msg::USERAUTH_REQUEST)
            .string(b"ari")
            .string(b"ssh-connection")
            .string(b"publickey")
            .bool(true)
            .string(HOSTKEY)
            .string(&blob);
        let sig = ed25519::sign(&[5; 32], &signed.0);
        let mut sig_blob = Writer::default();
        sig_blob.string(HOSTKEY).string(&sig);
        let (reply, events) = auth_request(&mut s, b"publickey", |w| {
            w.bool(true)
                .string(HOSTKEY)
                .string(&blob)
                .string(&sig_blob.0);
        });
        assert_eq!(reply, msg::USERAUTH_SUCCESS);
        assert_eq!(events, alloc::vec![Event::Authenticated { user: user() }]);
    }

    #[test]
    fn version_and_kexinit_are_sent_and_garbage_is_rejected() {
        let mut s = Server::new([1; 32], [2; 32], Vec::new());
        let mut out = Vec::new();
        s.start(&mut out);
        assert!(out.starts_with(b"SSH-2.0-AriCodeOS_sshd_1.0\r\n"));
        assert_eq!(out[VERSION.len() + 2 + 5], msg::KEXINIT);
        let mut ev = Vec::new();
        assert!(s
            .feed(b"banner line\r\nSSH-2.0-test\r\n", &mut out, &mut ev)
            .is_ok());
        assert_eq!(s.phase, Phase::Kex);
        let mut s = Server::new([1; 32], [2; 32], Vec::new());
        assert_eq!(
            s.feed(b"SSH-1.5-old\r\n", &mut out, &mut ev),
            Err(Error::Protocol("unsupported SSH protocol version"))
        );
    }

    /// A client KEXINIT packet (unencrypted) offering `kex` as its key
    /// exchange list and what the server supports otherwise.
    fn client_kexinit(kex: &[u8]) -> Vec<u8> {
        let mut payload = alloc::vec![msg::KEXINIT];
        payload.extend_from_slice(&[7; 16]);
        let lists: [&[u8]; 10] = [
            kex,
            b"ssh-ed25519",
            b"chacha20-poly1305@openssh.com",
            b"chacha20-poly1305@openssh.com",
            b"",
            b"",
            b"none",
            b"none",
            b"",
            b"",
        ];
        for l in lists {
            payload.extend_from_slice(&(l.len() as u32).to_be_bytes());
            payload.extend_from_slice(l);
        }
        payload.push(0); // first_kex_packet_follows
        payload.extend_from_slice(&[0; 4]);
        let mut pad = 8 - (5 + payload.len()) % 8;
        if pad < 4 {
            pad += 8;
        }
        let len = 1 + payload.len() + pad;
        let mut packet = (len as u32).to_be_bytes().to_vec();
        packet.push(pad as u8);
        packet.extend_from_slice(&payload);
        packet.extend(core::iter::repeat_n(0, pad));
        packet
    }

    /// Audit 2026-10-25 D, L7: a client without strict key exchange is open
    /// to Terrapin with this cipher; it is refused.
    #[test]
    fn strict_key_exchange_is_required() {
        let mut out = Vec::new();
        let mut ev = Vec::new();
        let mut s = Server::new([1; 32], [2; 32], Vec::new());
        s.start(&mut out);
        let mut input = b"SSH-2.0-test\r\n".to_vec();
        input.extend(client_kexinit(b"curve25519-sha256"));
        out.clear();
        assert_eq!(
            s.feed(&input, &mut out, &mut ev),
            Err(Error::Negotiation(
                "client does not offer strict key exchange (kex-strict-c-v00@openssh.com)"
            ))
        );
        // The client is told: a DISCONNECT (plain, before NEWKEYS) with
        // reason KEY_EXCHANGE_FAILED.
        assert_eq!(out[5], msg::DISCONNECT);
        assert_eq!(u32::from_be_bytes(out[6..10].try_into().unwrap()), 3);
        let mut s = Server::new([1; 32], [2; 32], Vec::new());
        s.start(&mut out);
        let mut input = b"SSH-2.0-test\r\n".to_vec();
        input.extend(client_kexinit(
            b"curve25519-sha256,kex-strict-c-v00@openssh.com",
        ));
        assert_eq!(s.feed(&input, &mut out, &mut ev), Ok(()));
        assert!(s.strict);
    }

    /// Audit 2026-10-25 E, L2: a user name from the network goes to the log
    /// without control bytes, bounded.
    #[test]
    fn user_names_are_made_log_safe() {
        let mut b = [0u8; 32];
        assert_eq!(log_safe(b"ari", &mut b), b"ari");
        assert_eq!(
            log_safe(b"x' authenticated\n\x1b[2Jroot", &mut b),
            b"x' authenticated??[2Jroot"
        );
        assert_eq!(log_safe(&[b'a'; 100], &mut b).len(), 32);
    }
}
