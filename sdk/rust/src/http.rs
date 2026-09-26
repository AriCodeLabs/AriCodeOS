//! Cliente HTTP GET sobre `NetClient` (Fase 24).

use crate::net::{NetClient, NetError};
use aricode_http::{decode_chunked, parse_response, write_get, BodyLength, Url};
use aricode_proto::net::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    BadUrl,
    Resolve(NetError),
    Connect(NetError),
    Io(NetError),
    /// La respuesta no es HTTP o está truncada.
    Malformed,
    /// El cuerpo no cabe en el buffer del llamador.
    TooBig,
}

/// Resultado de un GET: código de estado y bytes del cuerpo escritos en `out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    pub body_len: usize,
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
    let result = fetch(net, sock, &url, out, timeout_ms);
    let _ = net.tcp_close(sock);
    result
}

fn fetch(
    net: &mut NetClient,
    sock: u16,
    url: &Url,
    out: &mut [u8],
    timeout_ms: u32,
) -> Result<Fetched, HttpError> {
    let mut req = [0u8; 512];
    let mut host = [0u8; 80];
    let host_len = write_host(url, &mut host);
    let host_str = core::str::from_utf8(&host[..host_len]).map_err(|_| HttpError::BadUrl)?;
    let n = write_get(host_str, url.path, &mut req).ok_or(HttpError::BadUrl)?;
    net.tcp_send_all(sock, &req[..n]).map_err(HttpError::Io)?;

    // Cabecera: leer hasta la línea vacía en un buffer temporal.
    let mut head = [0u8; 2048];
    let mut have = 0;
    let response = loop {
        if let Some(r) = parse_response(&head[..have]) {
            break r.map_err(|_| HttpError::Malformed)?;
        }
        if have == head.len() {
            return Err(HttpError::TooBig);
        }
        match net.tcp_recv(sock, timeout_ms, &mut head[have..]) {
            Ok(0) => return Err(HttpError::Malformed),
            Ok(k) => have += k,
            Err(e) => return Err(HttpError::Io(e)),
        }
    };
    let mut body_len = 0;
    let rest = &head[response.header_len..have];
    match response.body {
        BodyLength::Length(len) => {
            if len > out.len() {
                return Err(HttpError::TooBig);
            }
            let first = rest.len().min(len);
            out[..first].copy_from_slice(&rest[..first]);
            body_len = first;
            while body_len < len {
                match net.tcp_recv(sock, timeout_ms, &mut out[body_len..len]) {
                    Ok(0) => return Err(HttpError::Malformed),
                    Ok(k) => body_len += k,
                    Err(e) => return Err(HttpError::Io(e)),
                }
            }
        }
        BodyLength::UntilClose => {
            let first = rest.len().min(out.len());
            out[..first].copy_from_slice(&rest[..first]);
            body_len = first;
            loop {
                if body_len == out.len() {
                    return Err(HttpError::TooBig);
                }
                match net.tcp_recv(sock, timeout_ms, &mut out[body_len..]) {
                    Ok(0) => break,
                    Ok(k) => body_len += k,
                    Err(NetError::Status(Status::Reset)) => break,
                    Err(e) => return Err(HttpError::Io(e)),
                }
            }
        }
        BodyLength::Chunked => {
            let mut raw = [0u8; 4096];
            let mut raw_len = rest.len().min(raw.len());
            raw[..raw_len].copy_from_slice(&rest[..raw_len]);
            loop {
                match decode_chunked(&raw[..raw_len], &mut out[body_len..]) {
                    Some((used, written, done)) => {
                        body_len += written;
                        raw.copy_within(used..raw_len, 0);
                        raw_len -= used;
                        if done {
                            break;
                        }
                        if written == 0 && used == 0 && raw_len == raw.len() {
                            return Err(HttpError::TooBig);
                        }
                    }
                    None if raw_len == raw.len() => return Err(HttpError::Malformed),
                    None => {}
                }
                match net.tcp_recv(sock, timeout_ms, &mut raw[raw_len..]) {
                    Ok(0) => return Err(HttpError::Malformed),
                    Ok(k) => raw_len += k,
                    Err(e) => return Err(HttpError::Io(e)),
                }
            }
        }
    }
    Ok(Fetched {
        status: response.status,
        body_len,
    })
}

/// `host[:puerto]` para la cabecera `Host`.
fn write_host(url: &Url, out: &mut [u8; 80]) -> usize {
    let h = url.host.as_bytes();
    let n = h.len().min(out.len() - 6);
    out[..n].copy_from_slice(&h[..n]);
    if url.port == 80 {
        return n;
    }
    out[n] = b':';
    let mut digits = [0u8; 5];
    let mut v = url.port;
    let mut d = 5;
    while v > 0 {
        d -= 1;
        digits[d] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let len = 5 - d;
    out[n + 1..n + 1 + len].copy_from_slice(&digits[d..]);
    n + 1 + len
}
