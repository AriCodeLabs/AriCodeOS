//! ECDSA signature verification (FIPS 186-4 section 6.4) on the NIST
//! curves P-256 and P-384, plus the DER `Ecdsa-Sig-Value` encoding used
//! by X.509 and TLS. Verification only, so it is not constant time.

use crate::p_curves::{from_be_bytes, geq, Curve as CurveArith, CurveParams, Modulus, P256, P384};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

/// `point`: SEC1 uncompressed public key (0x04 || X || Y). `digest`: hash
/// of the message (only its leftmost bits up to the order's size are
/// used, per FIPS 186-4). `r`, `s`: big-endian unsigned integers (any
/// length; leading zeros allowed).
pub fn verify(curve: Curve, point: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
    match curve {
        Curve::P256 => verify_on(&P256, point, digest, r, s),
        Curve::P384 => verify_on(&P384, point, digest, r, s),
    }
    .is_some()
}

fn verify_on<const N: usize>(
    params: &CurveParams<N>,
    point: &[u8],
    digest: &[u8],
    r: &[u8],
    s: &[u8],
) -> Option<()> {
    let len = 8 * N;
    if point.len() != 1 + 2 * len || point[0] != 0x04 {
        return None;
    }
    let curve = CurveArith::new(params);
    let q = curve.affine(
        &from_be_bytes(&point[1..1 + len])?,
        &from_be_bytes(&point[1 + len..])?,
    )?;

    let n = &params.n;
    let r = from_be_bytes::<N>(r)?;
    let s = from_be_bytes::<N>(s)?;
    let zero = [0u64; N];
    if r == zero || s == zero || geq(&r, n) || geq(&s, n) {
        return None;
    }

    // The orders are exactly 8 * len bits, so truncation is by bytes and
    // the result is below 2n.
    let fn_ = Modulus::new(*n);
    let e = fn_.reduce(&from_be_bytes(&digest[..digest.len().min(len)])?);
    let w = fn_.inv(&fn_.to_mont(&s));
    // Montgomery product of a normal value and a Montgomery value is normal.
    let u1 = fn_.mul(&e, &w);
    let u2 = fn_.mul(&r, &w);

    let g = curve.affine(&params.gx, &params.gy)?;
    let x = curve.affine_x(&curve.double_mul(&u1, &g, &u2, &q))?;
    // x < p < 2n.
    (fn_.reduce(&x) == r).then_some(())
}

/// DER `Ecdsa-Sig-Value ::= SEQUENCE { r INTEGER, s INTEGER }` -> (r, s)
/// as big-endian magnitudes (the sign byte, if any, is stripped). Rejects
/// non-minimal encodings, negative integers and trailing data.
pub fn parse_der_signature(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let (body, rest) = der_element(der, 0x30)?;
    if !rest.is_empty() {
        return None;
    }
    let (r, body) = der_element(body, 0x02)?;
    let (s, body) = der_element(body, 0x02)?;
    if !body.is_empty() {
        return None;
    }
    Some((positive_integer(r)?, positive_integer(s)?))
}

/// One DER element with the given tag: (contents, remaining input).
fn der_element(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&t, input) = input.split_first()?;
    if t != tag {
        return None;
    }
    let (&first, mut input) = input.split_first()?;
    let len = match first {
        0..=0x7f => first as usize,
        0x81 => {
            let (&l, rest) = input.split_first()?;
            if l < 0x80 {
                return None;
            }
            input = rest;
            l as usize
        }
        0x82 => {
            if input.len() < 2 {
                return None;
            }
            let l = (input[0] as usize) << 8 | input[1] as usize;
            if l < 0x100 {
                return None;
            }
            input = &input[2..];
            l
        }
        _ => return None,
    };
    if input.len() < len {
        return None;
    }
    Some(input.split_at(len))
}

/// Contents of a DER INTEGER that must be non-negative and minimally
/// encoded; returns the magnitude without the sign byte.
fn positive_integer(c: &[u8]) -> Option<&[u8]> {
    match c {
        [] => None,
        [b, ..] if b & 0x80 != 0 => None,
        [0, b, ..] if b & 0x80 == 0 => None,
        [0, rest @ ..] if !rest.is_empty() => Some(rest),
        _ => Some(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keys and signatures generated with OpenSSL 3.5:
    //   openssl ecparam -name prime256v1|secp384r1 -genkey -noout -out key.pem
    //   openssl dgst -sha256|-sha384|-sha512 -sign key.pem -out sig msg
    const MSG: &[u8] = b"AriCodeOS ECDSA test message\n";

    const P256_PUB: &str = "04ec78df7a802bdbbef48a59d0a3c24ef07c9424de2567ec32b4aa226f96c211f4\
        0e64c162fc2b2a01b0c92a163524bd21670240c7b3a18646c80c78e806a6399f";
    const P256_SHA256: &str = "30460221008bf475378095282482686a881a85003633aab63d57531b5a8bb4df3a\
        57b712ed022100fa05eccbb0451ec1085d0d2a3e59bc743d5aaf72a9395cc886c53176b1eddc32";
    // Digest shorter than / longer than the order exercises truncation.
    const P256_SHA384: &str = "3044022007e303c5c63ca879107d988b487ce9160376620881623ee397e2b5ea84\
        9c84f602200fdea64cd6c1e2f022baeb8b59eb74478bcc01f9c610f568831b1ae2061ea29a";
    const P256_SHA512: &str = "3045022000b53a20b035103d3c50bd8c35ec4237ed79b821f38e09b31d87f3d21e\
        67152f0221008e339fd6a035889b675229d07387c3942474b9825d2e9149babe296d729fdcb6";

    const P384_PUB: &str = "0489e1ff5e3c69b565ac7ed329adf0e116a333d2015a594c4a5a6d4cb79dd1a4165f\
        a2958cb99d1824b3f99cf5f444c9fae9b7da05e8f9a50b8d8b03595d3bff1107b922fb3249d99477c7995536c\
        008d4dc4f7f5df5035b32e05e4d02918d4061";
    const P384_SHA256: &str = "3065023037697ce08b831acaefaa13d9f5347639ec23701d5369daf99d85d8088f\
        bcc03fa68c8e1ba8762b83402a675a2f5cdd9c023100ffd7389e76d0aab08496916fb547364e1b9a31f74707a\
        037ba319f5b6d8e1ad2894842400fd1b0c3aee6ec63d6235e4a";
    const P384_SHA384: &str = "3066023100b0ea1680c6f3d904546b7ec97c25abaa863eb4cf218b73e9959946c8\
        762ac48bb58237a7946fbf3d5d83097cdc0b8fef023100adde2689763a3b130d4677bdbe7090b26728251d38f\
        3f08cfa9899324237083d56e5300ebf0dfa9dfddae395792078a6";
    const P384_SHA512: &str = "306402306cb3ae61248df9a8c9c5e62f71f19ebc0dcfc725d6c1f4c519cb91df3b\
        e92f2cfbf3118309cf2d69e96838740ff50f6202302bb52cfb8dd268532b477c0975885e5dacf6390a981c0ff\
        ecf19998be60340b8743da42c4a28730f8121a7f88b434f20";

    fn bytes<'a>(hex: &str, buf: &'a mut [u8; 128]) -> &'a mut [u8] {
        let out = &mut buf[..hex.len() / 2];
        crate::unhex(hex, out).unwrap();
        out
    }

    /// Digest of MSG (the SHA-384 one is the `openssl dgst` output).
    fn digest(kind: &str, out: &mut [u8; 64]) -> usize {
        match kind {
            "sha256" => {
                out[..32].copy_from_slice(&crate::sha256::sha256(MSG));
                32
            }
            "sha384" => {
                // `openssl dgst -sha384 msg`.
                crate::unhex(
                    "f164c0dbe6f35d43da20b1c96a3498afb412e5282039adf8fb9a30b95f008243\
                     d06881557aad581bdd23e0b2b315d6eb",
                    &mut out[..48],
                )
                .unwrap();
                48
            }
            _ => {
                out.copy_from_slice(&crate::sha512::sha512(MSG));
                64
            }
        }
    }

    fn check(curve: Curve, public: &str, sig: &str, hash: &str) -> bool {
        let (mut pb, mut sb, mut d) = ([0u8; 128], [0u8; 128], [0u8; 64]);
        let point = bytes(public, &mut pb);
        let sig = bytes(sig, &mut sb);
        let dl = digest(hash, &mut d);
        let (r, s) = parse_der_signature(sig).expect("DER signature");
        verify(curve, point, &d[..dl], r, s)
    }

    #[test]
    fn openssl_signatures_verify() {
        for (sig, hash) in [
            (P256_SHA256, "sha256"),
            (P256_SHA384, "sha384"),
            (P256_SHA512, "sha512"),
        ] {
            assert!(check(Curve::P256, P256_PUB, sig, hash), "P-256 {hash}");
        }
        for (sig, hash) in [
            (P384_SHA256, "sha256"),
            (P384_SHA384, "sha384"),
            (P384_SHA512, "sha512"),
        ] {
            assert!(check(Curve::P384, P384_PUB, sig, hash), "P-384 {hash}");
        }
    }

    #[test]
    fn wrong_digest_key_or_curve_fails() {
        assert!(!check(Curve::P256, P256_PUB, P256_SHA256, "sha512"));
        assert!(!check(Curve::P384, P384_PUB, P384_SHA384, "sha256"));
        assert!(!check(Curve::P384, P256_PUB, P256_SHA256, "sha256"));
        // A valid point of the wrong key: the generator.
        let g = "046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296\
                 4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
        assert!(!check(Curve::P256, g, P256_SHA256, "sha256"));
    }

    #[test]
    fn tampered_signature_or_point_fails() {
        let (mut pb, mut sb, mut d) = ([0u8; 128], [0u8; 128], [0u8; 64]);
        let point = bytes(P256_PUB, &mut pb);
        let sig = bytes(P256_SHA256, &mut sb);
        let dl = digest("sha256", &mut d);
        let d = &d[..dl];
        let (r, s) = parse_der_signature(sig).unwrap();
        let (mut r2, mut s2) = ([0u8; 32], [0u8; 32]);
        r2.copy_from_slice(r);
        s2.copy_from_slice(s);
        assert!(verify(Curve::P256, point, d, &r2, &s2));

        // Leading zeros are accepted.
        let mut padded = [0u8; 40];
        padded[8..].copy_from_slice(&r2);
        assert!(verify(Curve::P256, point, d, &padded, &s2));

        let mut bad = r2;
        bad[31] ^= 1;
        assert!(!verify(Curve::P256, point, d, &bad, &s2));
        let mut bad = s2;
        bad[0] ^= 0x40;
        assert!(!verify(Curve::P256, point, d, &r2, &bad));
        // r = 0, s = 0, r = n, oversize.
        assert!(!verify(Curve::P256, point, d, &[0], &s2));
        assert!(!verify(Curve::P256, point, d, &r2, &[]));
        let mut n = [0u8; 32];
        crate::unhex(
            "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
            &mut n,
        )
        .unwrap();
        assert!(!verify(Curve::P256, point, d, &n, &s2));
        assert!(!verify(Curve::P256, point, d, &[1; 33], &s2));

        // Off-curve point, compressed / truncated encodings.
        let mut off = [0u8; 65];
        off.copy_from_slice(point);
        off[64] ^= 1;
        assert!(!verify(Curve::P256, &off, d, &r2, &s2));
        let mut wrong_tag = [0u8; 65];
        wrong_tag.copy_from_slice(point);
        wrong_tag[0] = 0x02;
        assert!(!verify(Curve::P256, &wrong_tag, d, &r2, &s2));
        assert!(!verify(Curve::P256, &point[..33], d, &r2, &s2));
        // Coordinate >= p.
        let mut big = [0xffu8; 65];
        big[0] = 4;
        assert!(!verify(Curve::P256, &big, d, &r2, &s2));
    }

    #[test]
    fn der_parsing() {
        assert_eq!(
            parse_der_signature(&[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x7f]),
            Some((&[0x01][..], &[0x7f][..]))
        );
        // The sign byte is stripped.
        assert_eq!(
            parse_der_signature(&[0x30, 0x07, 0x02, 0x02, 0x00, 0x80, 0x02, 0x01, 0x01]),
            Some((&[0x80][..], &[0x01][..]))
        );
        for bad in [
            &[0x30, 0x06, 0x02, 0x01, 0x81, 0x02, 0x01, 0x01][..], // negative
            &[0x30, 0x07, 0x02, 0x02, 0x00, 0x01, 0x02, 0x01, 0x01], // non-minimal
            &[0x30, 0x05, 0x02, 0x00, 0x02, 0x01, 0x01],           // empty integer
            &[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00], // trailing data
            &[0x30, 0x07, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00], // inner trailing
            &[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x02, 0x01],     // truncated
            &[0x31, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01],     // wrong tag
            &[0x30, 0x81, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01], // long form < 128
            &[],
        ] {
            assert!(parse_der_signature(bad).is_none(), "{bad:02x?}");
        }
        // A 128-byte body needs the long-form length.
        let mut long = [0u8; 3 + 4 + 2 * 62];
        long[..3].copy_from_slice(&[0x30, 0x81, 0x80]);
        long[3..5].copy_from_slice(&[0x02, 0x3e]);
        long[5] = 0x01;
        long[67..69].copy_from_slice(&[0x02, 0x3e]);
        long[69] = 0x02;
        let (r, s) = parse_der_signature(&long).unwrap();
        assert_eq!((r.len(), r[0], s.len(), s[0]), (62, 1, 62, 2));
    }
}
