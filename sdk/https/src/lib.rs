// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! HTTP and HTTPS GET for AriCodeOS programs. `http://` goes straight over
//! the TCP service; `https://` runs TLS 1.3 (`aricode-tls`) on top, with
//! the certificate checked against the embedded roots
//! (`aricode_x509::roots::DEFAULT`), the host name and, if the kernel has
//! a real-time clock, the validity dates. Needs `alloc` (the TLS client
//! buffers records), so it lives outside `aricode-sys`.
//!
//! Every download has two time limits (incident 0009): `timeout_ms`, the
//! most one network operation may wait, and a [`Budget`] for the whole
//! exchange, consulted before every resolve, connect, TLS record, send and
//! receive. Each operation gets the smaller of the two, so a server that
//! keeps sending something small before each timeout cannot hold the
//! caller past its budget.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
pub use aricode_http::{Budget, Fetched, Phase, Unbounded};
use aricode_http::{Error, Transport, Url};
use aricode_sys::net::{NetClient, NetError};
use aricode_sys::{self as sys, Handle};
use aricode_tls::{Client, Event};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpsError {
    BadUrl,
    Resolve(NetError),
    Connect(NetError),
    Io(NetError),
    /// TLS handshake or record failure (certificate errors included).
    Tls(aricode_tls::Error),
    /// The response is not HTTP, is truncated or has invalid framing.
    Malformed,
    /// The response headers do not fit (a body that does not fit is
    /// truncated: see `Fetched::truncated`).
    TooBig,
    /// Unsupported encoding or protocol switch.
    Unsupported,
    /// The caller's sink refused more data (`get_with`).
    Aborted,
    /// The caller's budget ran out in this phase (incident 0009).
    Deadline(Phase),
    /// More than `aricode_http::MAX_INFORMATIONAL` interim responses.
    TooManyInformational,
    /// Headers of the exchange, interim responses included, exceed
    /// `aricode_http::MAX_HEADER_BYTES`.
    HeaderBytes,
}

#[derive(Debug, Clone, Copy)]
enum StreamError {
    Net(NetError),
    Tls(aricode_tls::Error),
}

/// The time one network operation may take: what is left of the budget,
/// never more than the per-operation cap. `0` once the budget is spent:
/// netd then waits one millisecond at most, and the caller reports the
/// deadline (`aricode_http::fetch_with` does so for every operation it
/// drives; `get_with` for the ones before it).
fn op_timeout(budget: &dyn Budget, cap_ms: u32) -> u32 {
    budget.remaining_ms().min(cap_ms)
}

/// A TCP connection, optionally wrapped in TLS, as an HTTP transport.
struct Stream<'a> {
    net: &'a mut NetClient,
    sock: u16,
    budget: &'a dyn Budget,
    cap_ms: u32,
    tls: Option<Tls>,
}

struct Tls {
    client: Client,
    plain: Vec<u8>,
    closed: bool,
}

impl Stream<'_> {
    fn timeout(&self) -> u32 {
        op_timeout(self.budget, self.cap_ms)
    }

    fn send_raw(&mut self, data: &[u8]) -> Result<(), StreamError> {
        if data.is_empty() {
            return Ok(());
        }
        let timeout = self.timeout();
        self.net
            .tcp_send_all_within(self.sock, data, timeout)
            .map_err(StreamError::Net)
    }

    /// Reads from TCP and feeds the TLS client; returns false at EOF.
    fn pump(&mut self) -> Result<bool, StreamError> {
        let mut buf = [0u8; 1400];
        let timeout = self.timeout();
        let n = self
            .net
            .tcp_recv(self.sock, timeout, &mut buf)
            .map_err(StreamError::Net)?;
        if n == 0 {
            return Ok(false);
        }
        let tls = self.tls.as_mut().expect("TLS stream");
        let mut out = Vec::new();
        let mut events = Vec::new();
        let r = tls.client.feed(&buf[..n], &mut out, &mut events);
        for e in events {
            match e {
                Event::Data(d) => tls.plain.extend_from_slice(&d),
                Event::Closed => tls.closed = true,
                Event::Connected => {}
            }
        }
        self.send_raw(&out)?;
        r.map_err(StreamError::Tls)?;
        Ok(true)
    }

    /// TLS handshake within the budget: every record exchange is bounded,
    /// and a server that answers slowly, record by record, ends in
    /// `Deadline(Handshake)` rather than holding the caller.
    fn handshake(&mut self) -> Result<(), HttpsError> {
        let mut out = Vec::new();
        self.tls
            .as_mut()
            .expect("TLS stream")
            .client
            .start(&mut out);
        self.send_raw(&out).map_err(|e| self.failure(e))?;
        while !self.tls.as_ref().expect("TLS stream").client.is_connected() {
            if self.budget.remaining_ms() == 0 {
                return Err(HttpsError::Deadline(Phase::Handshake));
            }
            match self.pump() {
                Ok(true) => {}
                Ok(false) => {
                    return Err(HttpsError::Tls(aricode_tls::Error::Protocol(
                        "connection closed during the handshake",
                    )))
                }
                Err(e) => return Err(self.failure(e)),
            }
        }
        Ok(())
    }

    /// A stream failure during the handshake: once the budget is spent it
    /// is the deadline (the operation was given the time that was left).
    fn failure(&self, e: StreamError) -> HttpsError {
        if self.budget.remaining_ms() == 0 {
            return HttpsError::Deadline(Phase::Handshake);
        }
        match e {
            StreamError::Net(e) => HttpsError::Io(e),
            StreamError::Tls(e) => HttpsError::Tls(e),
        }
    }
}

impl Transport for Stream<'_> {
    type Error = StreamError;
    fn send_all(&mut self, data: &[u8]) -> Result<(), StreamError> {
        match self.tls.as_mut() {
            None => self.send_raw(data),
            Some(tls) => {
                let mut out = Vec::new();
                tls.client.send(data, &mut out);
                self.send_raw(&out)
            }
        }
    }
    fn recv(&mut self, out: &mut [u8]) -> Result<usize, StreamError> {
        if self.tls.is_none() {
            let timeout = self.timeout();
            return self
                .net
                .tcp_recv(self.sock, timeout, out)
                .map_err(StreamError::Net);
        }
        loop {
            let tls = self.tls.as_mut().expect("TLS stream");
            if !tls.plain.is_empty() {
                let n = out.len().min(tls.plain.len());
                out[..n].copy_from_slice(&tls.plain[..n]);
                tls.plain.drain(..n);
                return Ok(n);
            }
            if tls.closed || !self.pump()? {
                return Ok(0);
            }
        }
    }
}

/// Downloads `url` (`http://` or `https://`, literal IP or name resolved by
/// netd) into `out`. `system` (READ) provides randomness and the clock for
/// TLS; `extra_roots` are trust anchors (DER) added to the embedded ones,
/// e.g. a private CA. `timeout_ms` bounds one network operation, `budget`
/// the whole download. A body larger than `out` is truncated.
pub fn get(
    net: &mut NetClient,
    system: Handle,
    extra_roots: &[Vec<u8>],
    url: &str,
    out: &mut [u8],
    timeout_ms: u32,
    budget: &dyn Budget,
) -> Result<Fetched, HttpsError> {
    let mut body_len = 0;
    let mut truncated = false;
    let mut sink = |chunk: &[u8]| {
        let n = chunk.len().min(out.len() - body_len);
        out[body_len..body_len + n].copy_from_slice(&chunk[..n]);
        body_len += n;
        truncated |= n < chunk.len();
        true
    };
    let status = get_with(net, system, extra_roots, url, timeout_ms, budget, &mut sink)?;
    Ok(Fetched {
        status,
        body_len,
        truncated,
    })
}

/// Streaming download: the body goes to `sink` piece by piece (any size,
/// in order), which returns `false` to abort. Returns the status code.
/// `timeout_ms` bounds one network operation, `budget` the whole exchange
/// from the name resolution to the last body byte (`Deadline`).
pub fn get_with(
    net: &mut NetClient,
    system: Handle,
    extra_roots: &[Vec<u8>],
    url: &str,
    timeout_ms: u32,
    budget: &dyn Budget,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<u16, HttpsError> {
    let url = Url::parse(url).ok_or(HttpsError::BadUrl)?;
    let addr = match aricode_net::Ipv4Addr::parse(url.host) {
        Some(ip) => ip.0,
        None => {
            if budget.remaining_ms() == 0 {
                return Err(HttpsError::Deadline(Phase::Resolve));
            }
            net.resolve(url.host, None, op_timeout(budget, timeout_ms))
                .map_err(|e| before_http(budget, Phase::Resolve, HttpsError::Resolve(e)))?
        }
    };
    if budget.remaining_ms() == 0 {
        return Err(HttpsError::Deadline(Phase::Connect));
    }
    let sock = net
        .tcp_connect(addr, url.port, op_timeout(budget, timeout_ms))
        .map_err(|e| before_http(budget, Phase::Connect, HttpsError::Connect(e)))?;
    let tls = url.secure.then(|| {
        let mut seed = [0u8; 32];
        let _ = sys::random(system, &mut seed);
        let now = sys::time::unix_seconds(system);
        let mut client = Client::new(url.host, seed, aricode_x509::roots::DEFAULT.to_vec(), now);
        for root in extra_roots {
            client.add_root(root.clone());
        }
        Tls {
            client,
            plain: Vec::new(),
            closed: false,
        }
    });
    let mut stream = Stream {
        net,
        sock,
        budget,
        cap_ms: timeout_ms,
        tls,
    };
    let result = match stream.tls.is_some() {
        true => stream.handshake(),
        false => Ok(()),
    }
    .and_then(|()| {
        aricode_http::fetch_with(&mut stream, &url, budget, sink).map_err(|e| match e {
            Error::BadUrl => HttpsError::BadUrl,
            Error::Transport(StreamError::Net(e)) => HttpsError::Io(e),
            Error::Transport(StreamError::Tls(e)) => HttpsError::Tls(e),
            Error::Malformed => HttpsError::Malformed,
            Error::TooBig => HttpsError::TooBig,
            Error::Unsupported => HttpsError::Unsupported,
            Error::Aborted => HttpsError::Aborted,
            Error::Deadline(phase) => HttpsError::Deadline(phase),
            Error::TooManyInformational => HttpsError::TooManyInformational,
            Error::HeaderBytes => HttpsError::HeaderBytes,
        })
    });
    if let Some(tls) = stream.tls.as_mut() {
        let mut bye = Vec::new();
        tls.client.close(&mut bye);
        let _ = stream.send_raw(&bye);
    }
    let _ = stream.net.tcp_close(sock);
    result
}

/// A failure before the HTTP exchange once the budget is spent is the
/// deadline: the operation was given the time that was left.
fn before_http(budget: &dyn Budget, phase: Phase, e: HttpsError) -> HttpsError {
    if budget.remaining_ms() == 0 {
        HttpsError::Deadline(phase)
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    /// Every operation gets what is left of the budget, never more than the
    /// per-operation cap, and nothing once the budget is spent.
    #[test]
    fn operation_timeout_follows_the_budget() {
        let left = Cell::new(60_000u32);
        let budget = || left.get();
        assert_eq!(
            op_timeout(&budget, 15_000),
            15_000,
            "the cap when time abounds"
        );
        left.set(4_000);
        assert_eq!(
            op_timeout(&budget, 15_000),
            4_000,
            "the remainder near the end"
        );
        left.set(0);
        assert_eq!(op_timeout(&budget, 15_000), 0, "nothing past the deadline");
        assert_eq!(op_timeout(&Unbounded, 8_000), 8_000);
    }

    /// Before the HTTP exchange a failure keeps its own name while there is
    /// time, and is the deadline once there is none.
    #[test]
    fn failures_before_http_become_the_deadline_only_when_spent() {
        let left = Cell::new(1u32);
        let budget = || left.get();
        let e = HttpsError::Resolve(NetError::Ipc(aricode_sys::Error::Timeout));
        assert_eq!(before_http(&budget, Phase::Resolve, e), e);
        left.set(0);
        assert_eq!(
            before_http(&budget, Phase::Connect, e),
            HttpsError::Deadline(Phase::Connect)
        );
    }
}
