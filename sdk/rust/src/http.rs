//! HTTP GET client over `NetClient` (Phase 24): resolves, connects and
//! delegates all HTTP logic to `aricode-http`, which is tested on the host.

use crate::net::{NetClient, NetError};
pub use aricode_http::Fetched;
use aricode_http::{Error, Transport, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    BadUrl,
    Resolve(NetError),
    Connect(NetError),
    Io(NetError),
    /// The response is not HTTP, is truncated or has invalid framing.
    Malformed,
    /// Headers or body do not fit in the caller's buffer.
    TooBig,
    /// Unsupported encoding or protocol switch.
    Unsupported,
}

/// netd TCP connection seen as an HTTP transport.
struct Stream<'a> {
    net: &'a mut NetClient,
    sock: u16,
    timeout_ms: u32,
}

impl Transport for Stream<'_> {
    type Error = NetError;
    fn send_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        self.net.tcp_send_all(self.sock, data)
    }
    fn recv(&mut self, out: &mut [u8]) -> Result<usize, NetError> {
        self.net.tcp_recv(self.sock, self.timeout_ms, out)
    }
}

/// Downloads `url` (literal IP or name resolved by netd) into `out`.
pub fn get(
    net: &mut NetClient,
    url: &str,
    out: &mut [u8],
    timeout_ms: u32,
) -> Result<Fetched, HttpError> {
    let url = Url::parse(url).ok_or(HttpError::BadUrl)?;
    let addr = match aricode_net::Ipv4Addr::parse(url.host) {
        Some(ip) => ip.0,
        None => net
            .resolve(url.host, None, timeout_ms)
            .map_err(HttpError::Resolve)?,
    };
    let sock = net
        .tcp_connect(addr, url.port, timeout_ms)
        .map_err(HttpError::Connect)?;
    let result = aricode_http::fetch(
        &mut Stream {
            net,
            sock,
            timeout_ms,
        },
        &url,
        out,
    );
    let _ = net.tcp_close(sock);
    result.map_err(|e| match e {
        Error::BadUrl => HttpError::BadUrl,
        Error::Transport(e) => HttpError::Io(e),
        Error::Malformed => HttpError::Malformed,
        Error::TooBig => HttpError::TooBig,
        Error::Unsupported => HttpError::Unsupported,
    })
}
