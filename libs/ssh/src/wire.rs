//! SSH wire encoding (RFC 4251 section 5): bytes, uint32, booleans,
//! strings, name-lists and mpints.

use alloc::vec::Vec;

#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new(msg: u8) -> Self {
        Self(alloc::vec![msg])
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    pub fn string(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32).bytes(v)
    }
    /// Unsigned big-endian integer as an mpint: no leading zeros, and a
    /// zero byte in front if the top bit is set.
    pub fn mpint(&mut self, v: &[u8]) -> &mut Self {
        let v = &v[v.iter().position(|&b| b != 0).unwrap_or(v.len())..];
        if v.first().is_some_and(|b| b & 0x80 != 0) {
            self.u32(v.len() as u32 + 1).u8(0).bytes(v)
        } else {
            self.string(v)
        }
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    pub fn bool(&mut self) -> Option<bool> {
        self.u8().map(|b| b != 0)
    }
    pub fn u32(&mut self) -> Option<u32> {
        let b = self.data.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_be_bytes(b.try_into().ok()?))
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(b)
    }
    pub fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }
}

/// Whether the comma-separated `list` contains `name`.
pub fn list_has(list: &[u8], name: &[u8]) -> bool {
    list.split(|&b| b == b',').any(|n| n == name)
}

/// First name of `client` (in its order of preference) that `server` offers.
pub fn negotiate<'a>(client: &'a [u8], server: &[&[u8]]) -> Option<&'a [u8]> {
    client
        .split(|&b| b == b',')
        .find(|c| server.iter().any(|s| s == c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_round_trip() {
        let mut w = Writer::new(7);
        w.u32(0xdead_beef)
            .bool(true)
            .string(b"ssh")
            .mpint(&[0, 0, 0x80, 1])
            .mpint(&[0, 0x7f])
            .mpint(&[0, 0]);
        let d = &w.0;
        assert_eq!(d[0], 7);
        let mut r = Reader::new(&d[1..]);
        assert_eq!(r.u32(), Some(0xdead_beef));
        assert_eq!(r.bool(), Some(true));
        assert_eq!(r.string(), Some(&b"ssh"[..]));
        assert_eq!(
            r.string(),
            Some(&[0, 0x80, 1][..]),
            "mpint gets a zero byte before a high bit"
        );
        assert_eq!(r.string(), Some(&[0x7f][..]));
        assert_eq!(r.string(), Some(&[][..]), "zero is the empty mpint");
        assert_eq!(r.u8(), None);
        assert_eq!(
            Reader::new(&[0, 0, 0, 9, 1]).string(),
            None,
            "truncated string"
        );
    }

    #[test]
    fn name_lists() {
        assert!(list_has(b"a,bb,c", b"bb"));
        assert!(!list_has(b"a,bb,c", b"b"));
        assert_eq!(
            negotiate(b"x,curve25519-sha256,y", &[b"y", b"curve25519-sha256"]),
            Some(&b"curve25519-sha256"[..])
        );
        assert_eq!(negotiate(b"x", &[b"y"]), None);
    }
}
