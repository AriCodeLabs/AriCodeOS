// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! HTTP GET client over `NetClient` (Phase 24): resolves, connects and
//! delegates all HTTP logic to `aricode-http`, which is tested on the host.

use crate::net::{NetClient, NetError};
pub use aricode_http::{Budget, Fetched, Phase, Unbounded};
use aricode_http::{Error, Transport, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    BadUrl,
    Resolve(NetError),
    Connect(NetError),
    Io(NetError),
    /// The response is not HTTP, is truncated or has invalid framing.
    Malformed,
    /// The response headers do not fit (a body that does not fit is
    /// truncated: see `Fetched::truncated`).
    TooBig,
    /// Unsupported encoding or protocol switch.
    Unsupported,
    /// The caller's budget ran out in this phase (incident 0009).
    Deadline(Phase),
    /// More than `aricode_http::MAX_INFORMATIONAL` interim responses.
    TooManyInformational,
    /// Headers of the exchange, interim responses included, exceed
    /// `aricode_http::MAX_HEADER_BYTES`.
    HeaderBytes,
}

/// netd TCP connection seen as an HTTP transport: every operation gets
/// what is left of the budget, never more than `cap_ms`.
struct Stream<'a> {
    net: &'a mut NetClient,
    sock: u16,
    budget: &'a dyn Budget,
    cap_ms: u32,
}

impl Stream<'_> {
    fn timeout(&self) -> u32 {
        self.budget.remaining_ms().min(self.cap_ms)
    }
}

impl Transport for Stream<'_> {
    type Error = NetError;
    fn send_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        let timeout = self.timeout();
        self.net.tcp_send_all_within(self.sock, data, timeout)
    }
    fn recv(&mut self, out: &mut [u8]) -> Result<usize, NetError> {
        let timeout = self.timeout();
        self.net.tcp_recv(self.sock, timeout, out)
    }
}

/// Downloads `url` (literal IP or name resolved by netd) into `out`.
/// `timeout_ms` bounds one network operation, `budget` the whole download
/// (`aricode_http::Budget`; `Unbounded` for a caller without a clock).
pub fn get(
    net: &mut NetClient,
    url: &str,
    out: &mut [u8],
    timeout_ms: u32,
    budget: &dyn Budget,
) -> Result<Fetched, HttpError> {
    let url = Url::parse(url).ok_or(HttpError::BadUrl)?;
    // Plain HTTP only; `aricode-https` adds TLS (it needs `alloc`).
    if url.secure {
        return Err(HttpError::BadUrl);
    }
    let op = |phase: Phase| match budget.remaining_ms() {
        0 => Err(HttpError::Deadline(phase)),
        left => Ok(left.min(timeout_ms)),
    };
    let addr = match aricode_net::Ipv4Addr::parse(url.host) {
        Some(ip) => ip.0,
        None => net
            .resolve(url.host, None, op(Phase::Resolve)?)
            .map_err(HttpError::Resolve)?,
    };
    let sock = net
        .tcp_connect(addr, url.port, op(Phase::Connect)?)
        .map_err(HttpError::Connect)?;
    let result = aricode_http::fetch(
        &mut Stream {
            net,
            sock,
            budget,
            cap_ms: timeout_ms,
        },
        &url,
        budget,
        out,
    );
    let _ = net.tcp_close(sock);
    result.map_err(|e| match e {
        Error::BadUrl => HttpError::BadUrl,
        Error::Transport(e) => HttpError::Io(e),
        Error::Malformed => HttpError::Malformed,
        Error::TooBig => HttpError::TooBig,
        Error::Unsupported => HttpError::Unsupported,
        // The buffer sink never aborts.
        Error::Aborted => HttpError::Malformed,
        Error::Deadline(phase) => HttpError::Deadline(phase),
        Error::TooManyInformational => HttpError::TooManyInformational,
        Error::HeaderBytes => HttpError::HeaderBytes,
    })
}
