//! HTTP/1.1 mínimo (Fase 24): construye peticiones `GET` y analiza
//! respuestas con `Content-Length` o `Transfer-Encoding: chunked`, sin
//! `alloc` y sobre buffers del llamador. TLS llegará más adelante.

#![no_std]

/// Escribe `GET path HTTP/1.1` con `Host` y `Connection: close`.
pub fn write_get(host: &str, path: &str, out: &mut [u8]) -> Option<usize> {
    let parts: [&[u8]; 7] = [
        b"GET ",
        path.as_bytes(),
        b" HTTP/1.1\r\nHost: ",
        host.as_bytes(),
        b"\r\nUser-Agent: AriCodeOS/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        b"",
        b"",
    ];
    let mut n = 0;
    for p in parts {
        out.get_mut(n..n + p.len())?.copy_from_slice(p);
        n += p.len();
    }
    Some(n)
}

/// URL `http://host[:port]/path` descompuesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

impl<'a> Url<'a> {
    /// Acepta `http://host`, `host:puerto/ruta` y `host/ruta`.
    pub fn parse(s: &'a str) -> Option<Self> {
        let rest = s.strip_prefix("http://").unwrap_or(s);
        if rest.starts_with("https://") || rest.is_empty() {
            return None;
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (authority, 80),
        };
        if host.is_empty() || host.contains(' ') || path.contains(' ') {
            return None;
        }
        Some(Url { host, port, path })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLength {
    Length(usize),
    Chunked,
    /// Hasta que el servidor cierre.
    UntilClose,
}

/// Cabecera de respuesta analizada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: BodyLength,
    /// Desplazamiento del cuerpo dentro del buffer analizado.
    pub header_len: usize,
}

/// Analiza la línea de estado y las cabeceras. `None` si aún no ha llegado
/// la línea vacía; `Some(Err(()))` si la respuesta no es HTTP.
pub fn parse_response(buf: &[u8]) -> Option<Result<Response, ()>> {
    let end = find(buf, b"\r\n\r\n")? + 4;
    let head = core::str::from_utf8(&buf[..end]).ok();
    let Some(head) = head else {
        return Some(Err(()));
    };
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if !status_line.starts_with("HTTP/1.") {
        return Some(Err(()));
    }
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());
    let Some(status) = status else {
        return Some(Err(()));
    };
    let mut body = BodyLength::UntilClose;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if let Ok(n) = value.parse() {
                body = BodyLength::Length(n);
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase_contains("chunked")
        {
            body = BodyLength::Chunked;
        }
    }
    Some(Ok(Response {
        status,
        body,
        header_len: end,
    }))
}

trait LowerContains {
    fn to_ascii_lowercase_contains(&self, needle: &str) -> bool;
}

impl LowerContains for str {
    fn to_ascii_lowercase_contains(&self, needle: &str) -> bool {
        self.len() >= needle.len()
            && self
                .as_bytes()
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decodificador incremental de cuerpos `chunked`: copia los datos de los
/// trozos completos que haya en `input` a `out`. Devuelve
/// `(bytes de entrada consumidos, bytes escritos, fin alcanzado)`.
pub fn decode_chunked(input: &[u8], out: &mut [u8]) -> Option<(usize, usize, bool)> {
    let mut i = 0;
    let mut written = 0;
    loop {
        let line_end = find(&input[i..], b"\r\n")? + i;
        let size_str = core::str::from_utf8(&input[i..line_end]).ok()?;
        let size_str = size_str.split(';').next()?.trim();
        let size = usize::from_str_radix(size_str, 16).ok()?;
        if size == 0 {
            // Fin: `0\r\n[trailers]\r\n`; si solo llegó `0\r\n`, esperar más datos.
            let trailer_end = find(&input[line_end..], b"\r\n\r\n").map(|t| line_end + t + 4);
            return trailer_end.map(|end| (end, written, true));
        }
        let data_start = line_end + 2;
        let data_end = data_start + size;
        if input.len() < data_end + 2 {
            return Some((i, written, false));
        }
        let room = out.len() - written;
        let n = size.min(room);
        out[written..written + n].copy_from_slice(&input[data_start..data_start + n]);
        written += n;
        if n < size {
            return Some((i, written, false)); // sin sitio: reintentar con `out` vacío
        }
        i = data_end + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_request_and_url() {
        let mut buf = [0u8; 256];
        let n = write_get("10.0.2.2:17010", "/hola", &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.starts_with("GET /hola HTTP/1.1\r\nHost: 10.0.2.2:17010\r\n"));
        assert!(text.ends_with("Connection: close\r\n\r\n"));
        assert_eq!(
            Url::parse("http://10.0.2.2:17010/hola"),
            Some(Url {
                host: "10.0.2.2",
                port: 17010,
                path: "/hola"
            })
        );
        assert_eq!(
            Url::parse("example.org"),
            Some(Url {
                host: "example.org",
                port: 80,
                path: "/"
            })
        );
        assert!(Url::parse("https://x").is_none());
        assert!(Url::parse("").is_none());
        assert!(Url::parse("host:puerto/").is_none());
    }

    #[test]
    fn response_headers() {
        let raw = b"HTTP/1.1 200 OK\r\nServer: xtask\r\nContent-Length: 5\r\n\r\nhola!";
        let r = parse_response(raw).unwrap().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, BodyLength::Length(5));
        assert_eq!(&raw[r.header_len..], b"hola!");
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Le").is_none(),
            "incompleta"
        );
        assert_eq!(parse_response(b"SSH-2.0\r\n\r\n"), Some(Err(())));
        let chunked = b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: Chunked\r\n\r\n";
        let r = parse_response(chunked).unwrap().unwrap();
        assert_eq!((r.status, r.body), (404, BodyLength::Chunked));
        let close = parse_response(b"HTTP/1.0 200 OK\r\n\r\n").unwrap().unwrap();
        assert_eq!(close.body, BodyLength::UntilClose);
    }

    #[test]
    fn chunked_decoding() {
        let mut out = [0u8; 64];
        let full = b"4\r\nhola\r\n6;ext=1\r\n mundo\r\n0\r\n\r\n";
        let (used, written, done) = decode_chunked(full, &mut out).unwrap();
        assert_eq!((used, written, done), (full.len(), 10, true));
        assert_eq!(&out[..10], b"hola mundo");
        // Trozo incompleto: consume solo lo completo.
        let partial = b"4\r\nhola\r\n6\r\n mun";
        let (used, written, done) = decode_chunked(partial, &mut out).unwrap();
        assert_eq!((used, written, done), (9, 4, false));
        assert!(decode_chunked(b"4\r\nho", &mut out).is_some());
        assert!(decode_chunked(b"zz\r\n", &mut out).is_none());
    }
}
