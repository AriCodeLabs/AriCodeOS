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
const STRICT_C: &[u8] = b"kex-strict-c-v00@openssh.com";
const HOSTKEY: &[u8] = b"ssh-ed25519";
const CIPHER: &[u8] = b"chacha20-poly1305@openssh.com";
/// Ignored with an AEAD cipher, but the lists must have a common name.
const MAC: &[u8] = b"hmac-sha2-256";
const NONE: &[u8] = b"none";

/// Window we grant the client, and when to top it up.
const RECV_WINDOW: u32 = 1 << 20;
const MAX_PACKET: u32 = 32 * 1024;
const MAX_AUTH_FAILURES: u32 = 10;

/// What happened, for the caller to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The client proved it holds an authorized key.
    Authenticated { user: String },
    /// A public-key attempt was refused (key not authorized or bad
    /// signature); the connection goes on until `MAX_AUTH_FAILURES`.
    AuthFailed { user: String },
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
    auth_failures: u32,
    user: Option<String>,
    channel: Option<Channel>,
}

/// `authorized_keys` content: the `ssh-ed25519` public keys it lists.
pub fn parse_authorized_keys(text: &[u8]) -> Vec<[u8; 32]> {
    let mut keys = Vec::new();
    for line in text.split(|&b| b == b'\n') {
        let mut fields = line
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|f| !f.is_empty());
        if fields.next() != Some(HOSTKEY) {
            continue;
        }
        let Some(blob) = fields
            .next()
            .and_then(|b| base64::decode(b.strip_suffix(b"\r").unwrap_or(b)))
        else {
            continue;
        };
        let mut r = Reader::new(&blob);
        if r.string() == Some(HOSTKEY) {
            if let Some(k) = r.string().and_then(|k| <[u8; 32]>::try_from(k).ok()) {
                keys.push(k);
            }
        }
    }
    keys
}

/// `string "ssh-ed25519" + string key`, the wire form of an Ed25519 key.
pub fn ed25519_blob(public: &[u8; 32]) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(HOSTKEY).string(public);
    w.0
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
            auth_failures: 0,
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
    fn in_strict_kex(&self) -> bool {
        self.strict && self.phase == Phase::Kex
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
        self.strict = list_has(lists[0], STRICT_C);
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
        if method == NONE {
            // A query for the supported methods (RFC 4252 5.2), not an attempt.
            let mut w = Writer::new(msg::USERAUTH_FAILURE);
            w.string(b"publickey").bool(false);
            self.send_packet(&w.0, out);
            return Ok(());
        }
        if method == b"publickey" {
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
            if let Some(key) = key {
                if !has_sig {
                    let mut w = Writer::new(msg::USERAUTH_PK_OK);
                    w.string(alg).string(blob);
                    self.send_packet(&w.0, out);
                    return Ok(());
                }
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
        }
        self.auth_failures += 1;
        events.push(Event::AuthFailed {
            user: String::from_utf8_lossy(user).into_owned(),
        });
        if self.auth_failures > MAX_AUTH_FAILURES {
            self.disconnect(14, b"too many authentication failures", out);
            return Ok(());
        }
        let mut w = Writer::new(msg::USERAUTH_FAILURE);
        w.string(b"publickey").bool(false);
        self.send_packet(&w.0, out);
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
            b"shell" => {
                events.push(Event::Shell { pty: ch.pty });
                true
            }
            b"exec" => {
                let command = r.string().ok_or(bad)?.to_vec();
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
        assert_eq!(parse_authorized_keys(&text), alloc::vec![pk]);
        assert!(fingerprint(&pk).starts_with("SHA256:"));
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
}
