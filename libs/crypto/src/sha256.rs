// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! SHA-256 (FIPS 180-4) over RustCrypto's `sha2` (ADR 0066): SHA-NI when
//! the CPU has it in user programs, the portable code in the kernel and
//! AriBoot (feature `soft`).

use sha2::Digest;

pub const DIGEST_LEN: usize = 32;

/// Incremental SHA-256; the state is wiped when dropped.
#[derive(Clone, Default)]
pub struct Sha256(sha2::Sha256);

impl Sha256 {
    pub fn new() -> Self {
        Self(sha2::Sha256::new())
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        self.0.finalize_reset().into()
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: `sha2` states are words and byte buffers, valid as zeros.
        unsafe { crate::wipe_state(&mut self.0) };
    }
}

pub fn sha256(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    fn h(data: &[u8]) -> alloc_free::Hex {
        alloc_free::Hex(sha256(data))
    }

    mod alloc_free {
        pub struct Hex(pub [u8; 32]);
        impl PartialEq<&str> for Hex {
            fn eq(&self, other: &&str) -> bool {
                let mut buf = [0u8; 64];
                super::hex(&self.0, &mut buf) == *other
            }
        }
        impl core::fmt::Debug for Hex {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                let mut buf = [0u8; 64];
                f.write_str(super::hex(&self.0, &mut buf))
            }
        }
    }

    #[test]
    fn fips_vectors() {
        assert_eq!(
            h(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            h(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            h(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million = [b'a'; 1000];
        let mut s = Sha256::new();
        for _ in 0..1000 {
            s.update(&million);
        }
        assert_eq!(
            alloc_free::Hex(s.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data: [u8; 300] = core::array::from_fn(|i| (i * 7 % 251) as u8);
        let mut s = Sha256::new();
        for chunk in data.chunks(37) {
            s.update(chunk);
        }
        assert_eq!(s.finish(), sha256(&data));
        let _ = hex;
    }

    #[test]
    fn state_is_wiped_when_dropped() {
        let mut s = core::mem::ManuallyDrop::new(Sha256::new());
        s.update(b"secret");
        // SAFETY: dropped once; afterwards only its bytes are read.
        unsafe { core::mem::ManuallyDrop::drop(&mut s) };
        assert!(crate::tests::bytes_of(&*s).iter().all(|&b| b == 0));
    }
}
