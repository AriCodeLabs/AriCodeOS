// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! HTTP and HTTPS GET for AriCodeOS programs. `http://` goes straight over
//! the TCP service; `https://` runs TLS 1.3 (`aricode-tls`) on top, with
//! the certificate checked against the embedded roots
//! (`aricode_x509::roots::DEFAULT`), the host name and, if the kernel has
//! a real-time clock, the validity dates. Needs `alloc` (the TLS client
//! buffers records), so it lives outside `aricode-sys`.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
pub use aricode_http::Fetched;
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
}

#[derive(Debug, Clone, Copy)]
enum StreamError {
    Net(NetError),
    Tls(aricode_tls::Error),
}

/// A TCP connection, optionally wrapped in TLS, as an HTTP transport.
struct Stream<'a> {
    net: &'a mut NetClient,
    sock: u16,
    timeout_ms: u32,
    tls: Option<Tls>,
}

struct Tls {
    client: Client,
    plain: Vec<u8>,
    closed: bool,
}

impl Stream<'_> {
    fn send_raw(&mut self, data: &[u8]) -> Result<(), StreamError> {
        if data.is_empty() {
            return Ok(());
        }
        self.net
            .tcp_send_all(self.sock, data)
            .map_err(StreamError::Net)
    }

    /// Reads from TCP and feeds the TLS client; returns false at EOF.
    fn pump(&mut self) -> Result<bool, StreamError> {
        let mut buf = [0u8; 1400];
        let n = self
            .net
            .tcp_recv(self.sock, self.timeout_ms, &mut buf)
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

    fn handshake(&mut self) -> Result<(), StreamError> {
        let mut out = Vec::new();
        self.tls
            .as_mut()
            .expect("TLS stream")
            .client
            .start(&mut out);
        self.send_raw(&out)?;
        while !self.tls.as_ref().expect("TLS stream").client.is_connected() {
            if !self.pump()? {
                return Err(StreamError::Tls(aricode_tls::Error::Protocol(
                    "connection closed during the handshake",
                )));
            }
        }
        Ok(())
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
            return self
                .net
                .tcp_recv(self.sock, self.timeout_ms, out)
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
/// e.g. a private CA. A body larger than `out` is truncated.
pub fn get(
    net: &mut NetClient,
    system: Handle,
    extra_roots: &[Vec<u8>],
    url: &str,
    out: &mut [u8],
    timeout_ms: u32,
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
    let status = get_with(net, system, extra_roots, url, timeout_ms, &mut sink)?;
    Ok(Fetched {
        status,
        body_len,
        truncated,
    })
}

/// Streaming download: the body goes to `sink` piece by piece (any size,
/// in order), which returns `false` to abort. Returns the status code.
pub fn get_with(
    net: &mut NetClient,
    system: Handle,
    extra_roots: &[Vec<u8>],
    url: &str,
    timeout_ms: u32,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<u16, HttpsError> {
    let url = Url::parse(url).ok_or(HttpsError::BadUrl)?;
    let addr = match aricode_net::Ipv4Addr::parse(url.host) {
        Some(ip) => ip.0,
        None => net
            .resolve(url.host, None, timeout_ms)
            .map_err(HttpsError::Resolve)?,
    };
    let sock = net
        .tcp_connect(addr, url.port, timeout_ms)
        .map_err(HttpsError::Connect)?;
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
        timeout_ms,
        tls,
    };
    let result = match stream.tls.is_some() {
        true => stream.handshake().map_err(Error::Transport),
        false => Ok(()),
    }
    .and_then(|()| aricode_http::fetch_with(&mut stream, &url, sink));
    if let Some(tls) = stream.tls.as_mut() {
        let mut bye = Vec::new();
        tls.client.close(&mut bye);
        let _ = stream.send_raw(&bye);
    }
    let _ = stream.net.tcp_close(sock);
    result.map_err(|e| match e {
        Error::BadUrl => HttpsError::BadUrl,
        Error::Transport(StreamError::Net(e)) => HttpsError::Io(e),
        Error::Transport(StreamError::Tls(e)) => HttpsError::Tls(e),
        Error::Malformed => HttpsError::Malformed,
        Error::TooBig => HttpsError::TooBig,
        Error::Unsupported => HttpsError::Unsupported,
        Error::Aborted => HttpsError::Aborted,
    })
}
