// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Wycheproof vectors through `aricode-crypto` (ADR 0066): every
//! implementation, adopted or ours, must give the expected answer on every
//! case its parameters cover. "Acceptable" cases (either answer allowed)
//! only have to not panic. A file whose cases all fall outside the
//! supported parameters fails too, so a change cannot skip it silently.

use aricode_crypto::{aead, ecdsa, ed25519, hkdf, hmac, rsa, sha256, sha512, x25519, Hash};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/wycheproof/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
}

fn h(v: &Value) -> Vec<u8> {
    let s = v.as_str().unwrap_or("");
    let mut out = vec![0u8; s.len() / 2];
    aricode_crypto::unhex(s, &mut out).expect("hex");
    out
}

/// Runs `f` on every case of `file`: `Some(accepted)` or `None` when the
/// parameters are not supported. Returns how many cases were judged.
fn run(file: &str, mut f: impl FnMut(&Value, &Value) -> Option<bool>) -> usize {
    let doc = load(file);
    let (mut judged, mut wrong) = (0, Vec::new());
    for g in doc["testGroups"].as_array().expect("groups") {
        for t in g["tests"].as_array().expect("tests") {
            let expected = t["result"].as_str().expect("result");
            let Some(accepted) = f(g, t) else { continue };
            judged += 1;
            match (expected, accepted) {
                ("acceptable", _) | ("valid", true) | ("invalid", false) => {}
                _ => wrong.push(format!("tcId {} ({expected})", t["tcId"])),
            }
        }
    }
    assert!(wrong.is_empty(), "{file}: wrong answers: {wrong:?}");
    assert!(judged > 0, "{file}: no case judged");
    judged
}

#[test]
fn aes_gcm() {
    let n = run("aes_gcm_test.json", |_, t| {
        let (key, iv, aad, msg, ct, tag) = (
            h(&t["key"]),
            h(&t["iv"]),
            h(&t["aad"]),
            h(&t["msg"]),
            h(&t["ct"]),
            h(&t["tag"]),
        );
        if iv.len() != 12 || tag.len() != 16 {
            return None;
        }
        let gcm = aead::AesGcm::new(&key)?;
        let nonce: [u8; 12] = iv.try_into().unwrap();
        let tag: [u8; 16] = tag.try_into().unwrap();
        let mut buf = ct.clone();
        let opened = gcm.open(&nonce, &aad, &mut buf, &tag);
        if !opened {
            assert_eq!(buf, ct, "a failed open must leave the data untouched");
            return Some(false);
        }
        let mut sealed = msg.clone();
        let t2 = gcm.seal(&nonce, &aad, &mut sealed);
        Some(buf == msg && sealed == ct && t2 == tag)
    });
    assert!(n >= 130, "{n}");
}

#[test]
fn chacha20_poly1305() {
    let n = run("chacha20_poly1305_test.json", |_, t| {
        let (key, iv, aad, msg, ct, tag) = (
            h(&t["key"]),
            h(&t["iv"]),
            h(&t["aad"]),
            h(&t["msg"]),
            h(&t["ct"]),
            h(&t["tag"]),
        );
        if iv.len() != 12 || key.len() != 32 || tag.len() != 16 {
            return None;
        }
        let key: [u8; 32] = key.try_into().unwrap();
        let nonce: [u8; 12] = iv.try_into().unwrap();
        let tag: [u8; 16] = tag.try_into().unwrap();
        let mut buf = ct.clone();
        if !aead::chacha20poly1305_open(&key, &nonce, &aad, &mut buf, &tag) {
            assert_eq!(buf, ct, "a failed open must leave the data untouched");
            return Some(false);
        }
        let mut sealed = msg.clone();
        let t2 = aead::chacha20poly1305_seal(&key, &nonce, &aad, &mut sealed);
        Some(buf == msg && sealed == ct && t2 == tag)
    });
    assert!(n >= 300, "{n}");
}

#[test]
fn ed25519() {
    let n = run("ed25519_test.json", |g, t| {
        let (pk, msg, sig) = (h(&g["publicKey"]["pk"]), h(&t["msg"]), h(&t["sig"]));
        let (Ok(pk), Ok(sig)) = (<[u8; 32]>::try_from(pk), <[u8; 64]>::try_from(sig)) else {
            return Some(false);
        };
        Some(ed25519::verify(&pk, &msg, &sig))
    });
    assert!(n >= 200, "{n}");
}

#[test]
fn x25519() {
    let n = run("x25519_test.json", |_, t| {
        let (public, private, shared) = (h(&t["public"]), h(&t["private"]), h(&t["shared"]));
        let (Ok(p), Ok(k)) = (<[u8; 32]>::try_from(public), <[u8; 32]>::try_from(private)) else {
            return Some(false);
        };
        let out = x25519::scalar_mult(&k, &p);
        // The all-zero output of a low-order point is refused by `shared`.
        assert_eq!(x25519::shared(&k, &p).is_none(), out == [0; 32]);
        Some(out[..] == shared[..])
    });
    assert!(n >= 500, "{n}");
}

fn ecdsa_file(file: &str, curve: ecdsa::Curve) -> usize {
    run(file, |g, t| {
        let (pk, msg, sig) = (
            h(&g["publicKey"]["uncompressed"]),
            h(&t["msg"]),
            h(&t["sig"]),
        );
        let digest: Vec<u8> = match g["sha"].as_str() {
            Some("SHA-256") => sha256::sha256(&msg).to_vec(),
            Some("SHA-384") => sha512::sha384(&msg).to_vec(),
            _ => return None,
        };
        Some(
            ecdsa::parse_der_signature(&sig)
                .is_some_and(|(r, s)| ecdsa::verify(curve, &pk, &digest, r, s)),
        )
    })
}

#[test]
fn ecdsa_p256_p384() {
    assert!(ecdsa_file("ecdsa_secp256r1_sha256_test.json", ecdsa::Curve::P256) >= 480);
    assert!(ecdsa_file("ecdsa_secp384r1_sha256_test.json", ecdsa::Curve::P384) >= 470);
    assert!(ecdsa_file("ecdsa_secp384r1_sha384_test.json", ecdsa::Curve::P384) >= 500);
}

fn rsa_file(file: &str, pss: bool) -> usize {
    run(file, |g, t| {
        let (n, e) = (
            h(&g["publicKey"]["modulus"]),
            h(&g["publicKey"]["publicExponent"]),
        );
        let (msg, sig) = (h(&t["msg"]), h(&t["sig"]));
        let sha = g["sha"].as_str()?;
        let (hash, digest): (Hash, Vec<u8>) = match sha {
            "SHA-256" => (Hash::Sha256, sha256::sha256(&msg).to_vec()),
            "SHA-384" => (Hash::Sha384, sha512::sha384(&msg).to_vec()),
            "SHA-512" => (Hash::Sha512, sha512::sha512(&msg).to_vec()),
            _ => return None,
        };
        if pss {
            // Ours: MGF1 with the message hash and a salt of its length.
            let salt = g["sLen"].as_u64()? as usize;
            if g["mgfSha"].as_str() != Some(sha) || salt != hash.len() {
                return None;
            }
            Some(rsa::verify_pss(&n, &e, hash, &digest, &sig))
        } else {
            Some(rsa::verify_pkcs1v15(&n, &e, hash, &digest, &sig))
        }
    })
}

#[test]
fn rsa_pkcs1_and_pss() {
    for file in [
        "rsa_signature_2048_sha256_test.json",
        "rsa_signature_2048_sha384_test.json",
        "rsa_signature_2048_sha512_test.json",
        "rsa_signature_3072_sha256_test.json",
        "rsa_signature_4096_sha256_test.json",
    ] {
        assert!(rsa_file(file, false) >= 250, "{file}");
    }
    for file in [
        "rsa_pss_2048_sha256_mgf1_32_test.json",
        "rsa_pss_2048_sha384_mgf1_48_test.json",
        "rsa_pss_4096_sha512_mgf1_64_test.json",
    ] {
        assert!(rsa_file(file, true) >= 100, "{file}");
    }
}

#[test]
fn hmac_sha256_sha384() {
    for (file, f) in [
        (
            "hmac_sha256_test.json",
            (|k: &[u8], m: &[u8]| hmac::hmac_sha256(k, &[m]).to_vec())
                as fn(&[u8], &[u8]) -> Vec<u8>,
        ),
        ("hmac_sha384_test.json", |k, m| {
            hmac::hmac_sha384(k, &[m]).to_vec()
        }),
    ] {
        let n = run(file, |g, t| {
            let bytes = g["tagSize"].as_u64()? as usize / 8;
            let (key, msg, tag) = (h(&t["key"]), h(&t["msg"]), h(&t["tag"]));
            Some(f(&key, &msg)[..bytes] == tag[..])
        });
        assert!(n >= 170, "{file}");
    }
}

#[test]
fn hkdf_sha256_sha384() {
    for (file, hash_len) in [
        ("hkdf_sha256_test.json", 32usize),
        ("hkdf_sha384_test.json", 48),
    ] {
        let n = run(file, |_, t| {
            let (ikm, salt, info, okm) = (h(&t["ikm"]), h(&t["salt"]), h(&t["info"]), h(&t["okm"]));
            let size = match &t["size"] {
                Value::String(s) => s.parse().ok()?,
                v => v.as_u64()? as usize,
            };
            // Ours asks the caller for at most 255 hash lengths of output.
            if size > 255 * hash_len {
                return None;
            }
            let mut out = vec![0u8; size];
            if hash_len == 32 {
                hkdf::expand_sha256(&hkdf::extract_sha256(&salt, &ikm), &info, &mut out);
            } else {
                hkdf::expand_sha384(&hkdf::extract_sha384(&salt, &ikm), &info, &mut out);
            }
            Some(out == okm)
        });
        assert!(n >= 80, "{file}");
    }
}
