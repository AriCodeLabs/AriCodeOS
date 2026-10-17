// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The project's Ed25519 public key, the single trust anchor every verifier
//! of the build uses: AriBoot (system images), init's broker and the
//! installer (packages), the updater (images, packages, update indexes).
//!
//! `cargo xtask ... --signing-key DIR` builds with `DIR/ed25519.pub`
//! (passed as `ARICODE_SIGNING_PUB`; cargo rebuilds when it changes).
//! Without it the build trusts the development key in `keys/dev`, whose
//! seed is public (ADR 0029, "The signing key").
//!
//! The hex is decoded at compile time: an invalid key does not build. The
//! key lives in [`TRUST_ANCHOR`] behind a fixed tag, and verifiers read it
//! with a volatile load, so its bytes are always present in the binary and
//! xtask checks after every build that they are the selected key.

/// The public key as hex (64 digits, surrounding whitespace allowed).
const PUBLIC_KEY_HEX: &str = match option_env!("ARICODE_SIGNING_PUB") {
    Some(key) => key,
    None => include_str!("../../../keys/dev/ed25519.pub"),
};

/// Marker before the key bytes in every verifier binary.
pub const ANCHOR_TAG: [u8; 16] = *b"ARICODE-TRUSTKEY";

/// The tag followed by the key, laid out contiguously.
#[repr(C)]
pub struct Anchor {
    pub tag: [u8; 16],
    pub key: [u8; 32],
}

pub static TRUST_ANCHOR: Anchor = Anchor {
    tag: ANCHOR_TAG,
    key: decode(PUBLIC_KEY_HEX),
};

const fn digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("ARICODE_SIGNING_PUB / keys/dev/ed25519.pub: not hex"),
    }
}

const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// 64 hex digits with optional surrounding whitespace; anything else stops
/// the build.
const fn decode(text: &str) -> [u8; 32] {
    let b = text.as_bytes();
    let mut start = 0;
    while start < b.len() && is_space(b[start]) {
        start += 1;
    }
    let mut end = b.len();
    while end > start && is_space(b[end - 1]) {
        end -= 1;
    }
    assert!(
        end - start == 64,
        "ARICODE_SIGNING_PUB / keys/dev/ed25519.pub: expected 64 hex digits"
    );
    let mut key = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        key[i] = digit(b[start + 2 * i]) << 4 | digit(b[start + 2 * i + 1]);
        i += 1;
    }
    key
}

/// The public key, read from [`TRUST_ANCHOR`].
pub fn public_key() -> [u8; 32] {
    // SAFETY: a valid, aligned, immutable static; the volatile read only
    // keeps the compiler from folding the key away from the binary.
    unsafe { core::ptr::read_volatile(&TRUST_ANCHOR.key) }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn embedded_key_is_the_dev_key_by_default() {
        if option_env!("ARICODE_SIGNING_PUB").is_none() {
            let mut dev = [0u8; 32];
            crate::unhex(
                include_str!("../../../keys/dev/ed25519.pub").trim(),
                &mut dev,
            )
            .unwrap();
            assert_eq!(public_key(), dev);
        }
        assert_eq!(TRUST_ANCHOR.tag, ANCHOR_TAG);
    }

    #[test]
    fn const_decoder() {
        let hex = "  000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1F\n";
        let key = decode(hex);
        assert_eq!(key[0], 0);
        assert_eq!(key[15], 0x0f);
        assert_eq!(key[31], 0x1f);
        assert!(std::panic::catch_unwind(|| decode("abcd")).is_err());
        assert!(std::panic::catch_unwind(|| decode(&"zz".repeat(32))).is_err());
    }
}
