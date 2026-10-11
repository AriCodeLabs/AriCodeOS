//! TLS 1.3 handshake messages (RFC 8446 section 4) as the client needs
//! them: ClientHello out; ServerHello, EncryptedExtensions,
//! CertificateRequest, Certificate, CertificateVerify, Finished,
//! NewSessionTicket and KeyUpdate in.

use crate::keys::Suite;
use crate::Error;
use alloc::vec::Vec;
use aricode_crypto::ecdsa::{self, Curve};
use aricode_crypto::{ed25519, rsa, Hash};
use aricode_x509::{Certificate, PublicKey};

pub const CLIENT_HELLO: u8 = 1;
pub const SERVER_HELLO: u8 = 2;
pub const NEW_SESSION_TICKET: u8 = 4;
pub const ENCRYPTED_EXTENSIONS: u8 = 8;
pub const CERTIFICATE: u8 = 11;
pub const CERTIFICATE_REQUEST: u8 = 13;
pub const CERTIFICATE_VERIFY: u8 = 15;
pub const FINISHED: u8 = 20;
pub const KEY_UPDATE: u8 = 24;

const EXT_SERVER_NAME: u16 = 0;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_KEY_SHARE: u16 = 51;

const TLS13: u16 = 0x0304;
const X25519: u16 = 0x001d;

const ECDSA_P256_SHA256: u16 = 0x0403;
const ECDSA_P384_SHA384: u16 = 0x0503;
const RSA_PSS_RSAE_SHA256: u16 = 0x0804;
const RSA_PSS_RSAE_SHA384: u16 = 0x0805;
const RSA_PSS_RSAE_SHA512: u16 = 0x0806;
const ED25519: u16 = 0x0807;
const SIGNATURE_SCHEMES: [u16; 6] = [
    ECDSA_P256_SHA256,
    ECDSA_P384_SHA384,
    RSA_PSS_RSAE_SHA256,
    RSA_PSS_RSAE_SHA384,
    RSA_PSS_RSAE_SHA512,
    ED25519,
];

/// SHA-256("HelloRetryRequest"): the ServerHello.random of a HelloRetryRequest.
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91,
    0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

const DECODE: Error = Error::Protocol("malformed handshake message");

/// Reader for TLS presentation-language vectors.
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.data.len() < n {
            return Err(DECODE);
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> Result<usize, Error> {
        let b = self.bytes(3)?;
        Ok((b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize)
    }

    pub fn vec8(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u8()? as usize;
        self.bytes(n)
    }

    pub fn vec16(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u16()? as usize;
        self.bytes(n)
    }

    pub fn vec24(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u24()?;
        self.bytes(n)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Fails unless everything was consumed.
    pub fn end(&self) -> Result<(), Error> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(DECODE)
        }
    }
}

/// Walks an extension block, rejecting duplicates; `f(type, body)`.
fn extensions<'a>(
    data: &'a [u8],
    mut f: impl FnMut(u16, &'a [u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut r = Reader::new(data);
    let mut seen: Vec<u16> = Vec::new();
    while !r.is_empty() {
        let typ = r.u16()?;
        let body = r.vec16()?;
        if seen.contains(&typ) {
            return Err(Error::Protocol("duplicate extension"));
        }
        seen.push(typ);
        f(typ, body)?;
    }
    Ok(())
}

/// Wraps a handshake body with its type and 24-bit length.
pub fn message(typ: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.push(typ);
    m.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    m.extend_from_slice(body);
    m
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn put_ext(v: &mut Vec<u8>, typ: u16, body: &[u8]) {
    put_u16(v, typ);
    put_u16(v, body.len() as u16);
    v.extend_from_slice(body);
}

/// IP literals must not be sent as SNI (RFC 6066 section 3).
fn is_ip_literal(host: &str) -> bool {
    host.contains(':') || host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// The ClientHello handshake message.
pub fn client_hello(
    random: &[u8; 32],
    session_id: &[u8],
    key_share: &[u8; 32],
    host: &str,
) -> Vec<u8> {
    let mut b = Vec::new();
    put_u16(&mut b, 0x0303);
    b.extend_from_slice(random);
    b.push(session_id.len() as u8);
    b.extend_from_slice(session_id);
    put_u16(&mut b, 2 * Suite::ALL.len() as u16);
    for s in Suite::ALL {
        put_u16(&mut b, s.id());
    }
    b.extend_from_slice(&[1, 0]); // null compression only

    let mut ext = Vec::new();
    if !host.is_empty() && !is_ip_literal(host) {
        let mut sni = Vec::new();
        put_u16(&mut sni, host.len() as u16 + 3);
        sni.push(0); // host_name
        put_u16(&mut sni, host.len() as u16);
        sni.extend_from_slice(host.as_bytes());
        put_ext(&mut ext, EXT_SERVER_NAME, &sni);
    }
    put_ext(&mut ext, EXT_SUPPORTED_VERSIONS, &[2, 0x03, 0x04]);
    put_ext(&mut ext, EXT_SUPPORTED_GROUPS, &[0, 2, 0x00, 0x1d]);
    let mut sig = Vec::new();
    put_u16(&mut sig, 2 * SIGNATURE_SCHEMES.len() as u16);
    for s in SIGNATURE_SCHEMES {
        put_u16(&mut sig, s);
    }
    put_ext(&mut ext, EXT_SIGNATURE_ALGORITHMS, &sig);
    let mut ks = Vec::new();
    put_u16(&mut ks, 36);
    put_u16(&mut ks, X25519);
    put_u16(&mut ks, 32);
    ks.extend_from_slice(key_share);
    put_ext(&mut ext, EXT_KEY_SHARE, &ks);

    put_u16(&mut b, ext.len() as u16);
    b.extend_from_slice(&ext);
    message(CLIENT_HELLO, &b)
}

/// What the client takes from a ServerHello.
#[derive(Debug, PartialEq, Eq)]
pub struct ServerHello {
    pub suite: Suite,
    pub key_share: [u8; 32],
}

/// Parses a ServerHello body. A HelloRetryRequest (we only offer x25519,
/// so it can only ask for a group we lack or carry a cookie) is reported
/// as unsupported.
pub fn server_hello(body: &[u8], session_id: &[u8]) -> Result<ServerHello, Error> {
    let mut r = Reader::new(body);
    let legacy_version = r.u16()?;
    let random = r.bytes(32)?;
    let echo = r.vec8()?;
    let suite_id = r.u16()?;
    let compression = r.u8()?;
    let exts = if r.is_empty() { &[][..] } else { r.vec16()? };
    r.end()?;
    let mut version = None;
    let mut share = None;
    extensions(exts, |typ, data| {
        let mut e = Reader::new(data);
        match typ {
            EXT_SUPPORTED_VERSIONS => version = Some(e.u16()?),
            // HelloRetryRequest carries only the selected group here.
            EXT_KEY_SHARE if data.len() == 2 => share = Some((e.u16()?, &[][..])),
            EXT_KEY_SHARE => share = Some((e.u16()?, e.vec16()?)),
            0x002c if random == HRR_RANDOM => {
                e.bytes(data.len())?; // cookie
            }
            _ => return Err(Error::Protocol("unexpected ServerHello extension")),
        }
        e.end()
    })?;
    if version != Some(TLS13) || legacy_version != 0x0303 {
        return Err(Error::Unsupported("server does not speak TLS 1.3"));
    }
    if random == HRR_RANDOM {
        return Err(Error::Unsupported("HelloRetryRequest"));
    }
    if echo != session_id || compression != 0 {
        return Err(Error::Protocol("bad ServerHello"));
    }
    let suite = Suite::from_id(suite_id).ok_or(Error::Protocol("cipher suite not offered"))?;
    let key_share = match share {
        Some((X25519, key)) => key
            .try_into()
            .map_err(|_| Error::Protocol("bad x25519 key share"))?,
        Some(_) => return Err(Error::Protocol("key share group not offered")),
        None => return Err(Error::Unsupported("no key share (PSK-only handshake)")),
    };
    Ok(ServerHello { suite, key_share })
}

/// Checks the EncryptedExtensions structure (we requested nothing that
/// needs handling).
pub fn encrypted_extensions(body: &[u8]) -> Result<(), Error> {
    let mut r = Reader::new(body);
    let exts = r.vec16()?;
    r.end()?;
    extensions(exts, |typ, _| match typ {
        EXT_SUPPORTED_VERSIONS | EXT_KEY_SHARE => Err(Error::Protocol(
            "ServerHello extension in EncryptedExtensions",
        )),
        _ => Ok(()),
    })
}

/// CertificateRequest: returns its certificate_request_context.
pub fn certificate_request(body: &[u8]) -> Result<Vec<u8>, Error> {
    let mut r = Reader::new(body);
    let context = r.vec8()?.to_vec();
    extensions(r.vec16()?, |_, _| Ok(()))?;
    r.end()?;
    Ok(context)
}

/// Our (empty) Certificate in answer to a CertificateRequest.
pub fn empty_certificate(context: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.push(context.len() as u8);
    b.extend_from_slice(context);
    b.extend_from_slice(&[0, 0, 0]);
    message(CERTIFICATE, &b)
}

/// Certificate: the DER certificates, leaf first.
pub fn certificate(body: &[u8]) -> Result<Vec<&[u8]>, Error> {
    let mut r = Reader::new(body);
    if !r.vec8()?.is_empty() {
        return Err(Error::Protocol("certificate_request_context must be empty"));
    }
    let mut list = Reader::new(r.vec24()?);
    r.end()?;
    let mut chain = Vec::new();
    while !list.is_empty() {
        let der = list.vec24()?;
        list.vec16()?; // per-certificate extensions (OCSP, SCT): ignored
        if der.is_empty() {
            return Err(DECODE);
        }
        chain.push(der);
    }
    if chain.is_empty() {
        return Err(Error::Protocol("empty server certificate"));
    }
    Ok(chain)
}

/// The content the server signs in CertificateVerify.
pub fn certificate_verify_content(transcript_hash: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(64 + 34 + transcript_hash.len());
    c.resize(64, 0x20);
    c.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
    c.extend_from_slice(transcript_hash);
    c
}

fn digest(hash: Hash, data: &[u8]) -> ([u8; 64], usize) {
    let mut out = [0u8; 64];
    let n = hash.digest(data, &mut out);
    (out, n)
}

/// Checks CertificateVerify (`body`) against the leaf certificate.
pub fn certificate_verify(body: &[u8], leaf: &[u8], transcript_hash: &[u8]) -> Result<(), Error> {
    let mut r = Reader::new(body);
    let scheme = r.u16()?;
    let sig = r.vec16()?;
    r.end()?;
    let cert = Certificate::parse(leaf).map_err(Error::Certificate)?;
    let content = certificate_verify_content(transcript_hash);
    let ecdsa_ok = |curve: Curve, point: &[u8], hash: Hash| {
        let (d, n) = digest(hash, &content);
        ecdsa::parse_der_signature(sig)
            .is_some_and(|(r, s)| ecdsa::verify(curve, point, &d[..n], r, s))
    };
    let ok = match (scheme, &cert.public_key) {
        (
            ECDSA_P256_SHA256,
            PublicKey::Ec {
                curve: Curve::P256,
                point,
            },
        ) => ecdsa_ok(Curve::P256, point, Hash::Sha256),
        (
            ECDSA_P384_SHA384,
            PublicKey::Ec {
                curve: Curve::P384,
                point,
            },
        ) => ecdsa_ok(Curve::P384, point, Hash::Sha384),
        (
            RSA_PSS_RSAE_SHA256 | RSA_PSS_RSAE_SHA384 | RSA_PSS_RSAE_SHA512,
            PublicKey::Rsa { n, e },
        ) => {
            let hash = match scheme {
                RSA_PSS_RSAE_SHA256 => Hash::Sha256,
                RSA_PSS_RSAE_SHA384 => Hash::Sha384,
                _ => Hash::Sha512,
            };
            let (d, len) = digest(hash, &content);
            rsa::verify_pss(n, e, hash, &d[..len], sig)
        }
        (ED25519, PublicKey::Ed25519(pk)) => {
            <&[u8; 64]>::try_from(sig).is_ok_and(|s| ed25519::verify(pk, &content, s))
        }
        _ => {
            return Err(Error::Protocol(
                "signature scheme not offered or not matching the certificate key",
            ))
        }
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Protocol("bad CertificateVerify signature"))
    }
}

/// KeyUpdate: true if the server asks us to update our keys too.
pub fn key_update(body: &[u8]) -> Result<bool, Error> {
    match body {
        [0] => Ok(false),
        [1] => Ok(true),
        _ => Err(Error::Protocol("bad KeyUpdate")),
    }
}

/// NewSessionTicket: checked for structure and ignored (no resumption).
pub fn new_session_ticket(body: &[u8]) -> Result<(), Error> {
    let mut r = Reader::new(body);
    r.bytes(8)?; // lifetime, age_add
    r.vec8()?; // nonce
    if r.vec16()?.is_empty() {
        return Err(DECODE);
    }
    extensions(r.vec16()?, |_, _| Ok(()))?;
    r.end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rfc8448::{h, SERVER_HELLO};
    use alloc::vec;

    #[test]
    fn client_hello_layout() {
        let m = client_hello(&[0xaa; 32], &[0xbb; 32], &[0xcc; 32], "example.com");
        assert_eq!(m[0], CLIENT_HELLO);
        let mut r = Reader::new(&m[1..]);
        let mut body = Reader::new(r.vec24().unwrap());
        r.end().unwrap();
        assert_eq!(body.u16().unwrap(), 0x0303);
        assert_eq!(body.bytes(32).unwrap(), &[0xaa; 32]);
        assert_eq!(body.vec8().unwrap(), &[0xbb; 32]);
        assert_eq!(body.vec16().unwrap(), &[0x13, 0x03, 0x13, 0x01, 0x13, 0x02]);
        assert_eq!(body.vec8().unwrap(), &[0]);
        let mut types = Vec::new();
        extensions(body.vec16().unwrap(), |t, d| {
            types.push(t);
            match t {
                EXT_SERVER_NAME => assert_eq!(&d[5..], b"example.com"),
                EXT_KEY_SHARE => assert_eq!(&d[6..], &[0xcc; 32]),
                EXT_SUPPORTED_VERSIONS => assert_eq!(d, &[2, 3, 4]),
                _ => {}
            }
            Ok(())
        })
        .unwrap();
        body.end().unwrap();
        assert_eq!(types, [0, 43, 10, 13, 51]);

        // No SNI for IP literals.
        for host in ["10.0.2.2", "::1"] {
            let m = client_hello(&[0; 32], &[0; 32], &[0; 32], host);
            assert!(!m.windows(host.len()).any(|w| w == host.as_bytes()));
        }
    }

    #[test]
    fn rfc8448_server_hello() {
        let sh = h(SERVER_HELLO);
        let parsed = server_hello(&sh[4..], &[]).unwrap();
        assert_eq!(parsed.suite, Suite::Aes128GcmSha256);
        assert_eq!(parsed.key_share[..4], [0xc9, 0x82, 0x88, 0x76]);
        // Wrong session id echo.
        assert!(server_hello(&sh[4..], &[1]).is_err());
    }

    /// ServerHello body with the given random, suite and extensions.
    fn sh_body(random: &[u8; 32], suite: u16, exts: &[u8]) -> Vec<u8> {
        let mut b = vec![3, 3];
        b.extend_from_slice(random);
        b.push(0);
        b.extend_from_slice(&suite.to_be_bytes());
        b.push(0);
        b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        b.extend_from_slice(exts);
        b
    }

    #[test]
    fn hello_retry_and_bad_server_hellos() {
        // HelloRetryRequest selecting secp256r1.
        let hrr = sh_body(
            &HRR_RANDOM,
            0x1301,
            &[0, 43, 0, 2, 3, 4, 0, 51, 0, 2, 0, 23],
        );
        assert_eq!(
            server_hello(&hrr, &[]),
            Err(Error::Unsupported("HelloRetryRequest"))
        );
        // TLS 1.2 server (no supported_versions).
        let tls12 = sh_body(&[1; 32], 0xc02f, &[]);
        assert!(matches!(
            server_hello(&tls12, &[]),
            Err(Error::Unsupported(_))
        ));
        // Suite we did not offer.
        let mut ks = vec![0, 51, 0, 36, 0, 0x1d, 0, 32];
        ks.extend_from_slice(&[9; 32]);
        let mut exts = vec![0, 43, 0, 2, 3, 4];
        exts.extend_from_slice(&ks);
        assert!(server_hello(&sh_body(&[1; 32], 0x1304, &exts), &[]).is_err());
        assert!(server_hello(&sh_body(&[1; 32], 0x1303, &exts), &[]).is_ok());
        // Duplicate extension.
        let mut dup = exts.clone();
        dup.extend_from_slice(&[0, 43, 0, 2, 3, 4]);
        assert_eq!(
            server_hello(&sh_body(&[1; 32], 0x1303, &dup), &[]),
            Err(Error::Protocol("duplicate extension"))
        );
    }

    #[test]
    fn certificate_and_small_messages() {
        let mut body = vec![0, 0, 0, 12];
        body.extend_from_slice(&[0, 0, 3, 1, 2, 3, 0, 0, 0, 0, 1, 9]);
        // Second entry has one byte of DER but a truncated extension block.
        assert!(certificate(&body).is_err());
        let body = [0, 0, 0, 8, 0, 0, 3, 1, 2, 3, 0, 0];
        assert_eq!(certificate(&body).unwrap(), vec![&[1u8, 2, 3][..]]);
        assert!(certificate(&[1, 7, 0, 0, 0]).is_err(), "non-empty context");
        assert!(certificate(&[0, 0, 0, 0]).is_err(), "empty chain");
        assert_eq!(
            empty_certificate(&[5]),
            [CERTIFICATE, 0, 0, 5, 1, 5, 0, 0, 0]
        );
        assert_eq!(certificate_request(&[1, 5, 0, 0]).unwrap(), [5]);
        assert_eq!(key_update(&[1]), Ok(true));
        assert!(key_update(&[2]).is_err());
        let content = certificate_verify_content(&[0xab; 32]);
        assert_eq!(content.len(), 64 + 34 + 32);
        assert_eq!(&content[64..97], b"TLS 1.3, server CertificateVerify");
        assert_eq!(content[97], 0);
    }
}
