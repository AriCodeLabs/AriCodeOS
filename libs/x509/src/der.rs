// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Minimal DER reader: single-byte tags, definite minimal lengths, and the
//! few universal types certificates use.

use crate::Error;

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTF8_STRING: u8 = 0x0c;
pub const PRINTABLE_STRING: u8 = 0x13;
pub const TELETEX_STRING: u8 = 0x14;
pub const IA5_STRING: u8 = 0x16;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// Context-specific constructed tag `[n]` (EXPLICIT tagging).
pub const fn explicit(n: u8) -> u8 {
    0xa0 | n
}

/// One element: its tag, its contents and the whole encoding.
#[derive(Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub value: &'a [u8],
    pub raw: &'a [u8],
}

/// Reads consecutive elements from a byte string.
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// Reads the next element, whatever its tag.
    pub fn any(&mut self) -> Result<Tlv<'a>, Error> {
        let d = self.data;
        let tag = *d.first().ok_or(Error::Malformed)?;
        if tag & 0x1f == 0x1f {
            return Err(Error::Malformed); // multi-byte tags never occur in certificates
        }
        let first = *d.get(1).ok_or(Error::Malformed)?;
        let (len, header) = if first < 0x80 {
            (first as usize, 2)
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 || d.len() < 2 + n || d[2] == 0 {
                return Err(Error::Malformed); // indefinite, huge or non-minimal
            }
            let len = d[2..2 + n]
                .iter()
                .fold(0usize, |acc, &b| acc << 8 | b as usize);
            if len < 0x80 {
                return Err(Error::Malformed);
            }
            (len, 2 + n)
        };
        let end = header.checked_add(len).ok_or(Error::Malformed)?;
        if end > d.len() {
            return Err(Error::Malformed);
        }
        self.data = &d[end..];
        Ok(Tlv {
            tag,
            value: &d[header..end],
            raw: &d[..end],
        })
    }

    /// Reads an element that must have tag `tag`; returns its contents.
    pub fn expect(&mut self, tag: u8) -> Result<&'a [u8], Error> {
        Ok(self.expect_tlv(tag)?.value)
    }

    pub fn expect_tlv(&mut self, tag: u8) -> Result<Tlv<'a>, Error> {
        let tlv = self.any()?;
        if tlv.tag != tag {
            return Err(Error::Malformed);
        }
        Ok(tlv)
    }

    /// Reads the next element only if it has tag `tag`.
    pub fn optional(&mut self, tag: u8) -> Result<Option<&'a [u8]>, Error> {
        if self.peek_tag() == Some(tag) {
            self.expect(tag).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Fails unless every byte has been consumed.
    pub fn finish(&self) -> Result<(), Error> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }
}

/// Parses `data` as exactly one element with tag `tag`; returns its contents.
pub fn single(data: &[u8], tag: u8) -> Result<&[u8], Error> {
    let mut r = Reader::new(data);
    let value = r.expect(tag)?;
    r.finish()?;
    Ok(value)
}

/// Contents of a non-negative INTEGER without the sign byte.
pub fn unsigned(value: &[u8]) -> Result<&[u8], Error> {
    match value {
        [] => Err(Error::Malformed),
        [b, ..] if b & 0x80 != 0 => Err(Error::Malformed),
        [0, rest @ ..] if !rest.is_empty() => {
            if rest[0] & 0x80 == 0 {
                Err(Error::Malformed) // non-minimal
            } else {
                Ok(rest)
            }
        }
        _ => Ok(value),
    }
}

/// Contents of a small non-negative INTEGER.
pub fn small_unsigned(value: &[u8]) -> Result<u32, Error> {
    let bytes = unsigned(value)?;
    if bytes.len() > 4 {
        return Err(Error::Unsupported);
    }
    Ok(bytes.iter().fold(0u32, |acc, &b| acc << 8 | b as u32))
}

/// Contents of a BOOLEAN.
pub fn boolean(value: &[u8]) -> Result<bool, Error> {
    match value {
        [0x00] => Ok(false),
        [0xff] => Ok(true),
        _ => Err(Error::Malformed),
    }
}

/// Bytes of a BIT STRING that has no unused bits (keys and signatures).
pub fn octet_aligned_bits(value: &[u8]) -> Result<&[u8], Error> {
    match value {
        [0, rest @ ..] => Ok(rest),
        _ => Err(Error::Malformed),
    }
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) as Unix
/// seconds. Times before 1970 are rejected.
pub fn time(tlv: Tlv) -> Result<u64, Error> {
    let v = tlv.value;
    let (year, rest) = match (tlv.tag, v.len()) {
        (UTC_TIME, 13) => {
            let yy = digits(&v[..2])?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &v[2..])
        }
        (GENERALIZED_TIME, 15) => (digits(&v[..4])?, &v[4..]),
        _ => return Err(Error::Malformed),
    };
    if rest[10] != b'Z' {
        return Err(Error::Malformed);
    }
    let month = digits(&rest[0..2])?;
    let day = digits(&rest[2..4])?;
    let hour = digits(&rest[4..6])?;
    let minute = digits(&rest[6..8])?;
    let second = digits(&rest[8..10])?;
    if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(Error::Malformed);
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Error::Malformed);
    }
    let days = days_from_civil(year, month, day);
    Ok(days * 86_400 + hour * 3600 + minute * 60 + second)
}

fn digits(text: &[u8]) -> Result<u64, Error> {
    text.iter().try_fold(0u64, |acc, &c| {
        if c.is_ascii_digit() {
            Ok(acc * 10 + (c - b'0') as u64)
        } else {
            Err(Error::Malformed)
        }
    })
}

fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (year >= 1970).
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let leaps = |y: u64| y / 4 - y / 100 + y / 400;
    let mut days = (year - 1970) * 365 + leaps(year - 1) - leaps(1969);
    for m in 1..month {
        days += days_in_month(year, m);
    }
    days + day - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(raw: &[u8]) -> Tlv<'_> {
        Reader::new(raw).any().unwrap()
    }

    #[test]
    fn lengths() {
        let mut long = [0u8; 3 + 200];
        long[..3].copy_from_slice(&[0x04, 0x81, 200]);
        let t = tlv(&long);
        assert_eq!((t.tag, t.value.len(), t.raw.len()), (0x04, 200, 203));
        // Non-minimal long form, indefinite length, truncated, multi-byte tag.
        for bad in [
            &[0x04, 0x81, 0x05, 0, 0, 0, 0, 0][..],
            &[0x30, 0x80, 0, 0],
            &[0x04, 0x03, 0],
            &[0x1f, 0x01, 0x00],
            &[0x04, 0x82, 0x00, 0x90],
        ] {
            assert_eq!(
                Reader::new(bad).any().err(),
                Some(Error::Malformed),
                "{bad:?}"
            );
        }
        let mut r = Reader::new(&[0x05, 0x00, 0x02, 0x01, 0x07]);
        assert_eq!(r.optional(INTEGER).unwrap(), None);
        assert_eq!(r.expect(NULL).unwrap(), &[] as &[u8]);
        assert!(r.finish().is_err());
        assert_eq!(r.expect(INTEGER).unwrap(), &[7]);
        assert!(r.finish().is_ok() && r.is_empty());
    }

    #[test]
    fn integers_and_booleans() {
        assert_eq!(unsigned(&[0x00, 0x80]).unwrap(), &[0x80]);
        assert_eq!(unsigned(&[0x00]).unwrap(), &[0x00]);
        assert_eq!(unsigned(&[0x01, 0x00, 0x01]).unwrap(), &[1, 0, 1]);
        assert!(unsigned(&[0x80]).is_err() && unsigned(&[]).is_err() && unsigned(&[0, 1]).is_err());
        assert_eq!(small_unsigned(&[0x01, 0x00]).unwrap(), 256);
        assert_eq!(small_unsigned(&[1, 2, 3, 4, 5]), Err(Error::Unsupported));
        assert!(boolean(&[0xff]).unwrap() && !boolean(&[0]).unwrap() && boolean(&[1]).is_err());
        assert_eq!(octet_aligned_bits(&[0, 9]).unwrap(), &[9]);
        assert!(octet_aligned_bits(&[3, 8]).is_err());
    }

    #[test]
    fn times() {
        // Values cross-checked with `date -u -d ... +%s`.
        assert_eq!(time(tlv(b"\x17\x0d700101000000Z")).unwrap(), 0);
        assert_eq!(time(tlv(b"\x17\x0d260101000000Z")).unwrap(), 1_767_225_600);
        assert_eq!(time(tlv(b"\x17\x0d000229235959Z")).unwrap(), 951_868_799);
        assert_eq!(
            time(tlv(b"\x18\x0f20550101000000Z")).unwrap(),
            2_682_374_400
        );
        assert_eq!(time(tlv(b"\x17\x0d991231235959Z")).unwrap(), 946_684_799);
        for bad in [
            &b"\x17\x0d010229000000Z"[..],
            b"\x17\x0d260101000000+",
            b"\x17\x0b2601010000Z",
            b"\x18\x0f19600101000000Z",
            b"\x17\x0d26010100006aZ",
            b"\x04\x0d260101000000Z",
        ] {
            assert_eq!(time(tlv(bad)), Err(Error::Malformed), "{bad:?}");
        }
    }
}
