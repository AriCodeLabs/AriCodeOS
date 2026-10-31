// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! ML-KEM-768 encapsulation (FIPS 203), for the SSH server's hybrid key
//! exchange (ADR 0068), over RustCrypto's `ml-kem`. Behind the `mlkem`
//! feature: the kernel and AriBoot do not link it.

use crate::{isolated, scrub_stack_deep, wipe, SecretBytes};

pub const ENCAPSULATION_KEY_LEN: usize = 1184;
pub const CIPHERTEXT_LEN: usize = 1088;

/// Words of each of the two frames [`scrub_stack_deep`] clears after an
/// encapsulation, 24 KiB in all: `ml-kem` keeps its matrix, its sampled
/// vectors and G's output (the shared key and the encryption randomness)
/// in frames reaching 22 to 24 KiB below this function in sshd's release
/// and dev builds (`cargo xtask stack-check`). No deeper, so the scrub
/// does not lengthen sshd's deepest path; two frames, so neither passes
/// its frame budget (incident 0055). The stack test proves the secrets
/// gone in both builds.
const SCRUB_MLKEM: usize = 1536;

/// Encapsulates to the client's key `ek` with the randomness `m`: the
/// ciphertext and the shared key, or `None` for a key of the wrong size
/// or one that fails FIPS 203's checks. What `ml-kem` leaves on the stack
/// is zeroed before returning (audit of ADR 0068, L1).
pub fn encapsulate(
    ek: &[u8],
    m: &SecretBytes<32>,
) -> Option<([u8; CIPHERTEXT_LEN], SecretBytes<32>)> {
    let r = isolated(|| {
        let ek = ml_kem::array::Array::try_from(ek).ok()?;
        let ek = ml_kem::EncapsulationKey768::new(&ek).ok()?;
        let mut m = ml_kem::B32::from(m.0);
        let (ct, mut k) = ek.encapsulate_deterministic(&m);
        wipe(m.as_mut_slice());
        let mut shared = SecretBytes([0u8; 32]);
        shared.0.copy_from_slice(&k);
        wipe(k.as_mut_slice());
        let mut out = [0u8; CIPHERTEXT_LEN];
        out.copy_from_slice(&ct);
        Some((out, shared))
    });
    scrub_stack_deep::<SCRUB_MLKEM>();
    r
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use ml_kem::{Decapsulate, KeyExport};
    use sha3::Digest;

    /// A key pair from a fixed seed: the decapsulation key and the
    /// encapsulation key's bytes.
    fn key_pair() -> (ml_kem::DecapsulationKey768, std::vec::Vec<u8>) {
        let dk = ml_kem::DecapsulationKey768::from_seed(ml_kem::Seed::from([0x21u8; 64]));
        let ek = dk.encapsulation_key().to_bytes().to_vec();
        (dk, ek)
    }

    #[test]
    fn the_holder_of_the_key_gets_the_same_secret() {
        let (dk, ek) = key_pair();
        assert_eq!(ek.len(), ENCAPSULATION_KEY_LEN);
        let (ct, k) = encapsulate(&ek, &SecretBytes([7; 32])).unwrap();
        let ct = ml_kem::Ciphertext::<ml_kem::MlKem768>::try_from(&ct[..]).unwrap();
        assert_eq!(dk.decapsulate(&ct).as_slice(), &k.0);
        // Wrong sizes, and a key whose coefficients are not reduced mod q
        // (FIPS 203 7.2, the modulus check).
        assert!(encapsulate(&ek[1..], &SecretBytes([7; 32])).is_none());
        assert!(encapsulate(&[ek.as_slice(), &[0]].concat(), &SecretBytes([7; 32])).is_none());
        assert!(encapsulate(&[0xff; ENCAPSULATION_KEY_LEN], &SecretBytes([7; 32])).is_none());
    }

    /// Audit of ADR 0068, L1: `ml-kem` left G's output on the stack, the
    /// shared key and the randomness r from which it can be recomputed.
    /// G(m || H(ek)) = (K, r), FIPS 203 algorithm 17.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn encapsulation_leaves_nothing_on_the_stack() {
        let (_, ek) = key_pair();
        let m: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
        let h = sha3::Sha3_256::digest(&ek);
        let mut g = sha3::Sha3_512::new();
        g.update(m);
        g.update(h);
        let g = g.finalize();
        let (k, r) = g.split_at(32);
        const DEPTH: usize = 64 * 1024;
        let top: usize;
        // SAFETY: reads rsp.
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) top, options(nomem, nostack)) };
        for a in top - DEPTH..top - 256 {
            // SAFETY: the thread's stack, below every live frame.
            unsafe { core::ptr::write_volatile(a as *mut u8, 0) };
        }
        let (_, shared) = core::hint::black_box(encapsulate(&ek, &SecretBytes(m)).unwrap());
        assert_eq!(&shared.0, k);
        let stack: std::vec::Vec<u8> = (top - DEPTH..top - 256)
            // SAFETY: as above.
            .map(|a| unsafe { core::ptr::read_volatile(a as *const u8) })
            .collect();
        for (name, secret) in [("m", &m[..]), ("K", k), ("r", r)] {
            assert!(
                !secret.windows(8).any(|w| stack.windows(8).any(|s| s == w)),
                "{name} left on the stack"
            );
        }
    }
}
