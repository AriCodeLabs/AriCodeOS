// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! TLS client (ADR 0067): rustls driven through its unbuffered API, on
//! `aricode-crypto` ([`provider`]) and `rustls-webpki` with this system's
//! certificate policy ([`verify`]). Bytes from the server go in through
//! [`Client::feed`]; bytes for the server and [`Event`]s come out. No I/O
//! and no threads, so it runs the same in AriCodeOS and in host tests.
//!
//! TLS 1.3 and TLS 1.2 (rustls's subset: ECDHE with AEADs, extended master
//! secret); key exchange X25519, P-256 or P-384; AES-GCM and
//! ChaCha20-Poly1305; server certificates verified against the given roots.
//! No client certificates (rustls answers a request with none), no session
//! resumption across connections, no early data.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod provider;
pub mod verify;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::ClientConfig;

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
    /// The certificate chain was refused.
    Certificate(aricode_x509::Error),
    /// The server and this client share no version, suite or group.
    Unsupported(&'static str),
    /// A record failed authentication.
    Decrypt,
}

/// The time rustls is told it is: the policy's moment (the clock, or the
/// floor without a trusted one). Certificates are checked by
/// [`verify::Verifier`] with the full policy.
#[derive(Debug)]
struct PolicyTime(u64);

impl rustls::time_provider::TimeProvider for PolicyTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            self.0,
        )))
    }
}

/// Bytes of server records held while rustls waits for the rest of a
/// handshake message: the largest message (2^16 - 1 bytes) plus one whole
/// record. rustls keeps every fragment of a message in the caller's
/// buffer, headers and padding included, so without a limit a server
/// could fill the program's heap with tiny fragments before it is
/// authenticated (audit of ADR 0067, M1).
pub const MAX_INCOMING: usize = 0xFFFF + 18_437;

/// The default trust anchors (Mozilla's, `aricode_x509::roots`), parsed
/// once per program. The certificates stay where the binary holds them:
/// only this small table of references is allocated, once, and kept.
pub fn mozilla_roots() -> &'static [CertificateDer<'static>] {
    static ROOTS: aricode_sync::SpinLock<Option<&'static [CertificateDer<'static>]>> =
        aricode_sync::SpinLock::new(None);
    let mut roots = ROOTS.lock();
    roots.get_or_insert_with(|| {
        Box::leak(
            aricode_x509::roots::default()
                .map(CertificateDer::from_slice)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    })
}

/// A TLS connection to one server.
pub struct Client {
    conn: Option<UnbufferedClientConnection>,
    verifier: Arc<verify::Verifier>,
    /// Bytes from the server not yet consumed by rustls.
    incoming: Vec<u8>,
    /// Events produced outside [`Client::feed`], handed out by the next one.
    pending: Vec<Event>,
    connected: bool,
    closed: bool,
    /// Why the connection died outside [`Client::feed`], for the next one.
    failed: Option<Error>,
}

impl Client {
    /// `host`: SNI and name to verify (a DNS name or an IP address);
    /// `rng_seed`: 32 fresh random bytes, mixed into the generator;
    /// `roots`: trusted roots ([`mozilla_roots`] by default; ones that do
    /// not parse are ignored); `extra`: more roots the caller owns (tests,
    /// user-installed); `now`: what validity dates are checked against
    /// (`aricode_x509::check_time`).
    pub fn new(
        host: &str,
        rng_seed: [u8; 32],
        roots: &'static [CertificateDer<'static>],
        extra: &[Vec<u8>],
        now: aricode_x509::ValidityTime,
    ) -> Self {
        let mut seed = aricode_crypto::SecretBytes(rng_seed);
        provider::reseed(&seed.0);
        aricode_crypto::wipe(&mut seed.0);
        // Anchors borrow the static roots; only `extra` is copied.
        let mut anchors: Vec<_> = roots
            .iter()
            .filter_map(|der| webpki::anchor_from_trusted_cert(der).ok())
            .collect();
        anchors.extend(extra.iter().filter_map(|der| {
            webpki::anchor_from_trusted_cert(&CertificateDer::from(der.as_slice()))
                .ok()
                .map(|a| a.to_owned())
        }));
        let at = match now {
            aricode_x509::ValidityTime::Clock(t) | aricode_x509::ValidityTime::AtLeast(t) => t,
        };
        let verifier = verify::Verifier::new(anchors, now);
        let conn = ServerName::try_from(host).ok().and_then(|name| {
            let mut config = ClientConfig::builder_with_details(
                Arc::new(provider::provider()),
                Arc::new(PolicyTime(at)),
            )
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
            // TLS 1.2 only with the extended master secret (RFC 7627).
            config.require_ems = true;
            UnbufferedClientConnection::new(Arc::new(config), name.to_owned()).ok()
        });
        let failed = conn
            .is_none()
            .then_some(Error::Protocol("invalid server name"));
        Self {
            conn,
            verifier,
            incoming: Vec::new(),
            pending: Vec::new(),
            connected: false,
            closed: failed.is_some(),
            failed,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected && !self.closed
    }

    /// Writes the ClientHello into `out`.
    pub fn start(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let mut events = Vec::new();
        let r = self.drive(out, &mut events, None);
        self.pending.extend(events);
        r.inspect_err(|e| self.fail(*e))
    }

    /// Processes bytes from the server. On error the connection is dead:
    /// an alert for the server may have been written to `out`.
    pub fn feed(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        events.append(&mut self.pending);
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.closed {
            return Err(Error::Protocol("connection not open"));
        }
        if self.incoming.len() + input.len() > MAX_INCOMING {
            let e = Error::Protocol("handshake message spread over too many bytes");
            self.fail(e);
            return Err(e);
        }
        self.incoming.extend_from_slice(input);
        self.drive(out, events, None).inspect_err(|e| self.fail(*e))
    }

    /// Sends application data (ignored before [`Event::Connected`] or
    /// after [`Client::close`]).
    pub fn send(&mut self, data: &[u8], out: &mut Vec<u8>) {
        if !self.is_connected() {
            return;
        }
        let mut events = Vec::new();
        if let Err(e) = self.drive(out, &mut events, Some(Write::Data(data))) {
            self.fail(e);
        }
        self.pending.extend(events);
    }

    /// Sends close_notify; nothing more is sent afterwards.
    pub fn close(&mut self, out: &mut Vec<u8>) {
        if self.connected && !self.closed {
            let mut events = Vec::new();
            let _ = self.drive(out, &mut events, Some(Write::Close));
            self.pending.extend(events);
        }
        self.closed = true;
    }

    /// Marks the connection dead with `e`, reported by the next call.
    fn fail(&mut self, e: Error) {
        self.closed = true;
        self.failed.get_or_insert(e);
        self.incoming.clear();
    }

    /// Runs rustls over the bytes received so far until it needs more
    /// input or, with `write`, until that is encrypted into `out`.
    fn drive(
        &mut self,
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
        mut write: Option<Write<'_>>,
    ) -> Result<(), Error> {
        let Some(conn) = self.conn.as_mut() else {
            self.closed = true;
            return Err(Error::Protocol("invalid server name"));
        };
        loop {
            let UnbufferedStatus { mut discard, state } =
                conn.process_tls_records(&mut self.incoming);
            let state = match state {
                Ok(s) => s,
                Err(e) => {
                    let err = map_error(&self.verifier, &e);
                    // rustls queued a fatal alert: send it.
                    if let Ok(ConnectionState::EncodeTlsData(mut enc)) =
                        conn.process_tls_records(&mut []).state
                    {
                        let _ = encode(|b| enc.encode(b).map_err(Grow::from), out);
                    }
                    self.closed = true;
                    self.incoming.clear();
                    return Err(err);
                }
            };
            let mut done = false;
            match state {
                ConnectionState::ReadTraffic(mut traffic) => {
                    while let Some(record) = traffic.next_record() {
                        let record = record.map_err(|e| map_error(&self.verifier, &e))?;
                        discard += record.discard;
                        events.push(Event::Data(record.payload.to_vec()));
                    }
                }
                ConnectionState::EncodeTlsData(mut enc) => {
                    if encode(|b| enc.encode(b).map_err(Grow::from), out).is_err() {
                        self.closed = true;
                        return Err(Error::Protocol("handshake record too large"));
                    }
                }
                // `out` is sent by the caller after this returns, in order.
                ConnectionState::TransmitTlsData(t) => t.done(),
                ConnectionState::BlockedHandshake => done = true,
                ConnectionState::WriteTraffic(mut traffic) => {
                    if !self.connected {
                        self.connected = true;
                        events.push(Event::Connected);
                    }
                    match write.take() {
                        Some(Write::Data(data)) => {
                            encode(|b| traffic.encrypt(data, b).map_err(Grow::from), out)
                                .map_err(|_| Error::Protocol("record too large"))?;
                        }
                        Some(Write::Close) => {
                            encode(|b| traffic.queue_close_notify(b).map_err(Grow::from), out)
                                .map_err(|_| Error::Protocol("record too large"))?;
                        }
                        None => {}
                    }
                    done = true;
                }
                ConnectionState::PeerClosed => events.push(Event::Closed),
                ConnectionState::Closed => {
                    self.closed = true;
                    done = true;
                }
                _ => done = true,
            }
            self.incoming.drain(..discard);
            if done {
                return Ok(());
            }
        }
    }
}

/// rustls's error in this crate's terms, the certificate verifier's own
/// reason first.
fn map_error(verifier: &verify::Verifier, e: &rustls::Error) -> Error {
    use rustls::Error as R;
    if let Some(why) = verifier.refusal() {
        return Error::Certificate(why);
    }
    match e {
        R::AlertReceived(a) => Error::Alert(u8::from(*a)),
        R::DecryptError => Error::Decrypt,
        R::InvalidCertificate(c) => Error::Certificate(certificate_error(c)),
        R::PeerIncompatible(_) => Error::Unsupported("no common TLS parameters"),
        R::NoCertificatesPresented => Error::Protocol("no certificate"),
        R::PeerMisbehaved(_) => Error::Protocol("the server misbehaved"),
        R::InappropriateMessage { .. } | R::InappropriateHandshakeMessage { .. } => {
            Error::Protocol("unexpected message")
        }
        R::InvalidMessage(_) => Error::Protocol("invalid message"),
        R::PeerSentOversizedRecord => Error::Protocol("oversized record"),
        _ => Error::Protocol("TLS failure"),
    }
}

/// rustls's certificate errors that do not come from the chain verifier
/// (a bad CertificateVerify or ServerKeyExchange signature). Its deprecated
/// variant is still matched: older paths may return it.
#[allow(deprecated)]
fn certificate_error(c: &rustls::CertificateError) -> aricode_x509::Error {
    use aricode_x509::Error as X;
    use rustls::CertificateError as C;
    match c {
        C::BadSignature => X::BadSignature,
        C::Expired | C::ExpiredContext { .. } => X::Expired,
        C::NotValidYet | C::NotValidYetContext { .. } => X::NotYetValid,
        C::UnknownIssuer => X::UnknownIssuer,
        C::NotValidForName | C::NotValidForNameContext { .. } => X::NameMismatch,
        C::InvalidPurpose | C::InvalidPurposeContext { .. } => X::WrongPurpose,
        C::UnsupportedSignatureAlgorithm | C::UnsupportedSignatureAlgorithmContext { .. } => {
            X::Unsupported
        }
        _ => X::Malformed,
    }
}

/// What [`Client::drive`] encrypts once the connection allows it.
enum Write<'a> {
    Data(&'a [u8]),
    Close,
}

/// A buffer rustls found too small, and the size it needs.
struct Grow(usize);

impl From<EncodeError> for Grow {
    fn from(e: EncodeError) -> Self {
        match e {
            EncodeError::InsufficientSize(s) => Grow(s.required_size),
            _ => Grow(0),
        }
    }
}

impl From<EncryptError> for Grow {
    fn from(e: EncryptError) -> Self {
        match e {
            EncryptError::InsufficientSize(s) => Grow(s.required_size),
            _ => Grow(0),
        }
    }
}

/// Calls `f` with a buffer large enough for what it writes, and appends
/// the written bytes to `out`.
fn encode(
    mut f: impl FnMut(&mut [u8]) -> Result<usize, Grow>,
    out: &mut Vec<u8>,
) -> Result<(), ()> {
    let mut buf = vec![0u8; 2048];
    loop {
        match f(&mut buf) {
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                return Ok(());
            }
            Err(Grow(need)) if need > buf.len() => buf.resize(need, 0),
            Err(_) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests;
