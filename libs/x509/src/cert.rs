// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Certificate parsing (RFC 5280, the subset the web PKI uses).

use crate::der::{self, explicit, Reader, Tlv};
use crate::{Certificate, Error, PublicKey, SignatureAlgorithm};
use alloc::vec::Vec;
use aricode_crypto::ecdsa::Curve;
use aricode_crypto::Hash;

mod oid {
    pub const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    pub const RSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
    pub const SHA256_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
    pub const SHA384_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
    pub const SHA512_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
    pub const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    pub const P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    pub const P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
    pub const ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    pub const ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
    pub const ED25519: &[u8] = &[0x2b, 0x65, 0x70];
    pub const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    pub const SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
    pub const SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];
    pub const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
    pub const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
    pub const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
    pub const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
    pub const CERTIFICATE_POLICIES: &[u8] = &[0x55, 0x1d, 0x20];
    pub const EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
}

/// keyUsage bit 5 (keyCertSign), in the first content byte.
const KEY_CERT_SIGN: u8 = 0x04;
/// keyUsage bit 0 (digitalSignature), in the first content byte.
const DIGITAL_SIGNATURE: u8 = 0x80;
/// extendedKeyUsage purposes that allow authenticating a TLS server.
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const ANY_EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25, 0x00];
/// GeneralName `dNSName [2] IA5String` (implicit, primitive).
const DNS_NAME: u8 = 0x82;

/// The parts of a certificate shared by [`Certificate`] and trust anchors.
pub(crate) struct Parsed<'a> {
    pub tbs: &'a [u8],
    /// Outer signatureAlgorithm (the AlgorithmIdentifier contents).
    pub signature_algorithm: &'a [u8],
    pub signature: &'a [u8],
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: u64,
    pub not_after: u64,
    pub public_key: PublicKey<'a>,
    pub is_ca: bool,
    pub max_path_len: Option<u32>,
    pub dns_names: Vec<&'a str>,
    pub common_name: Option<&'a str>,
    pub digital_signature: bool,
    pub server_auth: bool,
}

impl<'a> Parsed<'a> {
    pub fn parse(der: &'a [u8]) -> Result<Self, Error> {
        let mut outer = Reader::new(der::single(der, der::SEQUENCE)?);
        let tbs = outer.expect_tlv(der::SEQUENCE)?;
        let signature_algorithm = outer.expect(der::SEQUENCE)?;
        let signature = der::octet_aligned_bits(outer.expect(der::BIT_STRING)?)?;
        outer.finish()?;

        let mut r = Reader::new(tbs.value);
        if let Some(version) = r.optional(explicit(0))? {
            let v = der::small_unsigned(der::single(version, der::INTEGER)?)?;
            if v > 2 {
                return Err(Error::Unsupported);
            }
        }
        r.expect(der::INTEGER)?; // serialNumber
        if r.expect(der::SEQUENCE)? != signature_algorithm {
            return Err(Error::Malformed); // RFC 5280 4.1.2.3: both must match
        }
        let issuer = r.expect_tlv(der::SEQUENCE)?.raw;
        let mut validity = Reader::new(r.expect(der::SEQUENCE)?);
        let not_before = der::time(validity.any()?)?;
        let not_after = der::time(validity.any()?)?;
        validity.finish()?;
        let subject = r.expect_tlv(der::SEQUENCE)?;
        let public_key = parse_public_key(r.expect(der::SEQUENCE)?)?;
        r.optional(0x81)?; // issuerUniqueID
        r.optional(0x82)?; // subjectUniqueID
        let mut parsed = Parsed {
            tbs: tbs.raw,
            signature_algorithm,
            signature,
            issuer,
            subject: subject.raw,
            not_before,
            not_after,
            public_key,
            is_ca: false,
            digital_signature: true,
            server_auth: true,
            max_path_len: None,
            dns_names: Vec::new(),
            common_name: common_name(subject.value)?,
        };
        if let Some(extensions) = r.optional(explicit(3))? {
            parsed.parse_extensions(der::single(extensions, der::SEQUENCE)?)?;
        }
        r.finish()?;
        Ok(parsed)
    }

    fn parse_extensions(&mut self, data: &'a [u8]) -> Result<(), Error> {
        let mut basic_ca = false;
        let mut cert_sign = true; // no keyUsage extension: no restriction
        let mut list = Reader::new(data);
        while !list.is_empty() {
            let mut ext = Reader::new(list.expect(der::SEQUENCE)?);
            let id = ext.expect(der::OID)?;
            let critical = match ext.optional(der::BOOLEAN)? {
                Some(b) => der::boolean(b)?,
                None => false,
            };
            let value = ext.expect(der::OCTET_STRING)?;
            ext.finish()?;
            match id {
                oid::BASIC_CONSTRAINTS => {
                    let mut bc = Reader::new(der::single(value, der::SEQUENCE)?);
                    if let Some(ca) = bc.optional(der::BOOLEAN)? {
                        basic_ca = der::boolean(ca)?;
                    }
                    if let Some(len) = bc.optional(der::INTEGER)? {
                        self.max_path_len = Some(der::small_unsigned(len)?);
                    }
                    bc.finish()?;
                }
                oid::KEY_USAGE => {
                    let bits = der::single(value, der::BIT_STRING)?;
                    if bits.is_empty() || bits[0] > 7 {
                        return Err(Error::Malformed);
                    }
                    cert_sign = bits.get(1).is_some_and(|b| b & KEY_CERT_SIGN != 0);
                    self.digital_signature =
                        bits.get(1).is_some_and(|b| b & DIGITAL_SIGNATURE != 0);
                }
                oid::EXTENDED_KEY_USAGE => {
                    let mut purposes = Reader::new(der::single(value, der::SEQUENCE)?);
                    let mut server = false;
                    while !purposes.is_empty() {
                        let purpose = purposes.expect(der::OID)?;
                        server |= purpose == SERVER_AUTH || purpose == ANY_EXTENDED_KEY_USAGE;
                    }
                    self.server_auth = server;
                }
                oid::SUBJECT_ALT_NAME => {
                    let mut names = Reader::new(der::single(value, der::SEQUENCE)?);
                    while !names.is_empty() {
                        let name = names.any()?;
                        if name.tag == DNS_NAME {
                            self.dns_names.push(ascii(name.value)?);
                        }
                    }
                }
                // Understood well enough to ignore even when critical.
                oid::CERTIFICATE_POLICIES => {}
                _ if critical => return Err(Error::Unsupported),
                _ => {}
            }
        }
        self.is_ca = basic_ca && cert_sign;
        Ok(())
    }

    /// Maps the signature AlgorithmIdentifier to a supported algorithm.
    pub fn algorithm(&self) -> Result<SignatureAlgorithm, Error> {
        let mut r = Reader::new(self.signature_algorithm);
        let id = r.expect(der::OID)?;
        let params = r.any().ok();
        r.finish()?;
        let null_or_absent = params.is_none_or(|p| p.tag == der::NULL && p.value.is_empty());
        let absent = params.is_none();
        Ok(match id {
            oid::SHA256_WITH_RSA if null_or_absent => SignatureAlgorithm::RsaPkcs1Sha256,
            oid::SHA384_WITH_RSA if null_or_absent => SignatureAlgorithm::RsaPkcs1Sha384,
            oid::SHA512_WITH_RSA if null_or_absent => SignatureAlgorithm::RsaPkcs1Sha512,
            oid::ECDSA_SHA256 if absent => SignatureAlgorithm::EcdsaSha256,
            oid::ECDSA_SHA384 if absent => SignatureAlgorithm::EcdsaSha384,
            oid::ED25519 if absent => SignatureAlgorithm::Ed25519,
            oid::RSA_PSS => SignatureAlgorithm::RsaPss(pss_hash(params.ok_or(Error::Malformed)?)?),
            _ => return Err(Error::Unsupported),
        })
    }
}

impl<'a> Certificate<'a> {
    /// Parses a DER certificate. Fails with [`Error::Unsupported`] for
    /// signature or key algorithms outside the supported set and for
    /// unknown critical extensions.
    pub fn parse(der: &'a [u8]) -> Result<Self, Error> {
        let p = Parsed::parse(der)?;
        Ok(Certificate {
            signature_algorithm: p.algorithm()?,
            tbs: p.tbs,
            signature: p.signature,
            issuer: p.issuer,
            subject: p.subject,
            not_before: p.not_before,
            not_after: p.not_after,
            public_key: p.public_key,
            is_ca: p.is_ca,
            max_path_len: p.max_path_len,
            dns_names: p.dns_names,
            common_name: p.common_name,
            digital_signature: p.digital_signature,
            server_auth: p.server_auth,
        })
    }
}

/// RSASSA-PSS-params: only the hash is taken (the mask function is assumed
/// to be MGF1 with the same hash, as everywhere in practice). The SHA-1
/// default is not supported.
fn pss_hash(params: Tlv) -> Result<Hash, Error> {
    if params.tag != der::SEQUENCE {
        return Err(Error::Malformed);
    }
    let mut r = Reader::new(params.value);
    let hash_alg = r.optional(explicit(0))?.ok_or(Error::Unsupported)?;
    let mut alg = Reader::new(der::single(hash_alg, der::SEQUENCE)?);
    let id = alg.expect(der::OID)?;
    match id {
        oid::SHA256 => Ok(Hash::Sha256),
        oid::SHA384 => Ok(Hash::Sha384),
        oid::SHA512 => Ok(Hash::Sha512),
        _ => Err(Error::Unsupported),
    }
}

fn parse_public_key(spki: &[u8]) -> Result<PublicKey<'_>, Error> {
    let mut r = Reader::new(spki);
    let mut alg = Reader::new(r.expect(der::SEQUENCE)?);
    let key = der::octet_aligned_bits(r.expect(der::BIT_STRING)?)?;
    r.finish()?;
    let id = alg.expect(der::OID)?;
    match id {
        oid::RSA_ENCRYPTION => {
            let mut k = Reader::new(der::single(key, der::SEQUENCE)?);
            let n = der::unsigned(k.expect(der::INTEGER)?)?;
            let e = der::unsigned(k.expect(der::INTEGER)?)?;
            k.finish()?;
            Ok(PublicKey::Rsa { n, e })
        }
        oid::EC_PUBLIC_KEY => {
            let (curve, coord) = match alg.expect(der::OID)? {
                oid::P256 => (Curve::P256, 32),
                oid::P384 => (Curve::P384, 48),
                _ => return Err(Error::Unsupported),
            };
            if key.len() != 1 + 2 * coord || key[0] != 0x04 {
                return Err(Error::Unsupported); // only uncompressed points
            }
            Ok(PublicKey::Ec { curve, point: key })
        }
        oid::ED25519 => Ok(PublicKey::Ed25519(
            key.try_into().map_err(|_| Error::Malformed)?,
        )),
        _ => Err(Error::Unsupported),
    }
}

/// First commonName of a Name, if it is a readable string.
fn common_name(name: &[u8]) -> Result<Option<&str>, Error> {
    let mut rdns = Reader::new(name);
    while !rdns.is_empty() {
        let mut set = Reader::new(rdns.expect(der::SET)?);
        while !set.is_empty() {
            let mut atv = Reader::new(set.expect(der::SEQUENCE)?);
            let id = atv.expect(der::OID)?;
            let value = atv.any()?;
            if id == oid::COMMON_NAME
                && matches!(
                    value.tag,
                    der::UTF8_STRING
                        | der::PRINTABLE_STRING
                        | der::IA5_STRING
                        | der::TELETEX_STRING
                )
            {
                return Ok(core::str::from_utf8(value.value).ok());
            }
        }
    }
    Ok(None)
}

fn ascii(bytes: &[u8]) -> Result<&str, Error> {
    if bytes.is_ascii() {
        core::str::from_utf8(bytes).map_err(|_| Error::Malformed)
    } else {
        Err(Error::Malformed)
    }
}
