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
//! caller past its budget. The TCP and TLS layers are behind the private
//! `Tcp` and `Records` traits so the stream is tested on the host.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
pub use aricode_http::{Budget, Fetched, Phase, Unbounded};
use aricode_http::{Error, Transport, Url};
use aricode_sys::net::{NetClient, NetError};
use aricode_sys::{self as sys, Handle};
use aricode_tls::{Client, Event};

/// Bytes of TLS records accepted in a row without a byte of plaintext
/// coming out of them, once connected. Zero-length application-data
/// records (RFC 8446 §5.1), session tickets and key updates are legitimate
/// in small numbers; a stream of them is a stall that keeps every receive
/// under its timeout (incident 0009, audit of the fix). Counted in bytes,
/// not records, so a large record arriving in small segments is not
/// mistaken for one.
pub const MAX_SILENT_BYTES: usize = 64 * 1024;

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
    /// More than [`MAX_SILENT_BYTES`] of TLS records in a row carried no
    /// application data.
    EmptyRecords,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamError {
    Net(NetError),
    Tls(aricode_tls::Error),
    /// The budget ran out between two TLS records. `fetch_with` names the
    /// phase: it sees the budget spent and reports `Deadline` itself.
    Deadline,
    /// `MAX_SILENT_BYTES` exceeded.
    EmptyRecords,
}

/// The TCP side of a stream: a netd socket in programs, a script in tests.
trait Tcp {
    fn recv(&mut self, timeout_ms: u32, out: &mut [u8]) -> Result<usize, NetError>;
    fn send_all_within(&mut self, data: &[u8], timeout_ms: u32) -> Result<(), NetError>;
}

struct Socket<'a> {
    net: &'a mut NetClient,
    sock: u16,
}

impl Tcp for Socket<'_> {
    fn recv(&mut self, timeout_ms: u32, out: &mut [u8]) -> Result<usize, NetError> {
        self.net.tcp_recv(self.sock, timeout_ms, out)
    }
    fn send_all_within(&mut self, data: &[u8], timeout_ms: u32) -> Result<(), NetError> {
        self.net.tcp_send_all_within(self.sock, data, timeout_ms)
    }
}

/// The TLS record layer of a stream (`aricode_tls::Client` in programs).
trait Records {
    fn start(&mut self, out: &mut Vec<u8>);
    fn is_connected(&self) -> bool;
    fn feed(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), aricode_tls::Error>;
    fn send(&mut self, data: &[u8], out: &mut Vec<u8>);
    fn close(&mut self, out: &mut Vec<u8>);
}

impl Records for Client {
    fn start(&mut self, out: &mut Vec<u8>) {
        Client::start(self, out)
    }
    fn is_connected(&self) -> bool {
        Client::is_connected(self)
    }
    fn feed(
        &mut self,
        input: &[u8],
        out: &mut Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), aricode_tls::Error> {
        Client::feed(self, input, out, events)
    }
    fn send(&mut self, data: &[u8], out: &mut Vec<u8>) {
        Client::send(self, data, out)
    }
    fn close(&mut self, out: &mut Vec<u8>) {
        Client::close(self, out)
    }
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
struct Stream<'a, T: Tcp, R: Records> {
    tcp: T,
    budget: &'a dyn Budget,
    cap_ms: u32,
    tls: Option<Tls<R>>,
    /// Record bytes received since the last byte of plaintext.
    silent: usize,
}

struct Tls<R: Records> {
    client: R,
    plain: Vec<u8>,
    closed: bool,
}

impl<T: Tcp, R: Records> Stream<'_, T, R> {
    fn timeout(&self) -> u32 {
        op_timeout(self.budget, self.cap_ms)
    }

    fn send_raw(&mut self, data: &[u8]) -> Result<(), StreamError> {
        if data.is_empty() {
            return Ok(());
        }
        let timeout = self.timeout();
        self.tcp
            .send_all_within(data, timeout)
            .map_err(StreamError::Net)
    }

    /// Reads from TCP and feeds the TLS client; returns the bytes read,
    /// `0` at EOF.
    fn pump(&mut self) -> Result<usize, StreamError> {
        let mut buf = [0u8; 1400];
        let timeout = self.timeout();
        let n = self.tcp.recv(timeout, &mut buf).map_err(StreamError::Net)?;
        if n == 0 {
            return Ok(0);
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
        Ok(n)
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
                Ok(0) => {
                    return Err(HttpsError::Tls(aricode_tls::Error::Protocol(
                        "connection closed during the handshake",
                    )))
                }
                Ok(_) => {}
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
            StreamError::Deadline => HttpsError::Deadline(Phase::Handshake),
            StreamError::EmptyRecords => HttpsError::EmptyRecords,
        }
    }

    /// Says goodbye (TLS `close_notify`) if the stream is a TLS one.
    fn close(&mut self) {
        if let Some(tls) = self.tls.as_mut() {
            let mut bye = Vec::new();
            tls.client.close(&mut bye);
            let _ = self.send_raw(&bye);
        }
    }
}

impl<T: Tcp, R: Records> Transport for Stream<'_, T, R> {
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
            return self.tcp.recv(timeout, out).map_err(StreamError::Net);
        }
        loop {
            let tls = self.tls.as_mut().expect("TLS stream");
            if !tls.plain.is_empty() {
                let n = out.len().min(tls.plain.len());
                out[..n].copy_from_slice(&tls.plain[..n]);
                tls.plain.drain(..n);
                return Ok(n);
            }
            if tls.closed {
                return Ok(0);
            }
            // Records without application data keep every receive under
            // its timeout: the budget and the silence limit end them.
            if self.budget.remaining_ms() == 0 {
                return Err(StreamError::Deadline);
            }
            let n = self.pump()?;
            if n == 0 {
                return Ok(0);
            }
            let tls = self.tls.as_ref().expect("TLS stream");
            if tls.plain.is_empty() {
                self.silent += n;
                if self.silent > MAX_SILENT_BYTES {
                    return Err(StreamError::EmptyRecords);
                }
            } else {
                self.silent = 0;
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
        tcp: Socket { net, sock },
        budget,
        cap_ms: timeout_ms,
        tls,
        silent: 0,
    };
    let result = match stream.tls.is_some() {
        true => stream.handshake(),
        false => Ok(()),
    }
    .and_then(|()| fetch(&mut stream, &url, budget, sink));
    stream.close();
    let _ = stream.tcp.net.tcp_close(sock);
    result
}

/// The HTTP exchange over a connected stream, with its errors named.
fn fetch<T: Tcp, R: Records>(
    stream: &mut Stream<'_, T, R>,
    url: &Url,
    budget: &dyn Budget,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<u16, HttpsError> {
    // The 8 KiB header buffer on the heap: on the stack it sat in the
    // frame of every caller's download, next to the TLS certificate
    // checks (incident 0018).
    let mut head: Box<[u8; aricode_http::MAX_HEADERS]> =
        match alloc::vec![0u8; aricode_http::MAX_HEADERS]
            .into_boxed_slice()
            .try_into()
        {
            Ok(h) => h,
            Err(_) => unreachable!("the vector has MAX_HEADERS bytes"),
        };
    aricode_http::fetch_with_head(stream, url, budget, sink, &mut head).map_err(|e| match e {
        Error::BadUrl => HttpsError::BadUrl,
        Error::Transport(StreamError::Net(e)) => HttpsError::Io(e),
        Error::Transport(StreamError::Tls(e)) => HttpsError::Tls(e),
        // `fetch_with` reports the deadline itself whenever the budget is
        // spent; this is left for a budget that grew back meanwhile.
        Error::Transport(StreamError::Deadline) => HttpsError::Deadline(Phase::Body),
        Error::Transport(StreamError::EmptyRecords) => HttpsError::EmptyRecords,
        Error::Malformed => HttpsError::Malformed,
        Error::TooBig => HttpsError::TooBig,
        Error::Unsupported => HttpsError::Unsupported,
        Error::Aborted => HttpsError::Aborted,
        Error::Deadline(phase) => HttpsError::Deadline(phase),
        Error::TooManyInformational => HttpsError::TooManyInformational,
        Error::HeaderBytes => HttpsError::HeaderBytes,
    })
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

    // ------------------------------------------ the TLS stream on the host

    /// Simulated clock with a deadline, in milliseconds.
    struct Clock {
        now: Cell<u64>,
        deadline: u64,
    }

    impl Clock {
        fn budget(&self) -> impl Fn() -> u32 + '_ {
            move || self.deadline.saturating_sub(self.now.get()) as u32
        }
    }

    /// TCP side with data always queued: every receive returns at once
    /// (never the timeout) with up to `bytes` bytes, taking `step_ms` of
    /// simulated time. It delivers `script` first; after that, record-like
    /// filler for ever if `endless`, EOF otherwise.
    struct Queued<'a> {
        clock: &'a Clock,
        step_ms: u64,
        bytes: usize,
        script: Vec<u8>,
        endless: bool,
        receives: u32,
    }

    impl Tcp for Queued<'_> {
        fn recv(&mut self, _timeout_ms: u32, out: &mut [u8]) -> Result<usize, NetError> {
            self.receives += 1;
            assert!(self.receives < 100_000, "runaway receive loop");
            self.clock.now.set(self.clock.now.get() + self.step_ms);
            let n = self.bytes.min(out.len());
            if !self.script.is_empty() {
                let n = n.min(self.script.len());
                out[..n].copy_from_slice(&self.script[..n]);
                self.script.drain(..n);
                return Ok(n);
            }
            if !self.endless {
                return Ok(0);
            }
            out[..n].fill(0x17);
            Ok(n)
        }
        fn send_all_within(&mut self, _: &[u8], _: u32) -> Result<(), NetError> {
            Ok(())
        }
    }

    /// Record layer whose records never carry application data (zero-length
    /// records, tickets): `connected` says whether the handshake is over.
    struct Silent {
        connected: bool,
    }

    impl Records for Silent {
        fn start(&mut self, _: &mut Vec<u8>) {}
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn feed(
            &mut self,
            _: &[u8],
            _: &mut Vec<u8>,
            _: &mut Vec<Event>,
        ) -> Result<(), aricode_tls::Error> {
            Ok(())
        }
        fn send(&mut self, _: &[u8], _: &mut Vec<u8>) {}
        fn close(&mut self, _: &mut Vec<u8>) {}
    }

    /// Record layer that hands every byte through as plaintext.
    struct Plain;

    impl Records for Plain {
        fn start(&mut self, _: &mut Vec<u8>) {}
        fn is_connected(&self) -> bool {
            true
        }
        fn feed(
            &mut self,
            input: &[u8],
            _: &mut Vec<u8>,
            events: &mut Vec<Event>,
        ) -> Result<(), aricode_tls::Error> {
            events.push(Event::Data(input.to_vec()));
            Ok(())
        }
        fn send(&mut self, _: &[u8], _: &mut Vec<u8>) {}
        fn close(&mut self, _: &mut Vec<u8>) {}
    }

    /// A TLS stream over `Queued`, connected or not according to `records`.
    fn stream<'a, R: Records>(
        clock: &'a Clock,
        budget: &'a dyn Budget,
        step_ms: u64,
        bytes: usize,
        script: &[u8],
        records: R,
    ) -> Stream<'a, Queued<'a>, R> {
        Stream {
            tcp: Queued {
                clock,
                step_ms,
                bytes,
                script: script.to_vec(),
                endless: script.is_empty(),
                receives: 0,
            },
            budget,
            cap_ms: 8_000,
            tls: Some(Tls {
                client: records,
                plain: Vec::new(),
                closed: false,
            }),
            silent: 0,
        }
    }

    fn clock(deadline: u64) -> Clock {
        Clock {
            now: Cell::new(0),
            deadline,
        }
    }

    /// After the handshake a server streaming records without application
    /// data, each receive answered at once, used to hold the client for
    /// ever: the receive loop now asks the budget between records and the
    /// fetch ends at the deadline, in the headers phase, sink never called.
    #[test]
    fn empty_records_forever_end_at_the_deadline() {
        let clock = clock(60_000);
        let budget = clock.budget();
        // One byte per second: far under the silence limit, so this is the
        // budget alone at work.
        let mut s = stream(&clock, &budget, 1_000, 1, b"", Silent { connected: true });
        let url = Url::parse("https://host/index").unwrap();
        let mut calls = 0;
        let r = fetch(&mut s, &url, &budget, &mut |_: &[u8]| {
            calls += 1;
            true
        });
        assert_eq!(r, Err(HttpsError::Deadline(Phase::Headers)));
        assert_eq!(
            clock.now.get(),
            60_000,
            "not a millisecond past the deadline"
        );
        assert_eq!(calls, 0);
        assert_eq!(s.tcp.receives, 60);
    }

    /// Records without application data arriving fast are refused by the
    /// silence limit long before the deadline.
    #[test]
    fn empty_records_in_bulk_are_refused_by_the_silence_limit() {
        let clock = clock(600_000);
        let budget = clock.budget();
        let mut s = stream(&clock, &budget, 10, 1400, b"", Silent { connected: true });
        let url = Url::parse("https://host/index").unwrap();
        let r = fetch(&mut s, &url, &budget, &mut |_: &[u8]| true);
        assert_eq!(r, Err(HttpsError::EmptyRecords));
        assert_eq!(s.tcp.receives as usize, MAX_SILENT_BYTES / 1400 + 1);
        assert!(clock.now.get() < 1_000);
    }

    /// A handshake that never completes, records arriving at once, ends at
    /// the deadline in the handshake phase.
    #[test]
    fn handshake_without_progress_ends_at_the_deadline() {
        let clock = clock(30_000);
        let budget = clock.budget();
        let mut s = stream(
            &clock,
            &budget,
            1_000,
            100,
            b"",
            Silent { connected: false },
        );
        assert_eq!(s.handshake(), Err(HttpsError::Deadline(Phase::Handshake)));
        assert_eq!(clock.now.get(), 30_000);
    }

    /// Plaintext flows: a response arriving seven bytes per record, each
    /// taking a second, is delivered whole, and every byte of data starts
    /// the silence counter over.
    #[test]
    fn plaintext_within_budget_is_delivered() {
        let clock = clock(60_000);
        let budget = clock.budget();
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let mut s = stream(&clock, &budget, 1_000, 7, response, Plain);
        s.silent = MAX_SILENT_BYTES;
        let url = Url::parse("https://host/index").unwrap();
        let mut got = Vec::new();
        let r = fetch(&mut s, &url, &budget, &mut |c: &[u8]| {
            got.extend_from_slice(c);
            true
        });
        assert_eq!(r, Ok(200));
        assert_eq!(got, b"hello");
        assert_eq!(s.silent, 0, "data resets the silence counter");
        assert_eq!(s.tcp.receives as usize, response.len().div_ceil(7));
    }
}
