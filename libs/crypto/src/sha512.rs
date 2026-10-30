// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! SHA-512 and SHA-384 (FIPS 180-4) over RustCrypto's `sha2` (ADR 0066),
//! portable in the kernel and AriBoot (feature `soft`).

use sha2::Digest;

pub const DIGEST_LEN: usize = 64;
pub const SHA384_DIGEST_LEN: usize = 48;

/// Incremental SHA-512; the state is wiped when dropped.
#[derive(Clone, Default)]
pub struct Sha512(sha2::Sha512);

impl Sha512 {
    pub fn new() -> Self {
        Self(sha2::Sha512::new())
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        self.0.finalize_reset().into()
    }
}

impl Drop for Sha512 {
    fn drop(&mut self) {
        // SAFETY: `sha2` states are words and byte buffers, valid as zeros.
        unsafe { crate::wipe_state(&mut self.0) };
    }
}

pub fn sha512(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha512::new();
    h.update(data);
    h.finish()
}

/// Incremental SHA-384; the state is wiped when dropped.
#[derive(Clone, Default)]
pub struct Sha384(sha2::Sha384);

impl Sha384 {
    pub fn new() -> Self {
        Self(sha2::Sha384::new())
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(mut self) -> [u8; SHA384_DIGEST_LEN] {
        self.0.finalize_reset().into()
    }
}

impl Drop for Sha384 {
    fn drop(&mut self) {
        // SAFETY: `sha2` states are words and byte buffers, valid as zeros.
        unsafe { crate::wipe_state(&mut self.0) };
    }
}

pub fn sha384(data: &[u8]) -> [u8; SHA384_DIGEST_LEN] {
    let mut h = Sha384::new();
    h.update(data);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    fn check(data: &[u8], expected: &str) {
        let mut buf = [0u8; 128];
        assert_eq!(hex(&sha512(data), &mut buf), expected);
    }

    #[test]
    fn fips_vectors() {
        check(b"abc", "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f");
        check(b"", "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e");
        check(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu", "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909");
        // More than one block with padding crossing the boundary.
        let data = [0x61u8; 200];
        let mut s = Sha512::new();
        s.update(&data[..77]);
        s.update(&data[77..]);
        assert_eq!(s.finish(), sha512(&data));
    }

    #[test]
    fn sha384_fips_vectors() {
        let mut buf = [0u8; 96];
        let mut check = |data: &[u8], expected: &str| {
            assert_eq!(hex(&sha384(data), &mut buf), expected);
        };
        check(b"abc", "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7");
        check(b"", "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b");
        check(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu", "09330c33f71147e83d192fc782cd1b4753111b173b3b05d22fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039");
        let data = [0x61u8; 200];
        let mut s = Sha384::new();
        s.update(&data[..129]);
        s.update(&data[129..]);
        assert_eq!(s.finish(), sha384(&data));
    }

    #[test]
    fn state_is_wiped_when_dropped() {
        let mut s = core::mem::ManuallyDrop::new(Sha384::new());
        s.update(b"secret");
        // SAFETY: dropped once; afterwards only its bytes are read.
        unsafe { core::mem::ManuallyDrop::drop(&mut s) };
        assert!(crate::tests::bytes_of(&*s).iter().all(|&b| b == 0));
    }
}
