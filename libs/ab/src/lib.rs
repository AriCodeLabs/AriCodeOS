// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! A/B update (Phase 30). Three pure pieces, tested on the host and used
//! by AriBoot, init, the updater and xtask:
//!
//! - [`BootState`]: sector 0 of the `ARIBOOT` partition (active slot,
//!   pending slot with a try counter, CRC).
//! - [`Image`]: a system image in a slot partition: an `ARISLOT` header
//!   with the length, followed by an `aricode-initrd` container
//!   with `manifest`, `manifest.sig`, `kernel.elf` and `initrd.img`. The
//!   manifest carries the version and the SHA-256 of kernel and initrd; the
//!   Ed25519 signature covers the whole manifest.
//! - [`choose`]/[`confirm`]: the boot policy: a pending slot is tried
//!   while it has tries left; if they run out or its image does not
//!   verify, boot falls back to the active one (rollback); the booted system
//!   confirms and the pending slot becomes active.

#![no_std]

use aricode_crypto::{ed25519, sha256, unhex};
use aricode_initrd::Initrd;

pub const STATE_MAGIC: &[u8; 8] = b"ARIBOOT\0";
pub const IMAGE_MAGIC: &[u8; 8] = b"ARISLOT\0";
pub const SECTOR: usize = 512;
/// Tries granted to a freshly installed slot.
pub const DEFAULT_TRIES: u8 = 3;
/// Partition names the loader looks for.
pub const PART_STATE: &str = "ARIBOOT";
pub const PART_SLOT_A: &str = "SYSA";
pub const PART_SLOT_B: &str = "SYSB";
pub const PART_UPDATE: &str = "UPDATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "A",
            Slot::B => "B",
        }
    }
    pub fn partition(self) -> &'static str {
        match self {
            Slot::A => PART_SLOT_A,
            Slot::B => PART_SLOT_B,
        }
    }
    fn code(self) -> u8 {
        match self {
            Slot::A => 0,
            Slot::B => 1,
        }
    }
    fn from_code(c: u8) -> Option<Slot> {
        match c {
            0 => Some(Slot::A),
            1 => Some(Slot::B),
            _ => None,
        }
    }
}

/// Boot state (sector 0 of `ARIBOOT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootState {
    pub active: Slot,
    /// Slot installed and awaiting confirmation, with its remaining tries.
    pub pending: Option<(Slot, u8)>,
    /// Boots counted by the loader.
    pub boot_count: u32,
}

impl BootState {
    pub const fn fresh(active: Slot) -> Self {
        Self {
            active,
            pending: None,
            boot_count: 0,
        }
    }

    pub fn encode(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(STATE_MAGIC);
        s[8] = 1; // format version
        s[9] = self.active.code();
        match self.pending {
            Some((slot, tries)) => {
                s[10] = slot.code();
                s[11] = tries;
            }
            None => {
                s[10] = 0xFF;
                s[11] = 0;
            }
        }
        s[12..16].copy_from_slice(&self.boot_count.to_le_bytes());
        let crc = aricode_crypto::sha256::sha256(&s[..60]);
        s[60..64].copy_from_slice(&crc[..4]);
        s
    }

    /// `None` if the sector is not a valid state (magic, version or CRC).
    pub fn decode(s: &[u8]) -> Option<Self> {
        if s.len() < 64 || &s[..8] != STATE_MAGIC || s[8] != 1 {
            return None;
        }
        let crc = aricode_crypto::sha256::sha256(&s[..60]);
        if s[60..64] != crc[..4] {
            return None;
        }
        let active = Slot::from_code(s[9])?;
        let pending = if s[10] == 0xFF {
            None
        } else {
            Some((Slot::from_code(s[10])?, s[11]))
        };
        Some(Self {
            active,
            pending,
            boot_count: u32::from_le_bytes([s[12], s[13], s[14], s[15]]),
        })
    }
}

/// Which slot to boot and which state to write before doing so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub slot: Slot,
    /// `true` if the booted slot awaits confirmation.
    pub pending: bool,
    /// State to write to `ARIBOOT` before jumping to the kernel.
    pub state: BootState,
    /// Human-readable reason (for the loader log).
    pub reason: &'static str,
}

/// Boot policy: consumes one try of the pending slot or falls back to the
/// active one when they run out.
pub fn choose(state: BootState) -> Decision {
    let mut next = state;
    next.boot_count = state.boot_count.wrapping_add(1);
    match state.pending {
        Some((slot, tries)) if tries > 0 => {
            next.pending = Some((slot, tries - 1));
            Decision {
                slot,
                pending: true,
                state: next,
                reason: "pending slot with tries left",
            }
        }
        Some((_, _)) => {
            next.pending = None;
            Decision {
                slot: state.active,
                pending: false,
                state: next,
                reason: "pending slot exhausted: rollback to active",
            }
        }
        None => Decision {
            slot: state.active,
            pending: false,
            state: next,
            reason: "active slot",
        },
    }
}

/// If the image of the chosen slot does not verify, the loader drops the
/// pending slot and falls back to the active one.
pub fn reject(state: BootState) -> Decision {
    let mut next = state;
    next.pending = None;
    Decision {
        slot: state.active,
        pending: false,
        state: next,
        reason: "pending slot image rejected: rollback to active",
    }
}

/// The consumed try of the pending slot could not be saved: without a
/// persisted counter the pending slot is not booted (it could repeat forever);
/// boot falls back to the active one and the pending slot is kept for when the disk writes again.
pub fn unpersisted(state: BootState) -> Decision {
    Decision {
        slot: state.active,
        pending: false,
        state,
        reason: "pending try not persisted: booting active",
    }
}

/// The system booted from `slot` works: it becomes active.
pub fn confirm(state: BootState, slot: Slot) -> BootState {
    BootState {
        active: slot,
        pending: None,
        boot_count: state.boot_count,
    }
}

/// State after installing a new image in the inactive slot.
pub fn schedule(state: BootState, slot: Slot) -> BootState {
    BootState {
        active: state.active,
        pending: Some((slot, DEFAULT_TRIES)),
        boot_count: state.boot_count,
    }
}

// ---------------------------------------------------------------- images

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    BadHeader,
    BadContainer,
    MissingFile(&'static str),
    BadManifest,
    BadSignature,
    HashMismatch(&'static str),
}

/// Parsed manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub kernel_sha256: [u8; 32],
    pub initrd_sha256: [u8; 32],
}

impl Manifest {
    pub fn parse(text: &str) -> Option<Self> {
        let (mut version, mut kernel, mut initrd) = (None, None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            match k.trim() {
                "version" => version = Some(v.parse().ok()?),
                "kernel" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    kernel = Some(h);
                }
                "initrd" => {
                    let mut h = [0u8; 32];
                    unhex(v, &mut h)?;
                    initrd = Some(h);
                }
                _ => return None,
            }
        }
        Some(Self {
            version: version?,
            kernel_sha256: kernel?,
            initrd_sha256: initrd?,
        })
    }

    /// Canonical manifest text (what gets signed).
    pub fn write<'a>(&self, out: &'a mut [u8; 192]) -> &'a str {
        let mut n = 0;
        let mut put = |s: &[u8], n: &mut usize| {
            out[*n..*n + s.len()].copy_from_slice(s);
            *n += s.len();
        };
        put(b"version = ", &mut n);
        let mut num = [0u8; 10];
        let mut v = self.version;
        let mut i = num.len();
        loop {
            i -= 1;
            num[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        put(&num[i..], &mut n);
        put(b"\nkernel = ", &mut n);
        let mut hex = [0u8; 64];
        put(
            aricode_crypto::hex(&self.kernel_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\ninitrd = ", &mut n);
        put(
            aricode_crypto::hex(&self.initrd_sha256, &mut hex).as_bytes(),
            &mut n,
        );
        put(b"\n", &mut n);
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

/// Verified image: references to the bytes inside the container.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    pub manifest: Manifest,
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
}

pub struct Image;

impl Image {
    /// Slot header: magic and length of the container that follows.
    pub fn header(container_len: u32) -> [u8; SECTOR] {
        let mut h = [0u8; SECTOR];
        h[..8].copy_from_slice(IMAGE_MAGIC);
        h[8..12].copy_from_slice(&container_len.to_le_bytes());
        h
    }

    /// Container length according to the slot header.
    pub fn container_len(header: &[u8]) -> Result<u32, ImageError> {
        if header.len() < 12 || &header[..8] != IMAGE_MAGIC {
            return Err(ImageError::BadHeader);
        }
        let len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        if len == 0 {
            return Err(ImageError::BadHeader);
        }
        Ok(len)
    }

    /// Verifies a container: manifest signature with `pubkey` and hashes of
    /// kernel and initrd. Only returns data if everything matches.
    pub fn verify<'a>(container: &'a [u8], pubkey: &[u8; 32]) -> Result<Verified<'a>, ImageError> {
        let rd = Initrd::parse(container).map_err(|_| ImageError::BadContainer)?;
        let manifest_bytes = rd
            .find("manifest")
            .ok_or(ImageError::MissingFile("manifest"))?;
        let sig = rd
            .find("manifest.sig")
            .ok_or(ImageError::MissingFile("manifest.sig"))?;
        let kernel = rd
            .find("kernel.elf")
            .ok_or(ImageError::MissingFile("kernel.elf"))?;
        let initrd = rd
            .find("initrd.img")
            .ok_or(ImageError::MissingFile("initrd.img"))?;
        if sig.len() != ed25519::SIGNATURE_LEN {
            return Err(ImageError::BadSignature);
        }
        let mut signature = [0u8; 64];
        signature.copy_from_slice(sig);
        if !ed25519::verify(pubkey, manifest_bytes, &signature) {
            return Err(ImageError::BadSignature);
        }
        let text = core::str::from_utf8(manifest_bytes).map_err(|_| ImageError::BadManifest)?;
        let manifest = Manifest::parse(text).ok_or(ImageError::BadManifest)?;
        if sha256::sha256(kernel) != manifest.kernel_sha256 {
            return Err(ImageError::HashMismatch("kernel.elf"));
        }
        if sha256::sha256(initrd) != manifest.initrd_sha256 {
            return Err(ImageError::HashMismatch("initrd.img"));
        }
        Ok(Verified {
            manifest,
            kernel,
            initrd,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn state_round_trip_and_crc() {
        let s = BootState {
            active: Slot::A,
            pending: Some((Slot::B, 2)),
            boot_count: 41,
        };
        let sector = s.encode();
        assert_eq!(BootState::decode(&sector), Some(s));
        let mut bad = sector;
        bad[11] = 3;
        assert_eq!(BootState::decode(&bad), None, "CRC");
        assert_eq!(BootState::decode(&[0u8; 512]), None);
        assert_eq!(
            BootState::decode(&BootState::fresh(Slot::B).encode())
                .unwrap()
                .active,
            Slot::B
        );
    }

    #[test]
    fn boot_policy() {
        let fresh = BootState::fresh(Slot::A);
        let d = choose(fresh);
        assert_eq!((d.slot, d.pending, d.state.boot_count), (Slot::A, false, 1));
        // Install B: three tries, then rollback.
        let mut st = schedule(fresh, Slot::B);
        assert_eq!(st.pending, Some((Slot::B, 3)));
        for tries_left in [2u8, 1, 0] {
            let d = choose(st);
            assert_eq!((d.slot, d.pending), (Slot::B, true));
            assert_eq!(d.state.pending, Some((Slot::B, tries_left)));
            st = d.state;
        }
        let d = choose(st);
        assert_eq!((d.slot, d.pending), (Slot::A, false), "exhausted");
        assert_eq!(d.state.pending, None);
        // Confirmation after the first good boot.
        let d = choose(schedule(fresh, Slot::B));
        let confirmed = confirm(d.state, d.slot);
        assert_eq!((confirmed.active, confirmed.pending), (Slot::B, None));
        // Rejected image: immediate rollback.
        let r = reject(schedule(fresh, Slot::B));
        assert_eq!((r.slot, r.state.pending), (Slot::A, None));
        // Try cannot be persisted: the active slot boots and the pending one stays.
        let u = unpersisted(schedule(fresh, Slot::B));
        assert_eq!((u.slot, u.pending), (Slot::A, false));
        assert_eq!(u.state.pending, Some((Slot::B, DEFAULT_TRIES)));
    }

    fn build(seed: &[u8; 32], version: u32, kernel: &[u8], initrd: &[u8]) -> Vec<u8> {
        let m = Manifest {
            version,
            kernel_sha256: sha256::sha256(kernel),
            initrd_sha256: sha256::sha256(initrd),
        };
        let mut buf = [0u8; 192];
        let text = m.write(&mut buf).as_bytes().to_vec();
        let sig = ed25519::sign(seed, &text);
        aricode_initrd::build(&[
            ("manifest", text.as_slice()),
            ("manifest.sig", &sig[..]),
            ("kernel.elf", kernel),
            ("initrd.img", initrd),
        ])
        .unwrap()
    }

    #[test]
    fn manifest_text_round_trip() {
        let m = Manifest {
            version: 12,
            kernel_sha256: [0xAB; 32],
            initrd_sha256: [0x01; 32],
        };
        let mut buf = [0u8; 192];
        let text = m.write(&mut buf);
        assert!(text.starts_with("version = 12\nkernel = abab"));
        assert_eq!(Manifest::parse(text), Some(m));
        assert!(Manifest::parse("version = x").is_none());
        assert!(Manifest::parse("version = 1\nkernel = 00\ninitrd = 00").is_none());
    }

    #[test]
    fn image_verification() {
        let seed = [9u8; 32];
        let pk = ed25519::public_key(&seed);
        let kernel = b"fake but consistent ELF".to_vec();
        let initrd = b"fake ARIRD".to_vec();
        let img = build(&seed, 7, &kernel, &initrd);
        let v = Image::verify(&img, &pk).unwrap();
        assert_eq!(v.manifest.version, 7);
        assert_eq!(v.kernel, kernel.as_slice());
        assert_eq!(v.initrd, initrd.as_slice());
        // Another key.
        let other = ed25519::public_key(&[1u8; 32]);
        assert_eq!(
            Image::verify(&img, &other).err(),
            Some(ImageError::BadSignature)
        );
        // Kernel tampered inside the container: different hash.
        let mut tampered = img.clone();
        let pos = tampered
            .windows(kernel.len())
            .position(|w| w == kernel.as_slice())
            .unwrap();
        tampered[pos] ^= 1;
        assert_eq!(
            Image::verify(&tampered, &pk).err(),
            Some(ImageError::HashMismatch("kernel.elf"))
        );
        // Container without signature.
        let unsigned =
            aricode_initrd::build(&[("manifest", &b"version = 1"[..]), ("kernel.elf", &b"k"[..])])
                .unwrap();
        assert_eq!(
            Image::verify(&unsigned, &pk).err(),
            Some(ImageError::MissingFile("manifest.sig"))
        );
        // Slot header.
        let h = Image::header(img.len() as u32);
        assert_eq!(Image::container_len(&h), Ok(img.len() as u32));
        assert_eq!(
            Image::container_len(&[0u8; 512]),
            Err(ImageError::BadHeader)
        );
    }
}
