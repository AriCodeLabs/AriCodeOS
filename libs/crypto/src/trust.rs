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
//!
//! A second, optional anchor is the development channel's key (ADR 0045):
//! `ARICODE_DEV_CHANNEL_PUB`, set by xtask for the updater of every build
//! and for AriBoot only on a machine that opted in (`usb-image
//! --dev-source`). Development builds are signed with it; the release key
//! never signs them and it never signs a release, so the key that verifies
//! an image or an index also tells its channel. Without the variable
//! [`dev_channel_key`] is `None` and nothing of the development channel
//! verifies.

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

/// The development channel's public key as hex, when this build trusts
/// that channel at all.
const DEV_CHANNEL_HEX: Option<&str> = option_env!("ARICODE_DEV_CHANNEL_PUB");

/// Marker before the development channel's key in a binary that carries it.
pub const DEV_ANCHOR_TAG: [u8; 16] = *b"ARICODE-DEVCHKEY";

/// The development channel's anchor; its key is all zeros (and never read)
/// when the build does not trust the channel.
pub static DEV_CHANNEL_ANCHOR: Anchor = Anchor {
    tag: DEV_ANCHOR_TAG,
    key: match DEV_CHANNEL_HEX {
        Some(hex) => decode(hex),
        None => [0; 32],
    },
};

/// The public keys whose seeds are in the repository (`keys/dev`): the test
/// key and the test development-channel key. Anyone can sign with them.
pub const PUBLIC_SEED_KEYS: [[u8; 32]; 2] = [
    decode(include_str!("../../../keys/dev/ed25519.pub")),
    decode(include_str!("../../../keys/dev/dev-channel/ed25519.pub")),
];

const fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut i = 0;
    while i < 32 {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Compile-time rules of the two anchors (ADR 0045, decision 3): the
/// development channel's key is never the release key (the key tells the
/// channel), and a build that trusts a real release key never trusts a key
/// whose seed is public for its development channel.
const _: () = {
    if let Some(hex) = DEV_CHANNEL_HEX {
        let dev = decode(hex);
        assert!(
            !same(&dev, &TRUST_ANCHOR.key),
            "ARICODE_DEV_CHANNEL_PUB equals the release key: the channels need two keys"
        );
        if option_env!("ARICODE_SIGNING_PUB").is_some() {
            let mut i = 0;
            while i < PUBLIC_SEED_KEYS.len() {
                assert!(
                    !same(&dev, &PUBLIC_SEED_KEYS[i]),
                    "a build with a real signing key cannot trust a public test key for the dev channel"
                );
                i += 1;
            }
        }
    }
};

const fn digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!(
            "trust anchor (ARICODE_SIGNING_PUB, ARICODE_DEV_CHANNEL_PUB or keys/dev): not hex"
        ),
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
        "trust anchor (ARICODE_SIGNING_PUB, ARICODE_DEV_CHANNEL_PUB or keys/dev): expected 64 hex digits"
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

/// The development channel's public key, read from [`DEV_CHANNEL_ANCHOR`];
/// `None` when this build does not trust that channel.
pub fn dev_channel_key() -> Option<[u8; 32]> {
    match DEV_CHANNEL_HEX {
        // SAFETY: as in `public_key`.
        Some(_) => Some(unsafe { core::ptr::read_volatile(&DEV_CHANNEL_ANCHOR.key) }),
        None => None,
    }
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
    fn dev_channel_anchor_follows_the_variable() {
        assert_eq!(DEV_CHANNEL_ANCHOR.tag, DEV_ANCHOR_TAG);
        match option_env!("ARICODE_DEV_CHANNEL_PUB") {
            Some(hex) => {
                let key = dev_channel_key().expect("set");
                assert_eq!(key, decode(hex));
                assert_ne!(key, public_key(), "two channels, two keys");
            }
            None => assert_eq!(dev_channel_key(), None),
        }
        // The two test keys whose seeds are public are different keys.
        assert!(!same(&PUBLIC_SEED_KEYS[0], &PUBLIC_SEED_KEYS[1]));
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
