//! DNS (RFC 1035): A query and reading of the first A answer.

use crate::{be16, Ipv4Addr};

pub const PORT: u16 = 53;
pub const HEADER_LEN: usize = 12;
const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
pub const MAX_NAME: usize = 253;

/// Writes a recursive A query for `name` (`a.b.c`, without a trailing dot).
pub fn write_query(id: u16, name: &str, out: &mut [u8]) -> Option<usize> {
    if name.is_empty() || name.len() > MAX_NAME || out.len() < HEADER_LEN + name.len() + 2 + 4 {
        return None;
    }
    out[..HEADER_LEN].fill(0);
    out[0..2].copy_from_slice(&id.to_be_bytes());
    out[2] = 0x01; // RD
    out[5] = 1; // QDCOUNT
    let mut i = HEADER_LEN;
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        out[i] = label.len() as u8;
        out[i + 1..i + 1 + label.len()].copy_from_slice(label.as_bytes());
        i += 1 + label.len();
    }
    out[i] = 0;
    i += 1;
    out[i..i + 2].copy_from_slice(&TYPE_A.to_be_bytes());
    out[i + 2..i + 4].copy_from_slice(&CLASS_IN.to_be_bytes());
    Some(i + 4)
}

/// Skips a name (with or without compression) and returns the next position.
fn skip_name(b: &[u8], mut i: usize) -> Option<usize> {
    let mut hops = 0;
    loop {
        let len = *b.get(i)?;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += 1 + len as usize;
        hops += 1;
        if hops > 128 {
            return None;
        }
    }
}

/// Result of a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// First A address and its TTL.
    Address(Ipv4Addr, u32),
    /// The server answered with an error (`rcode`) or without A records.
    NoAddress(u8),
}

/// Parses a response to query `id`.
pub fn parse_response(id: u16, b: &[u8]) -> Option<Answer> {
    if b.len() < HEADER_LEN || be16(&b[0..2]) != id || b[2] & 0x80 == 0 {
        return None;
    }
    let rcode = b[3] & 0x0F;
    let qd = be16(&b[4..6]) as usize;
    let an = be16(&b[6..8]) as usize;
    let mut i = HEADER_LEN;
    for _ in 0..qd {
        i = skip_name(b, i)? + 4;
    }
    for _ in 0..an {
        i = skip_name(b, i)?;
        let rtype = be16(b.get(i..i + 2)?);
        let ttl = u32::from_be_bytes(b.get(i + 4..i + 8)?.try_into().ok()?);
        let rdlen = be16(b.get(i + 8..i + 10)?) as usize;
        let rdata = b.get(i + 10..i + 10 + rdlen)?;
        if rtype == TYPE_A && rdlen == 4 {
            let mut a = [0u8; 4];
            a.copy_from_slice(rdata);
            return Some(Answer::Address(Ipv4Addr(a), ttl));
        }
        i += 10 + rdlen;
    }
    Some(Answer::NoAddress(rcode))
}

/// Builds a response with a single A record for query `query`
/// (useful for test servers). Returns the length.
pub fn write_answer(query: &[u8], address: Ipv4Addr, ttl: u32, out: &mut [u8]) -> Option<usize> {
    if query.len() < HEADER_LEN + 5 {
        return None;
    }
    let qend = skip_name(query, HEADER_LEN)? + 4;
    let len = qend + 2 + 10 + 4;
    if out.len() < len || query.len() < qend {
        return None;
    }
    out[..qend].copy_from_slice(&query[..qend]);
    out[2] = 0x81; // QR, RD
    out[3] = 0x80; // RA
    out[6..8].copy_from_slice(&1u16.to_be_bytes()); // ANCOUNT
    out[8..12].fill(0);
    let mut i = qend;
    out[i..i + 2].copy_from_slice(&0xC00Cu16.to_be_bytes()); // pointer to the name
    i += 2;
    out[i..i + 2].copy_from_slice(&TYPE_A.to_be_bytes());
    out[i + 2..i + 4].copy_from_slice(&CLASS_IN.to_be_bytes());
    out[i + 4..i + 8].copy_from_slice(&ttl.to_be_bytes());
    out[i + 8..i + 10].copy_from_slice(&4u16.to_be_bytes());
    out[i + 10..i + 14].copy_from_slice(&address.0);
    Some(len)
}

/// Name asked in a query (for test servers and traces).
pub fn query_name<'a>(query: &[u8], out: &'a mut [u8; MAX_NAME]) -> Option<&'a str> {
    let mut i = HEADER_LEN;
    let mut n = 0;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        if n > 0 {
            *out.get_mut(n)? = b'.';
            n += 1;
        }
        let label = query.get(i + 1..i + 1 + len)?;
        out.get_mut(n..n + len)?.copy_from_slice(label);
        n += len;
        i += 1 + len;
    }
    core::str::from_utf8(&out[..n]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_answer_round_trip() {
        let mut q = [0u8; 128];
        let n = write_query(0x1234, "aricodeos.test", &mut q).unwrap();
        assert_eq!(&q[HEADER_LEN..HEADER_LEN + 10], b"\x09aricodeos");
        assert_eq!(&q[HEADER_LEN + 10..HEADER_LEN + 15], b"\x04test");
        let mut name = [0u8; MAX_NAME];
        assert_eq!(query_name(&q[..n], &mut name), Some("aricodeos.test"));
        let mut a = [0u8; 256];
        let m = write_answer(&q[..n], Ipv4Addr::new(10, 1, 2, 3), 300, &mut a).unwrap();
        assert_eq!(
            parse_response(0x1234, &a[..m]),
            Some(Answer::Address(Ipv4Addr::new(10, 1, 2, 3), 300))
        );
        assert!(parse_response(0x1235, &a[..m]).is_none(), "another id");
        assert!(
            parse_response(0x1234, &q[..n]).is_none(),
            "a query is not a response"
        );
        // NXDOMAIN without answers.
        let mut nx = a;
        nx[3] = 0x83;
        nx[6..8].copy_from_slice(&[0, 0]);
        assert_eq!(parse_response(0x1234, &nx[..m]), Some(Answer::NoAddress(3)));
        assert!(write_query(1, "", &mut q).is_none());
        assert!(write_query(1, "a..b", &mut q).is_none());
    }
}
