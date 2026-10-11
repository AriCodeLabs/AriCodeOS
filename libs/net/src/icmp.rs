//! ICMP echo (RFC 792): just enough for `ping`.

use crate::{be16, checksum};

pub const HEADER_LEN: usize = 8;
pub const TYPE_ECHO_REPLY: u8 = 0;
pub const TYPE_ECHO_REQUEST: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Echo {
    pub reply: bool,
    pub identifier: u16,
    pub sequence: u16,
}

impl Echo {
    /// Parses an ICMP message; only echo request/reply with a valid
    /// checksum. Also returns the payload.
    pub fn parse(b: &[u8]) -> Option<(Echo, &[u8])> {
        if b.len() < HEADER_LEN || checksum(0, b) != 0 || b[1] != 0 {
            return None;
        }
        let reply = match b[0] {
            TYPE_ECHO_REPLY => true,
            TYPE_ECHO_REQUEST => false,
            _ => return None,
        };
        Some((
            Echo {
                reply,
                identifier: be16(&b[4..6]),
                sequence: be16(&b[6..8]),
            },
            &b[HEADER_LEN..],
        ))
    }

    /// Writes header and payload with checksum.
    pub fn write(&self, payload: &[u8], out: &mut [u8]) -> Option<usize> {
        let len = HEADER_LEN + payload.len();
        if out.len() < len {
            return None;
        }
        out[0] = if self.reply {
            TYPE_ECHO_REPLY
        } else {
            TYPE_ECHO_REQUEST
        };
        out[1] = 0;
        out[2..4].copy_from_slice(&[0, 0]);
        out[4..6].copy_from_slice(&self.identifier.to_be_bytes());
        out[6..8].copy_from_slice(&self.sequence.to_be_bytes());
        out[HEADER_LEN..len].copy_from_slice(payload);
        let sum = checksum(0, &out[..len]);
        out[2..4].copy_from_slice(&sum.to_be_bytes());
        Some(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_round_trip() {
        let e = Echo {
            reply: false,
            identifier: 0xBEEF,
            sequence: 3,
        };
        let mut buf = [0u8; 64];
        let n = e.write(b"AriCodeOS ping", &mut buf).unwrap();
        assert_eq!(n, HEADER_LEN + 14);
        let (parsed, payload) = Echo::parse(&buf[..n]).unwrap();
        assert_eq!(parsed, e);
        assert_eq!(payload, b"AriCodeOS ping");
        buf[10] ^= 1;
        assert!(Echo::parse(&buf[..n]).is_none());
    }
}
