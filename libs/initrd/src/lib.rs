// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriCodeOS initrd (Phase 15): flat archive of named files.
//!
//! ```text
//! header     "ARIRD\x01"  (6 bytes)  count: u16 LE
//! entry      name_len: u16 LE | name (UTF-8) | data_len: u32 LE | pad to 8 | data | pad to 8
//! ```
//!
//! The reader copies nothing: it returns slices over the image. The data of
//! each file starts 8-byte aligned within the image; since the
//! kernel loads the image page aligned, every ELF ends up 8-byte aligned.

#![no_std]

#[cfg(feature = "write")]
extern crate std;

const MAGIC: &[u8; 6] = b"ARIRD\x01";
const ALIGN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitrdError {
    BadMagic,
    Truncated,
    BadName,
}

pub struct Initrd<'a> {
    data: &'a [u8],
    count: usize,
}

pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

const fn align_up(n: usize) -> usize {
    (n + ALIGN - 1) & !(ALIGN - 1)
}

impl<'a> Initrd<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, InitrdError> {
        if data.len() < 8 {
            return Err(InitrdError::Truncated);
        }
        if &data[..6] != MAGIC {
            return Err(InitrdError::BadMagic);
        }
        let count = u16::from_le_bytes([data[6], data[7]]) as usize;
        let initrd = Self { data, count };
        // Validate the whole structure once.
        let mut n = 0;
        for e in initrd.entries() {
            e?;
            n += 1;
        }
        if n != count {
            return Err(InitrdError::Truncated);
        }
        Ok(initrd)
    }

    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn entries(&self) -> Entries<'a> {
        Entries {
            data: self.data,
            offset: 8,
            remaining: self.count,
        }
    }

    pub fn find(&self, name: &str) -> Option<&'a [u8]> {
        self.entries()
            .filter_map(Result::ok)
            .find(|e| e.name == name)
            .map(|e| e.data)
    }
}

pub struct Entries<'a> {
    data: &'a [u8],
    offset: usize,
    remaining: usize,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<Entry<'a>, InitrdError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some(self.read_entry().inspect_err(|_| self.remaining = 0))
    }
}

impl<'a> Entries<'a> {
    fn read_entry(&mut self) -> Result<Entry<'a>, InitrdError> {
        let d = self.data;
        let get = |range: core::ops::Range<usize>| d.get(range).ok_or(InitrdError::Truncated);
        let name_len =
            u16::from_le_bytes(get(self.offset..self.offset + 2)?.try_into().unwrap()) as usize;
        let name = core::str::from_utf8(get(self.offset + 2..self.offset + 2 + name_len)?)
            .map_err(|_| InitrdError::BadName)?;
        if name.is_empty() || name.contains('/') {
            return Err(InitrdError::BadName);
        }
        let p = self.offset + 2 + name_len;
        let data_len = u32::from_le_bytes(get(p..p + 4)?.try_into().unwrap()) as usize;
        let start = align_up(p + 4);
        let data = get(start..start + data_len)?;
        self.offset = align_up(start + data_len);
        Ok(Entry { name, data })
    }
}

/// Where an entry's data lies in the archive: offset and length.
pub type Span = (usize, usize);

/// Walks an archive of `len` bytes read piece by piece through
/// `read(offset, buf)`, which fills `buf` from `offset`, for archives too
/// large to hold in memory (incident 0021). It checks the structure exactly
/// as [`Initrd::parse`] does (same accepted archives) and returns, for each
/// name in `wanted`, the data of the first entry with that name, as
/// [`Initrd::find`] would. Only the header and entry headers are read.
pub fn locate<const N: usize>(
    len: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> Result<(), InitrdError>,
    wanted: [&str; N],
) -> Result<[Option<Span>; N], InitrdError> {
    let mut fetch = |offset: usize, buf: &mut [u8]| {
        if offset.checked_add(buf.len()).is_none_or(|end| end > len) {
            return Err(InitrdError::Truncated);
        }
        read(offset, buf)
    };
    if len < 8 {
        return Err(InitrdError::Truncated);
    }
    let mut head = [0u8; 8];
    fetch(0, &mut head)?;
    if &head[..6] != MAGIC {
        return Err(InitrdError::BadMagic);
    }
    let count = u16::from_le_bytes([head[6], head[7]]) as usize;
    let mut found = [None; N];
    let mut offset = 8;
    for _ in 0..count {
        let mut word = [0u8; 2];
        fetch(offset, &mut word)?;
        let name_len = u16::from_le_bytes(word) as usize;
        let name_start = offset + 2;
        // The checks in `Entries::read_entry`'s order: the name's bytes,
        // then UTF-8, then empty or with '/', then the data length and data.
        if name_start + name_len > len {
            return Err(InitrdError::Truncated);
        }
        let mut same = [false; N];
        for (s, w) in same.iter_mut().zip(wanted) {
            *s = w.len() == name_len;
        }
        // UTF-8 checked piece by piece: an incomplete sequence at the end of
        // a piece is carried into the next one (at most 3 bytes).
        let mut piece = [0u8; 64 + 3];
        let mut carried = 0;
        let mut done = 0;
        let mut slash = false;
        while done < name_len {
            let n = (name_len - done).min(64);
            fetch(name_start + done, &mut piece[carried..carried + n])?;
            let bytes = &piece[carried..carried + n];
            slash |= bytes.contains(&b'/');
            for (s, w) in same.iter_mut().zip(wanted) {
                *s = *s && &w.as_bytes()[done..done + n] == bytes;
            }
            let total = carried + n;
            carried = match core::str::from_utf8(&piece[..total]) {
                Ok(_) => 0,
                Err(e) if e.error_len().is_none() => total - e.valid_up_to(),
                Err(_) => return Err(InitrdError::BadName),
            };
            piece.copy_within(total - carried..total, 0);
            done += n;
        }
        if carried != 0 || name_len == 0 || slash {
            return Err(InitrdError::BadName);
        }
        let p = name_start + name_len;
        let mut word = [0u8; 4];
        fetch(p, &mut word)?;
        let data_len = u32::from_le_bytes(word) as usize;
        let start = align_up(p + 4);
        if start + data_len > len {
            return Err(InitrdError::Truncated);
        }
        for (slot, s) in found.iter_mut().zip(same) {
            if s && slot.is_none() {
                *slot = Some((start, data_len));
            }
        }
        offset = align_up(start + data_len);
    }
    Ok(found)
}

/// Writer for host tools.
#[cfg(feature = "write")]
pub fn build(files: &[(&str, &[u8])]) -> Result<std::vec::Vec<u8>, InitrdError> {
    use std::vec::Vec;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    for (name, data) in files {
        if name.is_empty() || name.contains('/') || name.len() > u16::MAX as usize {
            return Err(InitrdError::BadName);
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.resize(align_up(out.len()), 0);
        out.extend_from_slice(data);
        out.resize(align_up(out.len()), 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[cfg(feature = "write")]
    #[test]
    fn roundtrip_and_alignment() {
        let img = build(&[
            ("init", b"\x7FELF"),
            ("hello", b"abcdefghijk"),
            ("empty", b""),
        ])
        .unwrap();
        let rd = Initrd::parse(&img).unwrap();
        assert_eq!(rd.len(), 3);
        assert_eq!(rd.find("init"), Some(&b"\x7FELF"[..]));
        assert_eq!(rd.find("hello"), Some(&b"abcdefghijk"[..]));
        assert_eq!(rd.find("empty"), Some(&b""[..]));
        assert_eq!(rd.find("nothing"), None);
        for e in rd.entries() {
            let e = e.unwrap();
            let off = e.data.as_ptr() as usize - img.as_ptr() as usize;
            assert_eq!(off % 8, 0, "{} not aligned", e.name);
        }
    }

    #[cfg(feature = "write")]
    #[test]
    fn corrupt_images_are_rejected() {
        let img = build(&[("a", b"12345678")]).unwrap();
        assert_eq!(
            Initrd::parse(&img[..img.len() - 1]).err(),
            Some(InitrdError::Truncated)
        );
        let mut bad = img.clone();
        bad[0] = b'X';
        assert_eq!(Initrd::parse(&bad).err(), Some(InitrdError::BadMagic));
        let mut more = img.clone();
        more[6] = 2; // announces two entries, there is one
        assert_eq!(Initrd::parse(&more).err(), Some(InitrdError::Truncated));
        assert_eq!(
            build(&[("with/slash", b"")]).err(),
            Some(InitrdError::BadName)
        );
        assert!(Initrd::parse(b"ARI").is_err());
    }

    /// `locate` over `img` read in pieces of at most `piece` bytes.
    fn locate_in<const N: usize>(
        img: &[u8],
        piece: usize,
        wanted: [&str; N],
    ) -> Result<[Option<Span>; N], InitrdError> {
        locate(
            img.len(),
            |offset, buf| {
                for (i, chunk) in buf.chunks_mut(piece).enumerate() {
                    let o = offset + i * piece;
                    chunk.copy_from_slice(&img[o..o + chunk.len()]);
                }
                Ok(())
            },
            wanted,
        )
    }

    /// What `parse` + `find` say about `img`, as spans.
    fn parsed<const N: usize>(img: &[u8], wanted: [&str; N]) -> Option<[Option<Span>; N]> {
        let rd = Initrd::parse(img).ok()?;
        Some(wanted.map(|w| {
            rd.find(w)
                .map(|d| (d.as_ptr() as usize - img.as_ptr() as usize, d.len()))
        }))
    }

    #[cfg(feature = "write")]
    #[test]
    fn locate_agrees_with_parse_on_damaged_archives() {
        use std::vec::Vec;
        const WANTED: [&str; 4] = ["a", "ñandú", "dup", "nope"];
        let long = "x".repeat(130);
        let base = build(&[
            ("a", b"12345"),
            ("ñandú", b"0123456789abcdef0"),
            ("dup", b"first"),
            (&long, b""),
            ("dup", b"second"),
        ])
        .unwrap();
        // A deterministic generator: damage one to three bytes, or cut.
        let mut seed = 0x2021_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut accepted = 0;
        for round in 0..6000 {
            let mut img: Vec<u8> = base.clone();
            if round % 7 == 0 {
                img.truncate(next() as usize % (base.len() + 1));
            } else {
                for _ in 0..1 + next() % 3 {
                    let i = next() as usize % img.len();
                    img[i] = next() as u8;
                }
            }
            let expected = parsed(&img, WANTED);
            for piece in [1, 3, 64, 4096] {
                assert_eq!(
                    locate_in(&img, piece, WANTED).ok(),
                    expected,
                    "round {round}, pieces of {piece}"
                );
            }
            accepted += expected.is_some() as u32;
        }
        assert!(
            accepted > 100,
            "too few damaged archives still valid: {accepted}"
        );
        let spans = locate_in(&base, 5, WANTED).unwrap();
        assert_eq!(spans[2].map(|(o, l)| &base[o..o + l]), Some(&b"first"[..]));
        assert_eq!(spans[3], None);
    }

    #[test]
    fn empty_image() {
        let img = [b'A', b'R', b'I', b'R', b'D', 1, 0, 0];
        let rd = Initrd::parse(&img).unwrap();
        assert!(rd.is_empty());
    }
}
