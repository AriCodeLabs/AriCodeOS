// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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
//! - [`seed`]: the kernel seed carried across boots in sectors 1 and 2 of
//!   `ARIBOOT` (ADR 0043).

#![no_std]

pub mod seed;

use aricode_crypto::{ed25519, sha256, unhex};

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
    /// The loader that wrote this state trusts the development channel's
    /// key (ADR 0045): AriBoot sets it on every boot from its own build.
    /// Advisory only: the updater does not install a development build the
    /// loader would refuse, but trust itself is AriBoot's compiled anchor,
    /// so a forged flag can only make the updater install an image AriBoot
    /// then rejects (and rolls back from).
    pub loader_dev_channel: bool,
}

impl BootState {
    /// The state of a new image: `active` confirmed, nothing pending. Also
    /// AriBoot's reading of a damaged record, the one reading that needs
    /// no other evidence (ADR 0060); the updater never reads a damaged
    /// record as fresh ([`recovered`]).
    pub const fn fresh(active: Slot) -> Self {
        Self {
            active,
            pending: None,
            boot_count: 0,
            loader_dev_channel: false,
        }
    }

    /// Whether AriBoot has written this record: it counts every boot it
    /// chooses ([`choose`]), and nothing else raises the count, so a record
    /// holding 0 boots is a new image's or one the updater rebuilt (ADR
    /// 0060), and its `loader_dev_channel` says nothing about the loader.
    /// The test errs one way only: AriBoot also writes a record without
    /// counting (`reject`, `unpersisted`) and the count wraps at
    /// `u32::MAX`, so a loader-written record can read as not written; the
    /// development channel then stays closed one boot longer.
    pub fn written_by_loader(&self) -> bool {
        self.boot_count != 0
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
        s[16] = u8::from(self.loader_dev_channel);
        let crc = aricode_crypto::sha256::sha256(&s[..60]);
        s[60..64].copy_from_slice(&crc[..4]);
        s
    }

    /// `None` if the sector is not a valid state (magic, version or CRC):
    /// a damaged record, which says nothing about the slots (ADR 0060).
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
        // A loader before ADR 0045 left the byte zero.
        let loader_dev_channel = match s[16] {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self {
            active,
            pending,
            boot_count: u32::from_le_bytes([s[12], s[13], s[14], s[15]]),
            loader_dev_channel,
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
        ..state
    }
}

/// What AriBoot says it booted, from the keys it alone writes on the
/// command line (ADR 0045).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Booted {
    /// The slot, and whether it was booted pending confirmation
    /// (`boot_slot`, `boot_pending`); `None` for the ESP.
    pub slot: Option<(Slot, bool)>,
    /// The booted image is a development build (`boot_channel=dev`): the
    /// loader verified it, so it trusts the development channel's key.
    pub dev_channel: bool,
}

/// The state to write over a damaged record (ADR 0060), rebuilt from
/// what AriBoot booted, never from the damaged bytes.
///
/// - From a slot, pending or not: that slot active, nothing pending.
///   Confirmed (no `boot_pending`), it is active and stays so for the
///   whole boot, since only init's confirmation of a pending boot changes
///   the active slot. Pending, it is what init meant to write: init
///   confirms the slot at the end of its boot (step 11), after every
///   health check, and a boot that fails a check never gets there and
///   starts no shell; the updater is launched only from a shell, which
///   init starts after step 11. So a running updater means the slot passed
///   init's checks, and a slot still marked pending only means the
///   confirmation's own write failed. A trial scheduled since (`update
///   apply`, `upgrade`, `update rollback`) is dropped: the running system
///   stays, and the install can be made again. **This rests on the
///   launch order**: anything that could run the updater before init's
///   step 11 would make this rule wrong for pending boots.
/// - From the ESP: AriBoot's own reading, A active ([`BootState::fresh`]),
///   which it does not write when A does not boot.
///
/// `loader_dev_channel` is set only when the running image is a
/// development build; otherwise it stays off until AriBoot writes the
/// record on the next boot.
pub fn recovered(booted: Booted) -> BootState {
    let active = booted.slot.map_or(Slot::A, |(slot, _)| slot);
    BootState {
        loader_dev_channel: booted.dev_channel,
        ..BootState::fresh(active)
    }
}

/// State after installing a new image in the inactive slot.
pub fn schedule(state: BootState, slot: Slot) -> BootState {
    BootState {
        pending: Some((slot, DEFAULT_TRIES)),
        ..state
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
    /// Signed by one channel's key but naming the other channel (ADR 0045):
    /// a key signs only its own channel.
    ChannelMismatch,
    /// The reader of a streaming verification failed (incident 0021).
    Unreadable,
}

/// The update channel of a system image or a source (ADR 0045): `Stable`
/// (releases from `main`, signed with the release key) or `Dev` (builds of
/// `develop` or a branch, signed with the development channel's key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    #[default]
    Stable,
    Dev,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stable" => Some(Channel::Stable),
            "dev" => Some(Channel::Dev),
            _ => None,
        }
    }
}

/// Longest commit text: 40 hex digits and `-dirty`.
pub const MAX_COMMIT: usize = 46;
const DIRTY: &str = "-dirty";

/// The commit a development build was made from: 7 to 40 lowercase hex
/// digits, `-dirty` when the tree had uncommitted changes (ADR 0045).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    bytes: [u8; MAX_COMMIT],
    len: u8,
}

impl Commit {
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_suffix(DIRTY).unwrap_or(s);
        if !(7..=40).contains(&hex.len())
            || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let mut bytes = [0u8; MAX_COMMIT];
        bytes[..s.len()].copy_from_slice(s.as_bytes());
        Some(Self {
            bytes,
            len: s.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// Parsed manifest.
///
/// ```text
/// version = 12            a release (stable): exactly these three keys,
/// kernel = <sha256>       as every loader since Phase 30 parses them
/// initrd = <sha256>
///
/// version = 7             a development build: its build number,
/// channel = dev           the channel and the commit it was built from
/// commit = 0123abc
/// kernel = <sha256>
/// initrd = <sha256>
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    /// The release version, or the build number of a development build.
    pub version: u32,
    pub channel: Channel,
    /// Present exactly for development builds.
    pub commit: Option<Commit>,
    pub kernel_sha256: [u8; 32],
    pub initrd_sha256: [u8; 32],
}

/// Room for the longest canonical manifest text.
pub const MANIFEST_MAX: usize = 256;

impl Manifest {
    /// A release manifest (no channel line, no commit).
    pub const fn stable(version: u32, kernel_sha256: [u8; 32], initrd_sha256: [u8; 32]) -> Self {
        Self {
            version,
            channel: Channel::Stable,
            commit: None,
            kernel_sha256,
            initrd_sha256,
        }
    }

    /// Strict: unknown or repeated keys, a `channel` other than `dev` (a
    /// release has no channel line: one spelling), a development build
    /// without its commit or a release with one are all refused.
    pub fn parse(text: &str) -> Option<Self> {
        let (mut version, mut kernel, mut initrd) = (None, None, None);
        let (mut channel, mut commit) = (None, None);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            let hash = |slot: &Option<[u8; 32]>| -> Option<[u8; 32]> {
                if slot.is_some() {
                    return None;
                }
                let mut h = [0u8; 32];
                unhex(v, &mut h)?;
                Some(h)
            };
            match k.trim() {
                "version" if version.is_none() => version = Some(v.parse().ok()?),
                "kernel" => kernel = Some(hash(&kernel)?),
                "initrd" => initrd = Some(hash(&initrd)?),
                "channel" if channel.is_none() && v == "dev" => channel = Some(Channel::Dev),
                "commit" if commit.is_none() => commit = Some(Commit::parse(v)?),
                _ => return None,
            }
        }
        let channel = channel.unwrap_or(Channel::Stable);
        if (channel == Channel::Dev) != commit.is_some() {
            return None;
        }
        Some(Self {
            version: version?,
            channel,
            commit,
            kernel_sha256: kernel?,
            initrd_sha256: initrd?,
        })
    }

    /// `version 12` or `dev build 7 (commit 0123abc)`, for reports.
    pub fn describe(&self) -> Describe {
        Describe {
            channel: self.channel,
            version: self.version,
            commit: self.commit,
        }
    }

    /// Canonical manifest text (what gets signed).
    pub fn write<'a>(&self, out: &'a mut [u8; MANIFEST_MAX]) -> &'a str {
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
        if self.channel == Channel::Dev {
            put(b"\nchannel = dev", &mut n);
        }
        if let Some(commit) = &self.commit {
            put(b"\ncommit = ", &mut n);
            put(commit.as_str().as_bytes(), &mut n);
        }
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

/// What a system is, in one phrase: `version 12` for a release,
/// `dev build 7 (commit 0123abc)` for a development build (ADR 0045).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Describe {
    pub channel: Channel,
    pub version: u32,
    pub commit: Option<Commit>,
}

impl core::fmt::Display for Describe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match (self.channel, &self.commit) {
            (Channel::Dev, Some(c)) => {
                write!(f, "dev build {} (commit {})", self.version, c.as_str())
            }
            (Channel::Dev, None) => write!(f, "dev build {}", self.version),
            (Channel::Stable, _) => write!(f, "version {}", self.version),
        }
    }
}

/// The keys a verifier trusts (ADR 0045): the release key for the stable
/// channel and, when the build trusts it, the development channel's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub stable: [u8; 32],
    pub dev: Option<[u8; 32]>,
}

impl Keys {
    /// Only the release key.
    pub const fn stable_only(stable: [u8; 32]) -> Self {
        Self { stable, dev: None }
    }

    /// The key that signs `channel`, if this verifier trusts it.
    pub fn for_channel(&self, channel: Channel) -> Option<&[u8; 32]> {
        match channel {
            Channel::Stable => Some(&self.stable),
            Channel::Dev => self.dev.as_ref(),
        }
    }
}

/// Verified image: references to the bytes inside the container.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    pub manifest: Manifest,
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
}

/// A container verified piece by piece: its manifest and where kernel and
/// initrd lie in it.
#[derive(Debug, Clone, Copy)]
pub struct StreamVerified {
    pub manifest: Manifest,
    pub kernel: aricode_initrd::Span,
    pub initrd: aricode_initrd::Span,
}

/// Largest manifest a verification reads: four times the canonical
/// [`MANIFEST_MAX`]; the signature covers the whole manifest, so it is
/// read whole.
pub const MANIFEST_READ_MAX: usize = 4 * MANIFEST_MAX;

/// Bytes hashed per read of a streaming verification.
const HASH_PIECE: usize = 4096;

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

    /// Verifies a container with the release key only (a stable image).
    pub fn verify<'a>(container: &'a [u8], pubkey: &[u8; 32]) -> Result<Verified<'a>, ImageError> {
        Self::verify_with(container, &Keys::stable_only(*pubkey))
    }

    /// Verifies a container: the manifest signature with one of `keys`, the
    /// manifest naming the channel of the key that signed it (ADR 0045),
    /// and the hashes of kernel and initrd. Only returns data if everything
    /// matches; nothing in the manifest is read before its signature
    /// verified.
    pub fn verify_with<'a>(container: &'a [u8], keys: &Keys) -> Result<Verified<'a>, ImageError> {
        let v = Self::verify_streaming(
            container.len(),
            |offset, buf| {
                buf.copy_from_slice(&container[offset..offset + buf.len()]);
                Ok(())
            },
            keys,
        )?;
        let at = |(offset, len): aricode_initrd::Span| &container[offset..offset + len];
        Ok(Verified {
            manifest: v.manifest,
            kernel: at(v.kernel),
            initrd: at(v.initrd),
        })
    }

    /// [`verify_with`](Self::verify_with) for a container of
    /// `container_len` bytes read piece by piece through `read(offset,
    /// buf)` (fills `buf` from `offset`), so an image need not fit in memory
    /// to be verified (incident 0021). The same checks in the same order;
    /// `verify_with` is this over a slice. A manifest over
    /// [`MANIFEST_READ_MAX`] bytes is `BadManifest`; a failed read,
    /// `Unreadable`.
    pub fn verify_streaming(
        container_len: usize,
        mut read: impl FnMut(usize, &mut [u8]) -> Result<(), ()>,
        keys: &Keys,
    ) -> Result<StreamVerified, ImageError> {
        // A failed read is told apart from a malformed container.
        let mut unreadable = false;
        let mut fetch = |offset: usize, buf: &mut [u8]| {
            read(offset, buf).map_err(|()| {
                unreadable = true;
                aricode_initrd::InitrdError::Truncated
            })
        };
        let names = ["manifest", "manifest.sig", "kernel.elf", "initrd.img"];
        let spans = aricode_initrd::locate(container_len, &mut fetch, names);
        let [manifest, sig, kernel, initrd] = match spans {
            Ok(s) => s,
            Err(_) if unreadable => return Err(ImageError::Unreadable),
            Err(_) => return Err(ImageError::BadContainer),
        };
        let manifest = manifest.ok_or(ImageError::MissingFile("manifest"))?;
        let sig = sig.ok_or(ImageError::MissingFile("manifest.sig"))?;
        let kernel = kernel.ok_or(ImageError::MissingFile("kernel.elf"))?;
        let initrd = initrd.ok_or(ImageError::MissingFile("initrd.img"))?;
        if sig.1 != ed25519::SIGNATURE_LEN {
            return Err(ImageError::BadSignature);
        }
        if manifest.1 > MANIFEST_READ_MAX {
            return Err(ImageError::BadManifest);
        }
        let mut signature = [0u8; 64];
        fetch(sig.0, &mut signature).map_err(|_| ImageError::Unreadable)?;
        let mut manifest_buf = [0u8; MANIFEST_READ_MAX];
        let manifest_bytes = &mut manifest_buf[..manifest.1];
        fetch(manifest.0, manifest_bytes).map_err(|_| ImageError::Unreadable)?;
        let manifest_bytes = &*manifest_bytes;
        let signed_by = [Channel::Stable, Channel::Dev]
            .into_iter()
            .find(|&c| {
                keys.for_channel(c)
                    .is_some_and(|k| ed25519::verify(k, manifest_bytes, &signature))
            })
            .ok_or(ImageError::BadSignature)?;
        let text = core::str::from_utf8(manifest_bytes).map_err(|_| ImageError::BadManifest)?;
        let parsed = Manifest::parse(text).ok_or(ImageError::BadManifest)?;
        if parsed.channel != signed_by {
            return Err(ImageError::ChannelMismatch);
        }
        let mut digest = |(offset, len): aricode_initrd::Span| {
            let mut h = sha256::Sha256::new();
            let mut piece = [0u8; HASH_PIECE];
            let mut done = 0;
            while done < len {
                let n = (len - done).min(HASH_PIECE);
                fetch(offset + done, &mut piece[..n]).map_err(|_| ImageError::Unreadable)?;
                h.update(&piece[..n]);
                done += n;
            }
            Ok(h.finish())
        };
        if digest(kernel)? != parsed.kernel_sha256 {
            return Err(ImageError::HashMismatch("kernel.elf"));
        }
        if digest(initrd)? != parsed.initrd_sha256 {
            return Err(ImageError::HashMismatch("initrd.img"));
        }
        Ok(StreamVerified {
            manifest: parsed,
            kernel,
            initrd,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;
    use std::vec::Vec;

    #[test]
    fn state_round_trip_and_crc() {
        let s = BootState {
            active: Slot::A,
            pending: Some((Slot::B, 2)),
            boot_count: 41,
            loader_dev_channel: false,
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

    fn booted(slot: Option<(Slot, bool)>) -> Booted {
        Booted {
            slot,
            dev_channel: false,
        }
    }

    #[test]
    fn a_damaged_state_is_rebuilt_from_the_booted_slot() {
        // Incident 0045: B active and booted, the record then damaged. The
        // updater read it as fresh (A active) and took B, the running
        // slot, for the inactive one to install into.
        let mut sector = BootState::fresh(Slot::B).encode();
        sector[60] ^= 1;
        assert_eq!(BootState::decode(&sector), None);
        let state = recovered(booted(Some((Slot::B, false))));
        assert_eq!((state.active, state.pending), (Slot::B, None));
        assert_eq!(choose(state).slot, Slot::B, "the next boot keeps B");
        // From the ESP nothing is known: AriBoot's own reading.
        assert_eq!(recovered(booted(None)), BootState::fresh(Slot::A));
        // The development flag only from a running development build.
        let dev = recovered(Booted {
            slot: Some((Slot::A, false)),
            dev_channel: true,
        });
        assert!(dev.loader_dev_channel);
        assert!(!recovered(booted(None)).loader_dev_channel);
        // Until AriBoot writes it again, a rebuilt record is not the
        // loader's word (re-audit of incident 0045, L-a).
        assert!(!recovered(booted(Some((Slot::B, true)))).written_by_loader());
        assert!(choose(recovered(booted(None))).state.written_by_loader());
        // What is written decodes as itself.
        for b in [None, Some((Slot::A, true)), Some((Slot::B, false))] {
            let s = recovered(booted(b));
            assert_eq!(BootState::decode(&s.encode()), Some(s));
        }
    }

    /// Audit and re-audit of incident 0045 (H1, M1): an updater runs only
    /// after init's step 11, so a slot booted pending has passed init's
    /// checks, whether its confirmation was written or not. Rebuilt as
    /// "the other slot active, this one pending with no tries", the
    /// running slot became the install target, or the next boot took an
    /// untried image as confirmed; left to AriBoot, a machine running B
    /// went back to A. Rebuilt as that slot active, neither happens.
    #[test]
    fn a_pending_boot_is_rebuilt_as_the_slot_init_checked() {
        for x in [Slot::A, Slot::B] {
            let y = x.other();
            let repaired = recovered(booted(Some((x, true))));
            // (a) y confirmed, x installed: AriBoot boots x pending, init
            // confirms x, then the record is damaged.
            let d = choose(schedule(BootState::fresh(y), x));
            assert_eq!((d.slot, d.pending), (x, true));
            let confirmed = confirm(d.state, x);
            assert_eq!(repaired, BootState::fresh(x), "what init wrote");
            assert_eq!(repaired.active.other(), confirmed.active.other());
            assert_ne!(repaired.active.other(), x, "never the running slot");
            assert_eq!(choose(repaired).slot, x, "the next boot keeps x");
            // (b) The same boot, then y installed and scheduled, and a
            // second install into y tears the record: the untried y is
            // never booted as confirmed; x, which passed init, boots.
            let scheduled = schedule(confirmed, y);
            let d2 = choose(scheduled);
            assert_eq!((d2.slot, d2.pending), (y, true));
            let next = choose(repaired);
            assert_eq!((next.slot, next.pending), (x, false));
            // (c) Init's confirmation write failed (no ARIBOOT, a read or
            // write error): the record still says y active, x pending;
            // the repair writes what init meant to.
            let unconfirmed = d.state;
            assert_eq!(unconfirmed.active, y);
            let meant = confirm(unconfirmed, x);
            assert_eq!(
                (repaired.active, repaired.pending),
                (meant.active, meant.pending)
            );
        }
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
        build_manifest(
            seed,
            Manifest::stable(version, sha256::sha256(kernel), sha256::sha256(initrd)),
            kernel,
            initrd,
        )
    }

    fn build_manifest(seed: &[u8; 32], m: Manifest, kernel: &[u8], initrd: &[u8]) -> Vec<u8> {
        let mut buf = [0u8; MANIFEST_MAX];
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

    fn dev_manifest(version: u32, commit: &str, kernel: &[u8], initrd: &[u8]) -> Manifest {
        Manifest {
            channel: Channel::Dev,
            commit: Commit::parse(commit),
            ..Manifest::stable(version, sha256::sha256(kernel), sha256::sha256(initrd))
        }
    }

    #[test]
    fn manifest_text_round_trip() {
        let m = Manifest::stable(12, [0xAB; 32], [0x01; 32]);
        let mut buf = [0u8; MANIFEST_MAX];
        let text = m.write(&mut buf);
        assert!(text.starts_with("version = 12\nkernel = abab"));
        assert_eq!(Manifest::parse(text), Some(m));
        assert!(Manifest::parse("version = x").is_none());
        assert!(Manifest::parse("version = 1\nkernel = 00\ninitrd = 00").is_none());
        assert_eq!(m.describe().to_string(), "version 12");
    }

    #[test]
    fn dev_manifest_carries_channel_and_commit() {
        // The longest commit text still fits the canonical buffer.
        let longest = std::format!("{}-dirty", "f".repeat(40));
        for commit in ["0123abc", longest.as_str()] {
            let m = Manifest {
                channel: Channel::Dev,
                commit: Commit::parse(commit),
                ..Manifest::stable(4_000_000_000, [0xAB; 32], [0x01; 32])
            };
            let mut buf = [0u8; MANIFEST_MAX];
            let text = m.write(&mut buf);
            assert!(text.starts_with(&std::format!(
                "version = 4000000000\nchannel = dev\ncommit = {commit}\nkernel = "
            )));
            assert_eq!(Manifest::parse(text), Some(m));
            assert_eq!(
                m.describe().to_string(),
                std::format!("dev build 4000000000 (commit {commit})")
            );
        }
        let hashes = std::format!(
            "kernel = {}\ninitrd = {}\n",
            "00".repeat(32),
            "11".repeat(32)
        );
        let ok = std::format!("version = 7\nchannel = dev\ncommit = 0123abc\n{hashes}");
        assert!(Manifest::parse(&ok).is_some());
        for bad in [
            // A development build always names its commit; a release never.
            std::format!("version = 7\nchannel = dev\n{hashes}"),
            std::format!("version = 7\ncommit = 0123abc\n{hashes}"),
            // One spelling: a release has no channel line.
            std::format!("version = 7\nchannel = stable\n{hashes}"),
            std::format!("version = 7\nchannel = beta\ncommit = 0123abc\n{hashes}"),
            // Repeated keys.
            std::format!("{ok}channel = dev\n"),
            std::format!("{ok}version = 8\n"),
            std::format!("{ok}kernel = {}\n", "22".repeat(32)),
            // Commit text.
            std::format!("version = 7\nchannel = dev\ncommit = 0123ab\n{hashes}"),
            std::format!("version = 7\nchannel = dev\ncommit = 0123ABC\n{hashes}"),
            std::format!("version = 7\nchannel = dev\ncommit = 0123abc-wip\n{hashes}"),
            std::format!(
                "version = 7\nchannel = dev\ncommit = {}\n{hashes}",
                "a".repeat(41)
            ),
        ] {
            assert!(Manifest::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_signing_key_decides_the_channel() {
        // ADR 0045: the release key signs releases only, the development
        // channel's key development builds only.
        let (stable_seed, dev_seed) = ([9u8; 32], [10u8; 32]);
        let keys = Keys {
            stable: ed25519::public_key(&stable_seed),
            dev: Some(ed25519::public_key(&dev_seed)),
        };
        let (kernel, initrd) = (b"k".to_vec(), b"i".to_vec());
        let release = build(&stable_seed, 12, &kernel, &initrd);
        let dev = build_manifest(
            &dev_seed,
            dev_manifest(7, "0123abc", &kernel, &initrd),
            &kernel,
            &initrd,
        );
        let v = Image::verify_with(&release, &keys).unwrap();
        assert_eq!(
            (v.manifest.channel, v.manifest.version),
            (Channel::Stable, 12)
        );
        let v = Image::verify_with(&dev, &keys).unwrap();
        assert_eq!((v.manifest.channel, v.manifest.version), (Channel::Dev, 7));
        assert_eq!(v.manifest.commit.unwrap().as_str(), "0123abc");
        // Without the development key a development build does not verify.
        let stable_only = Keys::stable_only(keys.stable);
        assert_eq!(
            Image::verify_with(&dev, &stable_only).err(),
            Some(ImageError::BadSignature)
        );
        assert_eq!(
            Image::verify(&dev, &keys.stable).err(),
            Some(ImageError::BadSignature)
        );
        // A manifest naming the other channel than its key's is refused.
        let dev_by_release = build_manifest(
            &stable_seed,
            dev_manifest(7, "0123abc", &kernel, &initrd),
            &kernel,
            &initrd,
        );
        assert_eq!(
            Image::verify_with(&dev_by_release, &keys).err(),
            Some(ImageError::ChannelMismatch)
        );
        let release_by_dev = build(&dev_seed, 12, &kernel, &initrd);
        assert_eq!(
            Image::verify_with(&release_by_dev, &keys).err(),
            Some(ImageError::ChannelMismatch)
        );
    }

    #[test]
    fn loader_dev_channel_flag_survives_confirm_and_schedule() {
        let mut s = BootState::fresh(Slot::A);
        assert!(!s.loader_dev_channel);
        s.loader_dev_channel = true;
        let sector = s.encode();
        assert_eq!(sector[16], 1);
        assert_eq!(BootState::decode(&sector), Some(s));
        assert!(schedule(s, Slot::B).loader_dev_channel);
        assert!(confirm(schedule(s, Slot::B), Slot::B).loader_dev_channel);
        assert!(choose(s).state.loader_dev_channel);
        // A loader before ADR 0045 wrote zero there: no flag.
        assert!(
            !BootState::decode(&BootState::fresh(Slot::A).encode())
                .unwrap()
                .loader_dev_channel
        );
        // Any other value (with a valid CRC) is not a state.
        let mut odd = sector;
        odd[16] = 2;
        let crc = aricode_crypto::sha256::sha256(&odd[..60]);
        odd[60..64].copy_from_slice(&crc[..4]);
        assert_eq!(BootState::decode(&odd), None);
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
        // Read piece by piece: the same verdicts, and a failing reader is
        // reported as such.
        let read_from = |img: &[u8]| {
            let img = img.to_vec();
            move |offset: usize, buf: &mut [u8]| {
                buf.copy_from_slice(&img[offset..offset + buf.len()]);
                Ok(())
            }
        };
        let keys = Keys::stable_only(pk);
        let v = Image::verify_streaming(img.len(), read_from(&img), &keys).unwrap();
        assert_eq!(v.manifest.version, 7);
        assert_eq!(&img[v.kernel.0..v.kernel.0 + v.kernel.1], kernel.as_slice());
        assert_eq!(
            Image::verify_streaming(tampered.len(), read_from(&tampered), &keys).err(),
            Some(ImageError::HashMismatch("kernel.elf"))
        );
        let mut reads = 0;
        let failing = |offset: usize, buf: &mut [u8]| {
            reads += 1;
            if reads > 3 {
                return Err(());
            }
            buf.copy_from_slice(&img[offset..offset + buf.len()]);
            Ok(())
        };
        assert_eq!(
            Image::verify_streaming(img.len(), failing, &keys).err(),
            Some(ImageError::Unreadable)
        );
        // A manifest larger than any the builder writes is refused unread.
        let big = "x".repeat(MANIFEST_READ_MAX + 1);
        let oversized = aricode_initrd::build(&[
            ("manifest", big.as_bytes()),
            ("manifest.sig", &[0u8; 64][..]),
            ("kernel.elf", &b"k"[..]),
            ("initrd.img", &b"i"[..]),
        ])
        .unwrap();
        assert_eq!(
            Image::verify(&oversized, &pk).err(),
            Some(ImageError::BadManifest)
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
