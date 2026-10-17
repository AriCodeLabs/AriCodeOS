// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! TLS 1.3 client (RFC 8446) as a state machine: bytes from the server go
//! in through [`Client::feed`], bytes for the server and [`Event`]s come
//! out. No I/O and no threads, so it runs the same in AriCodeOS and in host
//! tests against `openssl s_server`.
//!
//! Supported (and only): TLS 1.3, key exchange x25519, cipher suites
//! TLS_CHACHA20_POLY1305_SHA256, TLS_AES_128_GCM_SHA256 and
//! TLS_AES_256_GCM_SHA384, server signatures ecdsa_secp256r1_sha256,
//! ecdsa_secp384r1_sha384, rsa_pss_rsae_sha256/384/512 and ed25519, with
//! the certificate chain checked by `aricode-x509`. No resumption, no
//! early data, no client certificates (an empty Certificate answers a
//! CertificateRequest), no HelloRetryRequest.

#![no_std]

extern crate alloc;

pub mod handshake;
pub mod keys;
pub mod record;
#[cfg(test)]
mod rfc8448;

use alloc::string::String;
use alloc::vec::Vec;
use aricode_crypto::chacha20::Rng;
use aricode_crypto::{ct_eq, x25519};
use keys::{Schedule, Secret, Suite};
use record::{Protection, ALERT, APPLICATION_DATA, CHANGE_CIPHER_SPEC, HANDSHAKE};

/// Largest handshake message we buffer (certificate chains included).
const MAX_HANDSHAKE: usize = 1 << 17;

const ALERT_CLOSE_NOTIFY: u8 = 0;
const ALERT_USER_CANCELED: u8 = 90;

/// What happened, for the caller to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The handshake finished: the server is authenticated and
    /// [`Client::send`] can be used.
    Connected,
    /// Application data from the server.
    Data(Vec<u8>),
    /// The server sent close_notify; the connection can be dropped.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The server broke the protocol (or the connection is already closed).
    Protocol(&'static str),
    /// The server sent this fatal alert.
    Alert(u8),
    /// The certificate chain was rejected.
    Certificate(aricode_x509::Error),
    /// The server wants something we do not implement.
    Unsupported(&'static str),
    /// A record failed authentication (tampering or desynchronization).
    Decrypt,
}

impl Error {
    /// The alert we send the server for this error.
    fn alert(self, connected: bool) -> Option<u8> {
        use aricode_x509::Error as X;
        Some(match self {
            Error::Alert(_) => return None,
            Error::Decrypt => 20,                       // bad_record_mac
            Error::Certificate(X::UnknownIssuer) => 48, // unknown_ca
            Error::Certificate(X::Expired | X::NotYetValid) => 45, // certificate_expired
            Error::Certificate(X::Unsupported | X::WrongPurpose) => 43, // unsupported_certificate
            Error::Certificate(_) => 42,                // bad_certificate
            Error::Protocol(_) if connected => 10,      // unexpected_message
            Error::Protocol(_) | Error::Unsupported(_) => 40, // handshake_failure
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    WaitServerHello,
    WaitEncryptedExtensions,
    WaitCertificate,
    WaitCertificateVerify,
    WaitFinished,
    Connected,
    Closed,
}

/// Secrets known after the ServerHello.
struct Handshake {
    schedule: Schedule,
    client: Secret,
    server: Secret,
}

pub struct Client {
    state: State,
    host: String,
    rng: Rng,
    roots: Vec<&'static [u8]>,
    extra_roots: Vec<Vec<u8>>,
    now: Option<u64>,
    secret_key: [u8; 32],
    session_id: Vec<u8>,
    /// Handshake messages so far (hashed when a secret needs them).
    transcript: Vec<u8>,
    /// Received bytes not yet forming a whole record.
    input: Vec<u8>,
    /// Handshake bytes not yet forming a whole message.
    pending: Vec<u8>,
    /// Negotiated in the ServerHello (meaningless before).
    suite: Suite,
    hs: Option<Handshake>,
    leaf: Vec<u8>,
    cert_request: Option<Vec<u8>>,
    recv: Option<Protection>,
    send: Option<Protection>,
    /// Application traffic secrets, for KeyUpdate.
    recv_secret: Option<Secret>,
    send_secret: Option<Secret>,
    sent_ccs: bool,
    sent_close: bool,
}

impl Client {
    /// `host`: SNI and name to verify; `rng_seed`: 32 fresh random bytes;
    /// `roots`: trusted roots (DER); `now`: Unix seconds or None (no
    /// validity dates check).
    pub fn new(
        host: &str,
        rng_seed: [u8; 32],
        roots: Vec<&'static [u8]>,
        now: Option<u64>,
    ) -> Self {
        Self {
            state: State::Idle,
            host: String::from(host),
            rng: Rng::new(rng_seed),
            roots,
            extra_roots: Vec::new(),
            now,
            secret_key: [0; 32],
            session_id: Vec::new(),
            transcript: Vec::new(),
            input: Vec::new(),
            pending: Vec::new(),
            suite: Suite::ALL[0],
            hs: None,
            leaf: Vec::new(),
            cert_request: None,
            recv: None,
            send: None,
            recv_secret: None,
            send_secret: None,
            sent_ccs: false,
            sent_close: false,
        }
    }

    /// An extra trust anchor (tests, user-installed roots).
    pub fn add_root(&mut self, der: Vec<u8>) {
        self.extra_roots.push(der);
    }

    pub fn is_connected(&self) -> bool {
        self.state == State::Connected
    }

    /// Sends the ClientHello.
    pub fn start(&mut self, out: &mut Vec<u8>) {
        let mut random = [0u8; 32];
        self.rng.fill(&mut random);
        self.rng.fill(&mut self.secret_key);
        // A legacy session id makes middleboxes take this for resumption.
        self.session_id = alloc::vec![0; 32];
        self.rng.fill(&mut self.session_id);
        let public = x25519::public_key(&self.secret_key);
        let hello = handshake::client_hello(&random, &self.session_id, &public, &self.host);
        self.transcript = hello.clone();
        record::write_plain(HANDSHAKE, 0x0301, &hello, out);
        self.state = State::WaitServerHello;
    }

    /// Processes bytes from the server. On error the connection is dead:
    /// an alert for the server may have been written to `out`.
    pub fn feed(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        if matches!(self.state, State::Idle | State::Closed) {
            return Err(Error::Protocol("connection not open"));
        }
        self.input.extend_from_slice(input);
        let r = self.records(out, events);
        if let Err(e) = r {
            if let Some(alert) = e.alert(self.state == State::Connected) {
                self.send_alert(2, alert, out);
            }
            self.state = State::Closed;
            self.input.clear();
        }
        r
    }

    /// Sends application data (ignored before [`Event::Connected`] or
    /// after [`Client::close`]).
    pub fn send(&mut self, data: &[u8], out: &mut Vec<u8>) {
        if self.state != State::Connected || self.sent_close {
            return;
        }
        for chunk in data.chunks(record::MAX_PLAINTEXT) {
            self.send_record(APPLICATION_DATA, chunk, out);
        }
    }

    /// Sends close_notify; nothing more is sent afterwards.
    pub fn close(&mut self, out: &mut Vec<u8>) {
        if self.state != State::Idle && !self.sent_close {
            self.send_alert(1, ALERT_CLOSE_NOTIFY, out);
        }
    }

    fn send_alert(&mut self, level: u8, description: u8, out: &mut Vec<u8>) {
        if self.sent_close {
            return;
        }
        self.send_record(ALERT, &[level, description], out);
        self.sent_close = true;
    }

    fn send_record(&mut self, typ: u8, data: &[u8], out: &mut Vec<u8>) {
        match self.send.as_mut() {
            Some(p) => {
                if !self.sent_ccs {
                    // Middlebox compatibility (RFC 8446 appendix D.4).
                    record::write_plain(CHANGE_CIPHER_SPEC, 0x0303, &[1], out);
                    self.sent_ccs = true;
                }
                p.seal(typ, data, out);
            }
            None => record::write_plain(typ, 0x0303, data, out),
        }
    }

    fn records(&mut self, out: &mut Vec<u8>, events: &mut Vec<Event>) -> Result<(), Error> {
        let mut pos = 0;
        while self.state != State::Closed && self.input.len() - pos >= record::HEADER_LEN {
            let header: [u8; record::HEADER_LEN] = self.input[pos..pos + record::HEADER_LEN]
                .try_into()
                .unwrap_or_default();
            let len = u16::from_be_bytes([header[3], header[4]]) as usize;
            if header[1] != 3 {
                return Err(Error::Protocol("not a TLS record"));
            }
            if len > record::MAX_CIPHERTEXT {
                return Err(Error::Protocol("record overflow"));
            }
            let end = pos + record::HEADER_LEN + len;
            if self.input.len() < end {
                break;
            }
            let mut body = self.input[pos + record::HEADER_LEN..end].to_vec();
            pos = end;
            self.record(&header, &mut body, out, events)?;
        }
        self.input.drain(..pos);
        Ok(())
    }

    fn record(
        &mut self,
        header: &[u8; record::HEADER_LEN],
        body: &mut [u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let mut typ = header[0];
        let mut len = body.len();
        if typ == CHANGE_CIPHER_SPEC {
            // Accepted (and ignored) only during the handshake.
            let handshaking = !matches!(self.state, State::Connected | State::Closed);
            if handshaking && body == [1] {
                return Ok(());
            }
            return Err(Error::Protocol("unexpected ChangeCipherSpec"));
        }
        if let Some(recv) = self.recv.as_mut() {
            if typ != APPLICATION_DATA {
                return Err(Error::Protocol("unprotected record after the ServerHello"));
            }
            (typ, len) = recv.open(header, body)?;
        } else if len > record::MAX_PLAINTEXT {
            return Err(Error::Protocol("record overflow"));
        }
        let data = &body[..len];
        if typ != HANDSHAKE && !self.pending.is_empty() {
            return Err(Error::Protocol("handshake message interrupted"));
        }
        match typ {
            HANDSHAKE => {
                if data.is_empty() {
                    return Err(Error::Protocol("empty handshake record"));
                }
                self.pending.extend_from_slice(data);
                self.messages(out, events)
            }
            ALERT => self.alert(data, out, events),
            APPLICATION_DATA if self.state == State::Connected => {
                if !data.is_empty() {
                    events.push(Event::Data(data.to_vec()));
                }
                Ok(())
            }
            _ => Err(Error::Protocol("unexpected record type")),
        }
    }

    fn alert(
        &mut self,
        data: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let &[_level, description] = data else {
            return Err(Error::Protocol("malformed alert"));
        };
        match description {
            ALERT_CLOSE_NOTIFY => {
                self.send_alert(1, ALERT_CLOSE_NOTIFY, out);
                self.state = State::Closed;
                events.push(Event::Closed);
                Ok(())
            }
            ALERT_USER_CANCELED => Ok(()),
            d => Err(Error::Alert(d)),
        }
    }

    fn messages(&mut self, out: &mut Vec<u8>, events: &mut Vec<Event>) -> Result<(), Error> {
        while self.pending.len() >= 4 {
            let len = (self.pending[1] as usize) << 16
                | (self.pending[2] as usize) << 8
                | self.pending[3] as usize;
            if len > MAX_HANDSHAKE {
                return Err(Error::Protocol("handshake message too large"));
            }
            if self.pending.len() < 4 + len {
                break;
            }
            let msg: Vec<u8> = self.pending.drain(..4 + len).collect();
            let key_change = self.message(&msg, out, events)?;
            // Messages before a key change must end on a record boundary.
            if key_change && !self.pending.is_empty() {
                return Err(Error::Protocol(
                    "data after a key change in the same record",
                ));
            }
            if self.state == State::Closed {
                break;
            }
        }
        Ok(())
    }

    /// Handles one handshake message; true if the receive keys changed.
    fn message(
        &mut self,
        msg: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<bool, Error> {
        let typ = msg[0];
        let body = &msg[4..];
        match (self.state, typ) {
            (State::WaitServerHello, handshake::SERVER_HELLO) => {
                let sh = handshake::server_hello(body, &self.session_id)?;
                let shared = x25519::shared(&self.secret_key, &sh.key_share)
                    .ok_or(Error::Protocol("bad x25519 key share"))?;
                self.transcript.extend_from_slice(msg);
                let (schedule, secrets) = Schedule::new(sh.suite, &shared, &self.transcript);
                self.recv = Some(Protection::new(sh.suite, &secrets.server));
                self.send = Some(Protection::new(sh.suite, &secrets.client));
                self.suite = sh.suite;
                self.hs = Some(Handshake {
                    schedule,
                    client: secrets.client,
                    server: secrets.server,
                });
                self.state = State::WaitEncryptedExtensions;
                Ok(true)
            }
            (State::WaitEncryptedExtensions, handshake::ENCRYPTED_EXTENSIONS) => {
                handshake::encrypted_extensions(body)?;
                self.transcript.extend_from_slice(msg);
                self.state = State::WaitCertificate;
                Ok(false)
            }
            (State::WaitCertificate, handshake::CERTIFICATE_REQUEST)
                if self.cert_request.is_none() =>
            {
                self.cert_request = Some(handshake::certificate_request(body)?);
                self.transcript.extend_from_slice(msg);
                Ok(false)
            }
            (State::WaitCertificate, handshake::CERTIFICATE) => {
                let chain = handshake::certificate(body)?;
                let mut roots: Vec<&[u8]> = self.roots.clone();
                roots.extend(self.extra_roots.iter().map(Vec::as_slice));
                aricode_x509::verify_chain(&chain, &roots, &self.host, self.now)
                    .map_err(Error::Certificate)?;
                self.leaf = chain[0].to_vec();
                self.transcript.extend_from_slice(msg);
                self.state = State::WaitCertificateVerify;
                Ok(false)
            }
            (State::WaitCertificateVerify, handshake::CERTIFICATE_VERIFY) => {
                let hash = keys::transcript_hash(self.suite, &self.transcript);
                handshake::certificate_verify(body, &self.leaf, hash.as_bytes())?;
                self.transcript.extend_from_slice(msg);
                self.state = State::WaitFinished;
                Ok(false)
            }
            (State::WaitFinished, handshake::FINISHED) => {
                self.finished(msg, out, events)?;
                Ok(true)
            }
            (State::Connected, handshake::NEW_SESSION_TICKET) => {
                handshake::new_session_ticket(body)?;
                Ok(false)
            }
            (State::Connected, handshake::KEY_UPDATE) => {
                let suite = self.suite;
                let requested = handshake::key_update(body)?;
                if let Some(secret) = self.recv_secret.as_mut() {
                    *secret = keys::next_traffic_secret(suite, secret);
                    self.recv = Some(Protection::new(suite, secret));
                }
                if requested && !self.sent_close {
                    self.send_record(
                        HANDSHAKE,
                        &handshake::message(handshake::KEY_UPDATE, &[0]),
                        out,
                    );
                    if let Some(secret) = self.send_secret.as_mut() {
                        *secret = keys::next_traffic_secret(suite, secret);
                        self.send = Some(Protection::new(suite, secret));
                    }
                }
                Ok(true)
            }
            _ => Err(Error::Protocol("unexpected handshake message")),
        }
    }

    /// Server Finished: verify it, answer with our (empty Certificate and)
    /// Finished, and switch to the application keys.
    fn finished(
        &mut self,
        msg: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        let hs = self.hs.take().ok_or(Error::Protocol("no handshake keys"))?;
        let suite = self.suite;
        let expected = keys::finished(suite, &hs.server, &self.transcript);
        if !ct_eq(expected.as_bytes(), &msg[4..]) {
            return Err(Error::Protocol("bad server Finished"));
        }
        self.transcript.extend_from_slice(msg);
        let app = hs.schedule.application(&self.transcript);
        self.recv = Some(Protection::new(suite, &app.server));
        if let Some(context) = self.cert_request.take() {
            let cert = handshake::empty_certificate(&context);
            self.send_record(HANDSHAKE, &cert, out);
            self.transcript.extend_from_slice(&cert);
        }
        let verify = keys::finished(suite, &hs.client, &self.transcript);
        let ours = handshake::message(handshake::FINISHED, verify.as_bytes());
        self.send_record(HANDSHAKE, &ours, out);
        self.send = Some(Protection::new(suite, &app.client));
        self.recv_secret = Some(app.server);
        self.send_secret = Some(app.client);
        self.transcript = Vec::new();
        self.leaf = Vec::new();
        self.state = State::Connected;
        events.push(Event::Connected);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use rfc8448::*;

    /// The server's first flight: ServerHello record + encrypted record.
    fn server_flight() -> Vec<u8> {
        let mut f = vec![0x16, 3, 3, 0, 0x5a];
        f.extend_from_slice(&h(SERVER_HELLO));
        f.extend_from_slice(&h(SERVER_RECORD));
        f
    }

    /// The trace's self-signed RSA certificate (CN "rsa"), from the
    /// Certificate message.
    fn rfc_certificate() -> Vec<u8> {
        h(SERVER_FLIGHT)[51..51 + 0x1b0].to_vec()
    }

    /// A client that sent the trace's ClientHello: its key, session id and
    /// transcript replaced after `start`.
    fn rfc_client(host: &str, trust: bool) -> Client {
        let mut c = Client::new(host, [0x42; 32], Vec::new(), None);
        let mut out = Vec::new();
        c.start(&mut out);
        assert_eq!(
            &out[..5],
            &[
                0x16,
                3,
                1,
                ((out.len() - 5) >> 8) as u8,
                (out.len() - 5) as u8
            ]
        );
        c.secret_key = h(CLIENT_PRIVATE).try_into().unwrap();
        c.session_id = Vec::new();
        c.transcript = h(CLIENT_HELLO);
        if trust {
            c.add_root(rfc_certificate());
        }
        c
    }

    fn feed(c: &mut Client, data: &[u8]) -> (Result<(), Error>, Vec<u8>, Vec<Event>) {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let r = c.feed(data, &mut out, &mut events);
        (r, out, events)
    }

    const CCS: [u8; 6] = [20, 3, 3, 0, 1, 1];

    #[test]
    fn rfc8448_handshake_data_and_close() {
        let mut c = rfc_client("rsa", true);
        let (r, out, events) = feed(&mut c, &server_flight());
        assert_eq!(r, Ok(()));
        assert_eq!(events, [Event::Connected]);
        assert!(c.is_connected());
        assert_eq!(out, [&CCS[..], &h(CLIENT_FINISHED_RECORD)].concat());

        let (r, out, events) = feed(&mut c, &h(NEW_SESSION_TICKET_RECORD));
        assert_eq!((r, out.len(), events.len()), (Ok(()), 0, 0));

        let data: Vec<u8> = (0..50).collect();
        let mut out = Vec::new();
        c.send(&data, &mut out);
        assert_eq!(out, h(CLIENT_DATA_RECORD));

        let (r, _, events) = feed(&mut c, &h(SERVER_DATA_RECORD));
        assert_eq!(r, Ok(()));
        assert_eq!(events, [Event::Data(data)]);

        let mut out = Vec::new();
        c.close(&mut out);
        assert_eq!(out, h(CLIENT_CLOSE_RECORD));
        let (r, out, events) = feed(&mut c, &h(SERVER_CLOSE_RECORD));
        assert_eq!((r, out.len()), (Ok(()), 0));
        assert_eq!(events, [Event::Closed]);
        assert!(!c.is_connected());
        assert!(feed(&mut c, &[]).0.is_err());
    }

    #[test]
    fn fragmented_coalesced_and_ccs() {
        // Everything at once, with a middlebox CCS after the ServerHello.
        let mut all = server_flight();
        all.splice(95..95, CCS);
        all.extend_from_slice(&h(NEW_SESSION_TICKET_RECORD));
        all.extend_from_slice(&h(SERVER_DATA_RECORD));
        let mut c = rfc_client("rsa", true);
        let (r, _, events) = feed(&mut c, &all);
        assert_eq!(r, Ok(()));
        assert_eq!(events, [Event::Connected, Event::Data((0..50).collect())]);

        // One byte at a time.
        let mut c = rfc_client("rsa", true);
        let mut events = Vec::new();
        let mut out = Vec::new();
        for b in &all {
            c.feed(&[*b], &mut out, &mut events).unwrap();
        }
        assert_eq!(events, [Event::Connected, Event::Data((0..50).collect())]);
        assert_eq!(out, [&CCS[..], &h(CLIENT_FINISHED_RECORD)].concat());

        // A CCS after the handshake is a protocol error.
        let (r, _, _) = feed(&mut c, &CCS);
        assert!(matches!(r, Err(Error::Protocol(_))));
    }

    #[test]
    fn tampering_and_untrusted_certificates() {
        let mut flight = server_flight();
        let n = flight.len();
        flight[n - 20] ^= 0x80;
        let mut c = rfc_client("rsa", true);
        let (r, out, _) = feed(&mut c, &flight);
        assert_eq!(r, Err(Error::Decrypt));
        // CCS + encrypted alert (2 bytes + type + tag).
        assert_eq!(out.len(), 6 + 5 + 3 + 16);
        assert!(feed(&mut c, &[]).0.is_err());

        let mut c = rfc_client("rsa", false);
        let (r, _, _) = feed(&mut c, &server_flight());
        assert_eq!(
            r,
            Err(Error::Certificate(aricode_x509::Error::UnknownIssuer))
        );

        let mut c = rfc_client("server", true);
        let (r, _, events) = feed(&mut c, &server_flight());
        assert_eq!(
            r,
            Err(Error::Certificate(aricode_x509::Error::NameMismatch))
        );
        assert!(events.is_empty());
    }

    #[test]
    fn server_alert_and_garbage() {
        let mut c = rfc_client("rsa", true);
        let (r, out, _) = feed(&mut c, &[21, 3, 3, 0, 2, 2, 40]);
        assert_eq!(r, Err(Error::Alert(40)));
        assert!(out.is_empty(), "no alert in answer to an alert");

        let mut c = rfc_client("rsa", true);
        let (r, out, _) = feed(&mut c, b"HTTP/1.1 400 Bad Request\r\n");
        assert!(matches!(r, Err(Error::Protocol(_))));
        // Plaintext handshake_failure alert.
        assert_eq!(out, [21, 3, 3, 0, 2, 2, 40]);

        // Application data before the handshake.
        let mut c = rfc_client("rsa", true);
        assert!(feed(&mut c, &[23, 3, 3, 0, 1, 0]).0.is_err());
    }

    #[test]
    fn key_update_both_ways() {
        let mut c = rfc_client("rsa", true);
        assert_eq!(feed(&mut c, &server_flight()).2, [Event::Connected]);
        let suite = c.suite;
        let server_secret = c.recv_secret.clone().unwrap();
        let client_secret = c.send_secret.clone().unwrap();

        // The server updates its keys and asks us to update ours.
        let mut server_send = Protection::new(suite, &server_secret);
        let mut rec = Vec::new();
        server_send.seal(
            HANDSHAKE,
            &handshake::message(handshake::KEY_UPDATE, &[1]),
            &mut rec,
        );
        let next_server = keys::next_traffic_secret(suite, &server_secret);
        let mut server_send = Protection::new(suite, &next_server);
        server_send.seal(APPLICATION_DATA, b"after", &mut rec);
        let (r, out, events) = feed(&mut c, &rec);
        assert_eq!(r, Ok(()));
        assert_eq!(events, [Event::Data(b"after".to_vec())]);

        // Our KeyUpdate(update_not_requested) goes with the old keys...
        let mut server_recv = Protection::new(suite, &client_secret);
        let mut body = out[5..].to_vec();
        let (typ, len) = server_recv
            .open(out[..5].try_into().unwrap(), &mut body)
            .unwrap();
        assert_eq!((typ, &body[..len]), (HANDSHAKE, &[24, 0, 0, 1, 0][..]));
        // ...and what follows with the new ones.
        let mut out = Vec::new();
        c.send(b"x", &mut out);
        let mut server_recv =
            Protection::new(suite, &keys::next_traffic_secret(suite, &client_secret));
        let mut body = out[5..].to_vec();
        let (typ, len) = server_recv
            .open(out[..5].try_into().unwrap(), &mut body)
            .unwrap();
        assert_eq!((typ, &body[..len]), (APPLICATION_DATA, &b"x"[..]));
    }
}
