// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal HTTP/1.1 (Phase 24): `GET` requests, response parsing and
//! complete download over an abstract [`Transport`], without `alloc` and on
//! caller-supplied buffers. The logic does not touch the network, so it is
//! tested on the host with a simulated transport; the SDK only supplies the TCP transport.
//! TLS will come later.

#![no_std]

/// Maximum length of a host name (RFC 1035).
pub const MAX_HOST: usize = 253;
/// Maximum number of response header bytes accepted. Real sites send more
/// than the 2 KiB this started with (github.com answered with 5.3 KiB on
/// 2026-10-17, letsencrypt.org with 2.2 KiB); 8 KiB is what common servers
/// accept by default and what fits the 64 KiB user stack next to the TLS
/// state and the body buffer of the callers.
pub const MAX_HEADERS: usize = 8192;
/// Maximum number of informational (1xx) responses accepted before the
/// final one. A server may send a `100 Continue` and a few `103 Early
/// Hints`; a stream of them is a stall, not a response (incident 0009).
pub const MAX_INFORMATIONAL: u32 = 8;
/// Maximum header bytes of one fetch, discarded 1xx heads included; the
/// final response alone may use [`MAX_HEADERS`] (incident 0009). The
/// fetch is refused when more than this has arrived and the final
/// response is still incomplete, so one that completes within the next
/// receive is accepted.
pub const MAX_HEADER_BYTES: usize = 2 * MAX_HEADERS;
/// Maximum length of a chunk size line (with extensions).
const MAX_CHUNK_LINE: usize = 256;

/// Byte channel already connected to the server.
pub trait Transport {
    type Error;
    fn send_all(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Reads up to `out.len()` bytes; `Ok(0)` is a clean close by the server.
    fn recv(&mut self, out: &mut [u8]) -> Result<usize, Self::Error>;
}

/// Time budget of a whole fetch (incident 0009). [`fetch_with`] asks it
/// before every send and every receive and fails with [`Error::Deadline`]
/// once it answers `0`, whatever the server sends: a stream of interim
/// responses, header bytes one at a time or chunk trailers never reach
/// the sink, so the sink cannot be the deadline. Transports that wait (the
/// SDK's) bound each operation by the same budget, so the overshoot past
/// the deadline is at most one operation's timeout.
pub trait Budget {
    /// Milliseconds left for the fetch; `0` once the deadline passed.
    fn remaining_ms(&self) -> u32;
}

impl<F: Fn() -> u32> Budget for F {
    fn remaining_ms(&self) -> u32 {
        self()
    }
}

/// No absolute deadline: only the transport's own per-operation timeouts
/// bound the fetch. For a caller without a clock talking to a server it
/// controls (the test HTTP client); anything that fetches from a server it
/// does not control passes a real budget.
pub struct Unbounded;

impl Budget for Unbounded {
    fn remaining_ms(&self) -> u32 {
        u32::MAX
    }
}

/// Phase of a GET in which the budget ran out ([`Error::Deadline`]).
/// `Resolve`, `Connect` and `Handshake` precede the HTTP exchange and are
/// reported by the SDK transports; [`fetch_with`] reports the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Resolve,
    Connect,
    Handshake,
    /// Sending the request.
    Request,
    /// Waiting for the status line and headers (interim responses included).
    Headers,
    /// Body bytes, chunk framing and trailers.
    Body,
}

impl Phase {
    /// What was going on when the time ran out, for messages
    /// (`"deadline passed " + describe()`).
    pub fn describe(self) -> &'static str {
        match self {
            Phase::Resolve => "while resolving the host",
            Phase::Connect => "while connecting",
            Phase::Handshake => "in the TLS handshake",
            Phase::Request => "while sending the request",
            Phase::Headers => "while waiting for the response headers",
            Phase::Body => "while receiving the body",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    BadUrl,
    Transport(E),
    /// The response is not HTTP, is truncated or its framing is invalid.
    Malformed,
    /// Headers or body do not fit in the caller's buffers.
    TooBig,
    /// Unsupported encoding or protocol switch.
    Unsupported,
    /// The caller's sink refused more body data (`fetch_with`).
    Aborted,
    /// The caller's [`Budget`] ran out in this phase before the fetch
    /// completed.
    Deadline(Phase),
    /// More than [`MAX_INFORMATIONAL`] interim responses.
    TooManyInformational,
    /// The headers of this fetch, discarded interim responses included,
    /// exceed [`MAX_HEADER_BYTES`].
    HeaderBytes,
}

/// Result of a GET: status code and body bytes written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    /// Bytes of body in the caller's buffer.
    pub body_len: usize,
    /// The body did not fit: `body_len` is the buffer size and the rest
    /// was discarded (the connection is closed anyway).
    pub truncated: bool,
}

/// Writes `GET path HTTP/1.1` with `Host` and `Connection: close`.
pub fn write_get(host: &str, path: &str, out: &mut [u8]) -> Option<usize> {
    let parts: [&[u8]; 5] = [
        b"GET ",
        path.as_bytes(),
        b" HTTP/1.1\r\nHost: ",
        host.as_bytes(),
        b"\r\nUser-Agent: AriCodeOS/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
    ];
    let mut n = 0;
    for p in parts {
        out.get_mut(n..n + p.len())?.copy_from_slice(p);
        n += p.len();
    }
    Some(n)
}

/// Parsed `http[s]://host[:port]/path` URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
    /// `https://`: the transport must be TLS (default port 443).
    pub secure: bool,
}

impl<'a> Url<'a> {
    /// Accepts `http://host`, `https://host`, `host:port/path` and
    /// `host/path` (plain HTTP). Rejects empty hosts or hosts longer than
    /// [`MAX_HOST`] bytes, and spaces.
    pub fn parse(s: &'a str) -> Option<Self> {
        let (rest, secure) = match s.strip_prefix("https://") {
            Some(r) => (r, true),
            None => (s.strip_prefix("http://").unwrap_or(s), false),
        };
        if rest.is_empty() || rest.contains("://") {
            return None;
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (authority, if secure { 443 } else { 80 }),
        };
        if host.is_empty()
            || host.len() > MAX_HOST
            || host.contains(' ')
            || path.contains(' ')
            || path.bytes().any(|b| b < 0x20)
        {
            return None;
        }
        Some(Url {
            host,
            port,
            path,
            secure,
        })
    }

    /// `host[:port]` for the `Host` header (the scheme's default port is
    /// omitted).
    pub fn host_header<'b>(&self, out: &'b mut [u8; MAX_HOST + 6]) -> &'b str {
        let h = self.host.as_bytes();
        out[..h.len()].copy_from_slice(h);
        let mut n = h.len();
        if self.port != if self.secure { 443 } else { 80 } {
            out[n] = b':';
            n += 1;
            let mut digits = [0u8; 5];
            let mut v = self.port;
            let mut d = 5;
            while v > 0 {
                d -= 1;
                digits[d] = b'0' + (v % 10) as u8;
                v /= 10;
            }
            out[n..n + 5 - d].copy_from_slice(&digits[d..]);
            n += 5 - d;
        }
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLength {
    Length(usize),
    Chunked,
    /// Until the server closes.
    UntilClose,
}

/// Parsed response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: BodyLength,
    /// Offset of the body inside the parsed buffer.
    pub header_len: usize,
}

impl Response {
    /// Interim response (1xx): precedes the final one.
    pub fn is_informational(&self) -> bool {
        (100..200).contains(&self.status)
    }
}

/// Parses the status line and the headers. `None` if the empty line has
/// not arrived yet; `Some(Err(()))` if the response is not HTTP or its framing
/// is invalid (`Transfer-Encoding` other than `chunked`, `Content-Length`
/// repeated with different values or not numeric). `Transfer-Encoding:
/// chunked` takes precedence over `Content-Length` whatever the order
/// (RFC 9112 §6.3).
pub fn parse_response(buf: &[u8]) -> Option<Result<Response, ()>> {
    let end = find(buf, b"\r\n\r\n")? + 4;
    let Ok(head) = core::str::from_utf8(&buf[..end]) else {
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
        .filter(|s| s.len() == 3)
        .and_then(|s| s.parse::<u16>().ok());
    let Some(status) = status else {
        return Some(Err(()));
    };
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let Ok(n) = value.parse::<usize>() else {
                return Some(Err(()));
            };
            if content_length.is_some_and(|prev| prev != n) {
                return Some(Err(()));
            }
            content_length = Some(n);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            // Only `chunked` is supported as the final encoding.
            let last = value.rsplit(',').next().unwrap_or("").trim();
            if !last.eq_ignore_ascii_case("chunked") {
                return Some(Err(()));
            }
            chunked = true;
        }
    }
    let body = if (100..200).contains(&status) || status == 204 || status == 304 {
        BodyLength::Length(0)
    } else if chunked {
        BodyLength::Chunked
    } else {
        match content_length {
            Some(n) => BodyLength::Length(n),
            None => BodyLength::UntilClose,
        }
    };
    Some(Ok(Response {
        status,
        body,
        header_len: end,
    }))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    /// Waiting for the `size[;ext]\r\n` line.
    Size,
    /// Copying data of the current chunk.
    Data,
    /// Waiting for the `\r\n` that follows the data (bytes already seen).
    DataEnd(u8),
    /// After the size-0 chunk: trailer lines until an empty one.
    Trailer,
    Done,
}

/// Error from an invalid `chunked` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkError;

/// Incremental `Transfer-Encoding: chunked` decoder. Keeps its state
/// between calls, so chunks may be larger than the input buffer and
/// line boundaries may fall between two reads.
#[derive(Debug, Clone, Copy)]
pub struct ChunkDecoder {
    state: ChunkState,
    remaining: usize,
}

impl Default for ChunkDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkDecoder {
    pub const fn new() -> Self {
        Self {
            state: ChunkState::Size,
            remaining: 0,
        }
    }

    pub fn done(&self) -> bool {
        self.state == ChunkState::Done
    }

    /// Data of the current chunk remains to be copied: only advances if there
    /// is room in the output.
    pub fn wants_output(&self) -> bool {
        self.state == ChunkState::Data && self.remaining > 0
    }

    /// Consumes what it can from `input` and writes data into `out`. Returns
    /// `(input bytes consumed, bytes written)`. No progress means that
    /// more input is needed (or output, if `wants_output`).
    pub fn feed(&mut self, input: &[u8], out: &mut [u8]) -> Result<(usize, usize), ChunkError> {
        let mut i = 0;
        let mut written = 0;
        loop {
            match self.state {
                ChunkState::Size => {
                    let Some(rel) = find(&input[i..], b"\r\n") else {
                        if input.len() - i > MAX_CHUNK_LINE {
                            return Err(ChunkError);
                        }
                        return Ok((i, written));
                    };
                    let line = core::str::from_utf8(&input[i..i + rel]).map_err(|_| ChunkError)?;
                    let size_str = line.split(';').next().unwrap_or("").trim();
                    if size_str.is_empty() || size_str.len() > 15 {
                        return Err(ChunkError);
                    }
                    let size = usize::from_str_radix(size_str, 16).map_err(|_| ChunkError)?;
                    i += rel + 2;
                    self.remaining = size;
                    self.state = if size == 0 {
                        ChunkState::Trailer
                    } else {
                        ChunkState::Data
                    };
                }
                ChunkState::Data => {
                    let n = self.remaining.min(input.len() - i).min(out.len() - written);
                    if n == 0 {
                        return Ok((i, written));
                    }
                    out[written..written + n].copy_from_slice(&input[i..i + n]);
                    written += n;
                    i += n;
                    self.remaining -= n;
                    if self.remaining == 0 {
                        self.state = ChunkState::DataEnd(0);
                    }
                }
                ChunkState::DataEnd(seen) => {
                    let Some(&b) = input.get(i) else {
                        return Ok((i, written));
                    };
                    let expected = if seen == 0 { b'\r' } else { b'\n' };
                    if b != expected {
                        return Err(ChunkError);
                    }
                    i += 1;
                    self.state = if seen == 0 {
                        ChunkState::DataEnd(1)
                    } else {
                        ChunkState::Size
                    };
                }
                ChunkState::Trailer => {
                    let Some(rel) = find(&input[i..], b"\r\n") else {
                        if input.len() - i > MAX_CHUNK_LINE {
                            return Err(ChunkError);
                        }
                        return Ok((i, written));
                    };
                    i += rel + 2;
                    if rel == 0 {
                        self.state = ChunkState::Done;
                    }
                }
                ChunkState::Done => return Ok((i, written)),
            }
        }
    }
}

/// Sends a `GET` for `url` over `t` and leaves the body in `out`. Preceding
/// 1xx responses are consumed; a transport error close is never
/// taken for a complete download. A body larger than `out` is truncated
/// (`Fetched::truncated`); `TooBig` is only for headers that do not fit.
/// `budget` bounds the whole exchange (see [`Budget`]).
pub fn fetch<T: Transport>(
    t: &mut T,
    url: &Url,
    budget: &dyn Budget,
    out: &mut [u8],
) -> Result<Fetched, Error<T::Error>> {
    let mut body_len = 0;
    let mut truncated = false;
    let mut sink = |chunk: &[u8]| {
        let room = out.len() - body_len;
        let n = chunk.len().min(room);
        out[body_len..body_len + n].copy_from_slice(&chunk[..n]);
        body_len += n;
        truncated |= n < chunk.len();
        true
    };
    let status = fetch_with(t, url, budget, &mut sink)?;
    Ok(Fetched {
        status,
        body_len,
        truncated,
    })
}

/// Streaming `GET`: every piece of the body goes to `sink` as it arrives
/// (any size, in order); the sink returns `false` to abort (`Aborted`).
/// Returns the status code. The connection is consumed to the end of the
/// body so the caller can tell a complete download from a cut one.
/// `budget` is consulted before every transport operation (`Deadline`);
/// interim responses and header bytes are bounded as well
/// (`TooManyInformational`, `HeaderBytes`), so no phase of the exchange
/// depends on the server to end.
pub fn fetch_with<T: Transport>(
    t: &mut T,
    url: &Url,
    budget: &dyn Budget,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<u16, Error<T::Error>> {
    let mut host_buf = [0u8; MAX_HOST + 6];
    let host = url.host_header(&mut host_buf);
    let mut req = [0u8; MAX_HOST + 6 + 512];
    let n = write_get(host, url.path, &mut req).ok_or(Error::BadUrl)?;
    send(t, budget, &req[..n])?;

    // Headers: accumulate up to the empty line; skip 1xx responses, counting
    // them and the bytes they took.
    let mut head = [0u8; MAX_HEADERS];
    let mut have = 0;
    let mut informational = 0u32;
    let mut discarded = 0usize;
    let response = loop {
        match parse_response(&head[..have]) {
            Some(Err(())) => return Err(Error::Malformed),
            Some(Ok(r)) if r.is_informational() => {
                if r.status == 101 {
                    return Err(Error::Unsupported);
                }
                informational += 1;
                if informational > MAX_INFORMATIONAL {
                    return Err(Error::TooManyInformational);
                }
                discarded += r.header_len;
                head.copy_within(r.header_len..have, 0);
                have -= r.header_len;
                continue;
            }
            Some(Ok(r)) => break r,
            None => {}
        }
        if have == head.len() {
            return Err(Error::TooBig);
        }
        // Everything in `head` is header bytes of the current response (no
        // empty line yet): the fetch needs more and has used up its share.
        if discarded + have > MAX_HEADER_BYTES {
            return Err(Error::HeaderBytes);
        }
        match recv(t, budget, Phase::Headers, &mut head[have..])? {
            0 => return Err(Error::Malformed),
            k => have += k,
        }
    };
    let rest = &head[response.header_len..have];
    match response.body {
        BodyLength::Length(len) => read_length(t, budget, rest, len, sink)?,
        BodyLength::UntilClose => read_until_close(t, budget, rest, sink)?,
        BodyLength::Chunked => read_chunked(t, budget, rest, sink)?,
    }
    Ok(response.status)
}

/// `send_all` within the budget.
fn send<T: Transport>(t: &mut T, budget: &dyn Budget, data: &[u8]) -> Result<(), Error<T::Error>> {
    if budget.remaining_ms() == 0 {
        return Err(Error::Deadline(Phase::Request));
    }
    t.send_all(data)
        .map_err(|e| transport_error(budget, Phase::Request, e))
}

/// `recv` within the budget.
fn recv<T: Transport>(
    t: &mut T,
    budget: &dyn Budget,
    phase: Phase,
    out: &mut [u8],
) -> Result<usize, Error<T::Error>> {
    if budget.remaining_ms() == 0 {
        return Err(Error::Deadline(phase));
    }
    t.recv(out).map_err(|e| transport_error(budget, phase, e))
}

/// A transport failure once the budget is spent is the deadline: the
/// transport was given at most the time that was left and ran out of it.
fn transport_error<E>(budget: &dyn Budget, phase: Phase, e: E) -> Error<E> {
    if budget.remaining_ms() == 0 {
        Error::Deadline(phase)
    } else {
        Error::Transport(e)
    }
}

fn deliver<E>(sink: &mut dyn FnMut(&[u8]) -> bool, data: &[u8]) -> Result<(), Error<E>> {
    if data.is_empty() || sink(data) {
        Ok(())
    } else {
        Err(Error::Aborted)
    }
}

fn read_length<T: Transport>(
    t: &mut T,
    budget: &dyn Budget,
    rest: &[u8],
    len: usize,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<(), Error<T::Error>> {
    let first = rest.len().min(len);
    deliver(sink, &rest[..first])?;
    let mut got = first;
    let mut buf = [0u8; 1024];
    while got < len {
        let want = (len - got).min(buf.len());
        match recv(t, budget, Phase::Body, &mut buf[..want])? {
            0 => return Err(Error::Malformed),
            k => {
                deliver(sink, &buf[..k])?;
                got += k;
            }
        }
    }
    Ok(())
}

fn read_until_close<T: Transport>(
    t: &mut T,
    budget: &dyn Budget,
    rest: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<(), Error<T::Error>> {
    deliver(sink, rest)?;
    let mut buf = [0u8; 1024];
    loop {
        match recv(t, budget, Phase::Body, &mut buf)? {
            0 => return Ok(()),
            k => deliver(sink, &buf[..k])?,
        }
    }
}

fn read_chunked<T: Transport>(
    t: &mut T,
    budget: &dyn Budget,
    rest: &[u8],
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<(), Error<T::Error>> {
    let mut dec = ChunkDecoder::new();
    let mut raw = [0u8; 1024];
    let mut out = [0u8; 1024];
    let mut raw_len = 0;
    let mut pending = rest;
    loop {
        // Fill `raw` with what was left over from the headers before reading more.
        if !pending.is_empty() {
            let n = pending.len().min(raw.len() - raw_len);
            raw[raw_len..raw_len + n].copy_from_slice(&pending[..n]);
            raw_len += n;
            pending = &pending[n..];
        }
        let (used, written) = dec
            .feed(&raw[..raw_len], &mut out)
            .map_err(|_| Error::Malformed)?;
        deliver(sink, &out[..written])?;
        raw.copy_within(used..raw_len, 0);
        raw_len -= used;
        if dec.done() {
            return Ok(());
        }
        if used == 0 && written == 0 && raw_len == raw.len() {
            return Err(Error::Malformed);
        }
        if !pending.is_empty() || (used > 0 || written > 0) && raw_len > 0 {
            continue;
        }
        match recv(t, budget, Phase::Body, &mut raw[raw_len..])? {
            0 => return Err(Error::Malformed),
            k => raw_len += k,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::collections::VecDeque;
    use std::string::String;
    use std::vec;
    use std::vec::Vec;

    /// Simulated transport: delivers `parts` honoring a per-read maximum
    /// (like the real client's 1472-byte DMA buffer) and then `end`.
    struct Mock {
        incoming: VecDeque<Vec<u8>>,
        end: Result<usize, &'static str>,
        sent: Vec<u8>,
        max_read: usize,
    }

    impl Mock {
        fn new(parts: &[&[u8]]) -> Self {
            Self {
                incoming: parts.iter().map(|p| p.to_vec()).collect(),
                end: Ok(0),
                sent: Vec::new(),
                max_read: 1472,
            }
        }
    }

    impl Transport for Mock {
        type Error = &'static str;
        fn send_all(&mut self, data: &[u8]) -> Result<(), &'static str> {
            self.sent.extend_from_slice(data);
            Ok(())
        }
        fn recv(&mut self, out: &mut [u8]) -> Result<usize, &'static str> {
            if out.is_empty() {
                return Ok(0);
            }
            match self.incoming.pop_front() {
                Some(data) => {
                    let n = out.len().min(data.len()).min(self.max_read);
                    out[..n].copy_from_slice(&data[..n]);
                    if n < data.len() {
                        self.incoming.push_front(data[n..].to_vec());
                    }
                    Ok(n)
                }
                None => self.end,
            }
        }
    }

    fn get(parts: &[&[u8]], capacity: usize) -> Result<Fetched, Error<&'static str>> {
        let url = Url::parse("http://127.0.0.1/").unwrap();
        let mut out = vec![0u8; capacity];
        fetch(&mut Mock::new(parts), &url, &Unbounded, &mut out)
    }

    fn body(parts: &[&[u8]], capacity: usize) -> (Fetched, Vec<u8>) {
        let url = Url::parse("http://127.0.0.1/").unwrap();
        let mut out = vec![0u8; capacity];
        let f = fetch(&mut Mock::new(parts), &url, &Unbounded, &mut out).unwrap();
        out.truncate(f.body_len);
        (f, out)
    }

    #[test]
    fn get_request_and_url() {
        let mut buf = [0u8; 512];
        let n = write_get("10.0.2.2:17010", "/hola", &mut buf).unwrap();
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.starts_with("GET /hola HTTP/1.1\r\nHost: 10.0.2.2:17010\r\n"));
        assert!(text.ends_with("Connection: close\r\n\r\n"));
        let u = Url::parse("http://10.0.2.2:17010/hola").unwrap();
        assert_eq!(
            u,
            Url {
                host: "10.0.2.2",
                port: 17010,
                path: "/hola",
                secure: false,
            }
        );
        let mut hb = [0u8; MAX_HOST + 6];
        assert_eq!(u.host_header(&mut hb), "10.0.2.2:17010");
        let u = Url::parse("example.org").unwrap();
        assert_eq!((u.host, u.port, u.path), ("example.org", 80, "/"));
        assert_eq!(u.host_header(&mut hb), "example.org");
        let s = Url::parse("https://www.example.org/a?b").unwrap();
        assert_eq!(
            (s.host, s.port, s.path, s.secure),
            ("www.example.org", 443, "/a?b", true)
        );
        assert_eq!(s.host_header(&mut hb), "www.example.org");
        let s = Url::parse("https://h:8443").unwrap();
        assert_eq!((s.port, s.host_header(&mut hb)), (8443, "h:8443"));
        assert!(Url::parse("http://https://x").is_none());
        assert!(Url::parse("ftp://x").is_none());
        assert!(Url::parse("").is_none());
        assert!(Url::parse("host:puerto/").is_none());
        let long = std::format!("{}.test", "a".repeat(MAX_HOST));
        assert!(Url::parse(&long).is_none(), "host longer than 253 bytes");
    }

    #[test]
    fn long_hostname_is_kept_whole_in_host_header() {
        let host = std::format!("{}.{}.test", "a".repeat(40), "b".repeat(40));
        let url_text = std::format!("http://{host}/");
        let url = Url::parse(&url_text).unwrap();
        let mut t = Mock::new(&[b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"]);
        fetch(&mut t, &url, &Unbounded, &mut [0u8; 16]).unwrap();
        let sent = String::from_utf8(t.sent).unwrap();
        assert!(sent.contains(&std::format!("Host: {host}\r\n")), "{sent:?}");
    }

    #[test]
    fn response_headers_and_framing_precedence() {
        let raw = b"HTTP/1.1 200 OK\r\nServer: xtask\r\nContent-Length: 5\r\n\r\nhola!";
        let r = parse_response(raw).unwrap().unwrap();
        assert_eq!((r.status, r.body), (200, BodyLength::Length(5)));
        assert_eq!(&raw[r.header_len..], b"hola!");
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Le").is_none(),
            "incomplete"
        );
        assert_eq!(parse_response(b"SSH-2.0\r\n\r\n"), Some(Err(())));
        assert_eq!(parse_response(b"HTTP/1.1 abc\r\n\r\n"), Some(Err(())));
        let te_first =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 50\r\n\r\n";
        assert_eq!(
            parse_response(te_first).unwrap().unwrap().body,
            BodyLength::Chunked
        );
        let cl_first =
            b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\nTransfer-Encoding: Chunked\r\n\r\n";
        assert_eq!(
            parse_response(cl_first).unwrap().unwrap().body,
            BodyLength::Chunked
        );
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Some(Err(())),
            "unsupported encoding"
        );
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n"),
            Some(Err(())),
            "contradictory lengths"
        );
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n"),
            Some(Err(()))
        );
        let close = parse_response(b"HTTP/1.0 200 OK\r\n\r\n").unwrap().unwrap();
        assert_eq!(close.body, BodyLength::UntilClose);
        let no_content = parse_response(b"HTTP/1.1 204 No Content\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(no_content.body, BodyLength::Length(0));
    }

    #[test]
    fn chunk_decoder_streams_across_boundaries() {
        let mut dec = ChunkDecoder::new();
        let mut out = [0u8; 64];
        let full = b"4\r\nhola\r\n6;ext=1\r\n mundo\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let (used, written) = dec.feed(full, &mut out).unwrap();
        assert_eq!((used, written, dec.done()), (full.len(), 10, true));
        assert_eq!(&out[..10], b"hola mundo");

        // Byte by byte, as the reader does: accumulates what was not consumed and
        // feeds it again. Size lines, data and CRLFs are split anywhere.
        let mut dec = ChunkDecoder::new();
        let mut got = Vec::new();
        let mut pending: Vec<u8> = Vec::new();
        for b in full.iter() {
            pending.push(*b);
            let mut o = [0u8; 4];
            let (used, written) = dec.feed(&pending, &mut o).unwrap();
            pending.drain(..used);
            got.extend_from_slice(&o[..written]);
        }
        assert!(dec.done());
        assert!(pending.is_empty());
        assert_eq!(got, b"hola mundo");

        // Small output: the chunk is copied in parts.
        let mut dec = ChunkDecoder::new();
        let input = b"a\r\n0123456789\r\n0\r\n\r\n";
        let mut o = [0u8; 4];
        let (used, written) = dec.feed(input, &mut o).unwrap();
        assert_eq!((used, written), (7, 4));
        assert!(dec.wants_output());
        let (used2, written2) = dec.feed(&input[used..], &mut o).unwrap();
        assert_eq!((used2, written2), (4, 4));
        let (used3, written3) = dec.feed(&input[used + used2..], &mut o).unwrap();
        assert_eq!((used3, written3), (input.len() - used - used2, 2));
        assert!(dec.done());
    }

    #[test]
    fn chunk_decoder_rejects_garbage_without_panicking() {
        let mut out = [0u8; 8];
        let huge = std::format!("{:x}\r\n", usize::MAX - 10);
        let mut dec = ChunkDecoder::new();
        assert_eq!(
            dec.feed(huge.as_bytes(), &mut out),
            Err(ChunkError),
            "absurd size"
        );
        let mut dec = ChunkDecoder::new();
        assert_eq!(dec.feed(b"zz\r\n", &mut out), Err(ChunkError));
        let mut dec = ChunkDecoder::new();
        assert_eq!(
            dec.feed(b"4\r\nholaXX", &mut out),
            Err(ChunkError),
            "no CRLF after the data"
        );
        let mut dec = ChunkDecoder::new();
        let long_line = vec![b'1'; MAX_CHUNK_LINE + 1];
        assert_eq!(dec.feed(&long_line, &mut out), Err(ChunkError));
        let mut dec = ChunkDecoder::new();
        assert_eq!(
            dec.feed(b"4\r\nho", &mut out),
            Ok((5, 2)),
            "incomplete, no error"
        );
    }

    #[test]
    fn nominal_length_response() {
        let (f, b) = body(&[b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"], 5);
        assert_eq!(
            f,
            Fetched {
                status: 200,
                body_len: 5,
                truncated: false,
            }
        );
        assert_eq!(b, b"hello");
        // Body spread over several reads.
        let (f, b) = body(
            &[
                b"HTTP/1.1 200 OK\r\nContent-Len",
                b"gth: 5\r\n\r\nhe",
                b"llo",
            ],
            8,
        );
        assert_eq!((f.status, b.as_slice()), (200, &b"hello"[..]));
        assert_eq!(
            get(&[b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe"], 5),
            Err(Error::Malformed)
        );
        let (f, b) = body(&[b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nhello!"], 5);
        assert_eq!(
            (f.body_len, f.truncated, b.as_slice()),
            (5, true, &b"hello"[..])
        );
    }

    #[test]
    fn chunk_larger_than_scratch_fits_caller_buffer() {
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1000\r\n".to_vec();
        raw.extend(vec![b'x'; 4096]);
        raw.extend_from_slice(b"\r\n0\r\n\r\n");
        let (f, b) = body(&[&raw], 4096);
        assert_eq!(
            f,
            Fetched {
                status: 200,
                body_len: 4096,
                truncated: false,
            }
        );
        assert!(b.iter().all(|&c| c == b'x'));
        // And if it does not fit, the body is truncated without panicking.
        let (f, _) = body(&[&raw], 4095);
        assert_eq!((f.body_len, f.truncated), (4095, true));
        // Chunks split at line and data boundaries.
        let parts: [&[u8]; 6] = [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            b"d\r",
            b"\nprimer tro",
            b"zo,\r\ne\r\n segundo trozo\r",
            b"\n0\r\n",
            b"\r\n",
        ];
        let (f, b) = body(&parts, 64);
        assert_eq!(
            (f.status, b.as_slice()),
            (200, &b"primer trozo, segundo trozo"[..])
        );
        assert_eq!(
            get(
                &[b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nhola\r\n"],
                64
            ),
            Err(Error::Malformed),
            "close before the final chunk"
        );
    }

    #[test]
    fn close_delimited_body_exactly_fits() {
        let (f, b) = body(&[b"HTTP/1.0 200 OK\r\n\r\nhello"], 5);
        assert_eq!((f.status, b.as_slice()), (200, &b"hello"[..]));
        assert_eq!(
            get(&[b"HTTP/1.0 200 OK\r\n\r\n"], 0),
            Ok(Fetched {
                status: 200,
                body_len: 0,
                truncated: false,
            })
        );
        let (f, _) = body(&[b"HTTP/1.0 200 OK\r\n\r\nhello!"], 5);
        assert_eq!((f.body_len, f.truncated), (5, true));
        let (f, _) = body(&[b"HTTP/1.0 200 OK\r\n\r\nhello"], 6);
        assert_eq!((f.body_len, f.truncated), (5, false));
        let (f, _) = body(&[b"HTTP/1.0 200 OK\r\n\r\nhello"], 5);
        assert_eq!((f.body_len, f.truncated), (5, false));
    }

    #[test]
    fn reset_is_not_successful_completion() {
        let url = Url::parse("http://127.0.0.1/").unwrap();
        let mut t = Mock::new(&[b"HTTP/1.0 200 OK\r\n\r\npartial"]);
        t.end = Err("reset");
        assert_eq!(
            fetch(&mut t, &url, &Unbounded, &mut [0u8; 128]),
            Err(Error::Transport("reset"))
        );
        let mut t = Mock::new(&[b"HTTP/1.0 200 OK\r\n\r\nexact"]);
        t.end = Err("reset");
        assert_eq!(
            fetch(&mut t, &url, &Unbounded, &mut [0u8; 5]),
            Err(Error::Transport("reset")),
            "also when probing with the destination full"
        );
    }

    #[test]
    fn informational_responses_precede_the_final_one() {
        let raw: &[u8] = b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let (f, b) = body(&[raw], 128);
        assert_eq!(
            (f, b.as_slice()),
            (
                Fetched {
                    status: 200,
                    body_len: 5,
                    truncated: false,
                },
                &b"hello"[..]
            )
        );
        assert_eq!(
            get(
                &[b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: x\r\n\r\n"],
                8
            ),
            Err(Error::Unsupported)
        );
    }

    #[test]
    fn large_real_world_headers_fit() {
        // github.com-sized headers (5.3 KiB): a long content-security-policy,
        // several set-cookie lines and the usual rest, then a short body.
        let mut raw = b"HTTP/1.1 200 OK\r\nServer: GitHub.com\r\n".to_vec();
        raw.extend(b"Content-Security-Policy: ");
        raw.extend(vec![b'p'; 3200]);
        raw.extend(b"\r\n");
        for i in 0..6 {
            raw.extend(std::format!("Set-Cookie: c{i}=").as_bytes());
            raw.extend(vec![b'v'; 300]);
            raw.extend(b"; Path=/; Secure; HttpOnly\r\n");
        }
        for i in 0..8 {
            raw.extend(std::format!("X-Header-{i}: value\r\n").as_bytes());
        }
        raw.extend(b"Content-Length: 2\r\n\r\nok");
        assert!(raw.len() > 5300 && raw.len() < MAX_HEADERS);
        let (f, out) = body(&[&raw], 8);
        assert_eq!((f.status, out.as_slice()), (200, &b"ok"[..]));
    }

    #[test]
    fn oversized_headers_and_non_http() {
        let mut huge = b"HTTP/1.1 200 OK\r\n".to_vec();
        huge.extend(vec![b'x'; MAX_HEADERS]);
        assert_eq!(get(&[&huge], 8), Err(Error::TooBig));
        assert_eq!(get(&[b"SSH-2.0-OpenSSH\r\n\r\n"], 8), Err(Error::Malformed));
        assert_eq!(
            get(&[b"HTTP/1.1 200 OK\r\n"], 8),
            Err(Error::Malformed),
            "close in the middle of the headers"
        );
    }

    #[test]
    fn streaming_fetch_delivers_everything_and_can_abort() {
        let mut raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3000\r\n\r\n".to_vec();
        raw.extend((0..3000u32).map(|i| (i % 251) as u8));
        let mut t = Mock::new(&[&raw]);
        let mut got = Vec::new();
        let url = Url::parse("h/").unwrap();
        let status = fetch_with(&mut t, &url, &Unbounded, &mut |c: &[u8]| {
            got.extend_from_slice(c);
            true
        })
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(got, raw[raw.len() - 3000..]);
        // The sink aborts after the first piece.
        let mut t = Mock::new(&[&raw]);
        let mut pieces = 0;
        let r = fetch_with(&mut t, &url, &Unbounded, &mut |_: &[u8]| {
            pieces += 1;
            false
        });
        assert_eq!(r, Err(Error::Aborted));
        assert_eq!(pieces, 1);
    }

    // ------------------------------------------------- deadline (incident 0009)

    use std::cell::Cell;

    /// Simulated clock: a `Budget` over a deadline in simulated milliseconds.
    struct Clock {
        now: Cell<u64>,
        deadline: u64,
    }

    impl Clock {
        fn new(deadline: u64) -> Self {
            Self {
                now: Cell::new(0),
                deadline,
            }
        }
        fn advance(&self, ms: u64) {
            self.now.set(self.now.get() + ms);
        }
        fn budget(&self) -> impl Fn() -> u32 + '_ {
            move || self.deadline.saturating_sub(self.now.get()) as u32
        }
    }

    /// Server that answers the n-th receive with `reply(n)`, taking
    /// `step_ms` of simulated time per receive. Like the SDK transport
    /// bounded by the budget, it times out instead of waiting past the
    /// deadline, unless `fixed_timeout` makes it the pre-fix transport that
    /// knows nothing of the budget and always delivers.
    struct Slow<'a> {
        clock: &'a Clock,
        step_ms: u64,
        receives: u32,
        reply: &'a dyn Fn(u32) -> Vec<u8>,
        fixed_timeout: bool,
    }

    /// Receives after which a `fixed_timeout` transport declares the fetch
    /// a runaway (the test fails instead of looping).
    const RUNAWAY: u32 = 10_000;

    impl Transport for Slow<'_> {
        type Error = &'static str;
        fn send_all(&mut self, _: &[u8]) -> Result<(), &'static str> {
            Ok(())
        }
        fn recv(&mut self, out: &mut [u8]) -> Result<usize, &'static str> {
            self.receives += 1;
            if self.fixed_timeout {
                if self.receives > RUNAWAY {
                    return Err("runaway");
                }
            } else {
                let left = self.clock.deadline.saturating_sub(self.clock.now.get());
                if self.step_ms > left {
                    self.clock.advance(left);
                    return Err("timeout");
                }
            }
            self.clock.advance(self.step_ms);
            let data = (self.reply)(self.receives);
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            Ok(n)
        }
    }

    /// The parser alone ends a stall: with a transport that never times out
    /// (fixed per-operation timeout, data always queued: the pre-fix
    /// situation), the budget check before each receive stops a header
    /// dribble at the deadline after a bounded number of receives.
    #[test]
    fn parser_alone_ends_a_stall_on_a_transport_that_never_times_out() {
        let clock = Clock::new(10_000);
        let head = b"HTTP/1.1 200 OK\r\nX-Slow: yes\r\n";
        let reply = |n: u32| std::vec![head[(n as usize - 1) % head.len()]];
        let mut t = Slow {
            clock: &clock,
            step_ms: 1000,
            receives: 0,
            reply: &reply,
            fixed_timeout: true,
        };
        let budget = clock.budget();
        let url = Url::parse("http://host/index").unwrap();
        let r = fetch_with(&mut t, &url, &budget, &mut |_: &[u8]| true);
        assert_eq!(r, Err(Error::Deadline(Phase::Headers)));
        assert_eq!(t.receives, 10, "one receive per second until the deadline");
        assert_eq!(clock.now.get(), 10_000);
    }

    const EARLY_HINTS: &[u8] = b"HTTP/1.1 103 Early Hints\r\nLink: </x>; rel=preload\r\n\r\n";
    const FINAL_EMPTY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";

    /// Runs a fetch against `Slow`, returning the result, the simulated time
    /// at the end and how many times the sink was called.
    fn slow_fetch(
        clock: &Clock,
        step_ms: u64,
        reply: &dyn Fn(u32) -> Vec<u8>,
    ) -> (Result<u16, Error<&'static str>>, u64, u32) {
        let mut t = Slow {
            clock,
            step_ms,
            receives: 0,
            reply,
            fixed_timeout: false,
        };
        let mut calls = 0;
        let budget = clock.budget();
        let url = Url::parse("http://host/index").unwrap();
        let r = fetch_with(&mut t, &url, &budget, &mut |_: &[u8]| {
            calls += 1;
            true
        });
        (r, clock.now.get(), calls)
    }

    /// The audit's probe: 700 `103 Early Hints`, one second each, then an
    /// empty 200. The production parser used to finish after 701 simulated
    /// seconds with the sink never called; the interim-response limit now
    /// stops it after a few seconds.
    #[test]
    fn interim_response_flood_is_refused_long_before_the_deadline() {
        let clock = Clock::new(600_000);
        let reply = |n: u32| (if n <= 700 { EARLY_HINTS } else { FINAL_EMPTY }).to_vec();
        let (r, elapsed, calls) = slow_fetch(&clock, 1000, &reply);
        assert_eq!(r, Err(Error::TooManyInformational));
        assert_eq!(elapsed, (MAX_INFORMATIONAL as u64 + 1) * 1000);
        assert_eq!(calls, 0);
    }

    /// Interim responses spaced so that the count limit does not fire first:
    /// the budget, asked before every receive, ends the fetch at the
    /// deadline in the headers phase.
    #[test]
    fn slow_interim_responses_fail_at_the_deadline() {
        let clock = Clock::new(600_000);
        let reply = |_: u32| EARLY_HINTS.to_vec();
        let (r, elapsed, calls) = slow_fetch(&clock, 100_000, &reply);
        assert_eq!(r, Err(Error::Deadline(Phase::Headers)));
        assert_eq!(elapsed, 600_000, "not a millisecond past the deadline");
        assert_eq!(calls, 0);
        // Six interim responses were read before the deadline: fewer than
        // the count limit, so this really is the budget at work.
        assert!(600_000 / 100_000 < MAX_INFORMATIONAL as u64);
    }

    /// A header dribbled one byte per receive never reaches the sink; the
    /// deadline ends it.
    #[test]
    fn header_dribble_fails_at_the_deadline() {
        let clock = Clock::new(10_000);
        let head = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Slow: yes\r\n\r\nhello";
        let reply = |n: u32| std::vec![head[(n as usize - 1) % head.len()]];
        let (r, elapsed, calls) = slow_fetch(&clock, 1000, &reply);
        assert_eq!(r, Err(Error::Deadline(Phase::Headers)));
        assert_eq!(elapsed, 10_000);
        assert_eq!(calls, 0);
    }

    /// Chunk framing without body bytes: after the last chunk the server
    /// sends trailer lines forever. No body byte, no sink call; the budget
    /// ends it in the body phase.
    #[test]
    fn endless_chunk_trailers_fail_at_the_deadline() {
        let clock = Clock::new(30_000);
        let reply = |n: u32| {
            if n == 1 {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec()
            } else {
                std::format!("X-Trailer-{n}: still here\r\n").into_bytes()
            }
        };
        let (r, elapsed, calls) = slow_fetch(&clock, 1000, &reply);
        assert_eq!(r, Err(Error::Deadline(Phase::Body)));
        assert_eq!(elapsed, 30_000);
        assert_eq!(calls, 0);
        // A chunk size line that never ends: the line limit refuses it
        // before the deadline (framing, not time).
        let clock = Clock::new(600_000);
        let reply = |n: u32| {
            if n == 1 {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec()
            } else {
                b"1".to_vec()
            }
        };
        let (r, _, calls) = slow_fetch(&clock, 1000, &reply);
        assert_eq!((r, calls), (Err(Error::Malformed), 0));
    }

    /// A transport that times out once the budget is spent (the SDK gives it
    /// the remaining time) reports the deadline, not a network fault.
    #[test]
    fn transport_timeout_past_the_deadline_is_the_deadline() {
        let clock = Clock::new(5_000);
        // Nothing ever arrives: the first receive would outlast the budget.
        let reply = |_: u32| Vec::new();
        let (r, elapsed, calls) = slow_fetch(&clock, 20_000, &reply);
        assert_eq!(r, Err(Error::Deadline(Phase::Headers)));
        assert_eq!((elapsed, calls), (5_000, 0));
        // With time left, a transport error is still a transport error.
        let clock = Clock::new(600_000);
        let url = Url::parse("http://host/").unwrap();
        let mut t = Mock::new(&[b"HTTP/1.0 200 OK\r\n\r\npartial"]);
        t.end = Err("reset");
        let budget = clock.budget();
        assert_eq!(
            fetch(&mut t, &url, &budget, &mut [0u8; 128]),
            Err(Error::Transport("reset"))
        );
    }

    #[test]
    fn too_many_informational_responses() {
        let mut parts: Vec<&[u8]> = std::vec![EARLY_HINTS; MAX_INFORMATIONAL as usize];
        parts.push(FINAL_EMPTY);
        assert_eq!(
            get(&parts, 8),
            Ok(Fetched {
                status: 200,
                body_len: 0,
                truncated: false,
            }),
            "exactly the limit is accepted"
        );
        parts.insert(0, EARLY_HINTS);
        assert_eq!(get(&parts, 8), Err(Error::TooManyInformational));
    }

    #[test]
    fn cumulative_header_bytes_are_bounded() {
        // Each interim response fits `MAX_HEADERS` on its own, and there are
        // fewer than `MAX_INFORMATIONAL`, but together they exceed the fetch's
        // header budget.
        let mut big = b"HTTP/1.1 103 Early Hints\r\nLink: ".to_vec();
        big.extend(vec![b'l'; 3 * 1024]);
        big.extend(b"\r\n\r\n");
        assert!(big.len() < MAX_HEADERS);
        let count = MAX_HEADER_BYTES / big.len() + 1;
        assert!(count < MAX_INFORMATIONAL as usize);
        let mut parts: Vec<&[u8]> = std::vec![&big; count];
        parts.push(FINAL_EMPTY);
        assert_eq!(get(&parts, 8), Err(Error::HeaderBytes));
        // Two of them plus a large final response stay within the limit.
        let mut final_big = b"HTTP/1.1 200 OK\r\nX-Big: ".to_vec();
        final_big.extend(vec![b'b'; 7 * 1024]);
        final_big.extend(b"\r\nContent-Length: 2\r\n\r\nok");
        let (f, out) = body(&[&big, &big, &final_big], 8);
        assert_eq!((f.status, out.as_slice()), (200, &b"ok"[..]));
    }

    /// A normal fetch under budget: an interim response, headers and a body
    /// over several receives, each taking time, succeeds untouched.
    #[test]
    fn fast_fetch_within_budget_succeeds() {
        let clock = Clock::new(600_000);
        let reply = |n: u32| match n {
            1 => EARLY_HINTS.to_vec(),
            2 => b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello".to_vec(),
            _ => b" wor".to_vec(),
        };
        let mut t = Slow {
            clock: &clock,
            step_ms: 2_000,
            receives: 0,
            reply: &reply,
            fixed_timeout: false,
        };
        let budget = clock.budget();
        let url = Url::parse("http://host/index").unwrap();
        let mut out = [0u8; 16];
        let f = fetch(&mut t, &url, &budget, &mut out).unwrap();
        assert_eq!((f.status, f.body_len, f.truncated), (200, 10, false));
        assert_eq!(&out[..10], b"hello wor ");
        assert_eq!(clock.now.get(), 4 * 2_000);
    }
}
