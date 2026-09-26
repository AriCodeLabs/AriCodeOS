//! Cliente HTTP GET sobre `NetClient` (Fase 24): resuelve, conecta y delega
//! toda la lógica HTTP en `aricode-http`, que se prueba en host.

use crate::net::{NetClient, NetError};
pub use aricode_http::Fetched;
use aricode_http::{Error, Transport, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    BadUrl,
    Resolve(NetError),
    Connect(NetError),
    Io(NetError),
    /// La respuesta no es HTTP, está truncada o su framing es inválido.
    Malformed,
    /// Cabeceras o cuerpo no caben en el buffer del llamador.
    TooBig,
    /// Codificación o cambio de protocolo que no se soporta.
    Unsupported,
}

/// Conexión TCP de netd vista como transporte HTTP.
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

/// Descarga `url` (IP literal o nombre resuelto por netd) en `out`.
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
