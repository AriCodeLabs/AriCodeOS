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

    #[test]
    fn empty_image() {
        let img = [b'A', b'R', b'I', b'R', b'D', 1, 0, 0];
        let rd = Initrd::parse(&img).unwrap();
        assert!(rd.is_empty());
    }
}
