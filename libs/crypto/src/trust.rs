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

/// The public key as hex (64 digits, surrounding whitespace allowed).
pub const PUBLIC_KEY_HEX: &str = match option_env!("ARICODE_SIGNING_PUB") {
    Some(key) => key,
    None => include_str!("../../../keys/dev/ed25519.pub"),
};

/// The public key; `None` only if the embedded text is not a key, which
/// the build of a verifier must treat as fatal.
pub fn public_key() -> Option<[u8; 32]> {
    let mut pk = [0u8; 32];
    crate::unhex(PUBLIC_KEY_HEX.trim(), &mut pk)?;
    Some(pk)
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_key_parses() {
        assert!(super::public_key().is_some());
    }
}
