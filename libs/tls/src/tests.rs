// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The client against a rustls server in memory (with `ring` to sign), on
//! certificates made at run time by `rcgen`: every TLS 1.3 and TLS 1.2
//! suite, every key exchange group (a HelloRetryRequest included), and the
//! certificate policy. RSA servers are covered by the QEMU scenarios
//! (`openssl s_server`): `ring` cannot generate RSA keys.

use super::*;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::crypto::ring::{cipher_suite, default_provider, kx_group};
use rustls::pki_types::PrivateKeyDer;
use rustls::{ServerConfig, ServerConnection, SupportedCipherSuite};
use std::io::{Read, Write};
use std::string::{String, ToString};
use std::vec::Vec;

/// A test root and its key.
struct Root {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn root(name: &str) -> Root {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, name);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = std::vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    Root {
        cert: p.self_signed(&key).unwrap(),
        key,
    }
}

/// A server certificate for `names` under `root`, valid between the two
/// years given (or around now), with a key of `alg`.
struct Leaf {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

fn leaf(
    root: &Root,
    names: &[&str],
    alg: &'static rcgen::SignatureAlgorithm,
    years: Option<(i32, i32)>,
) -> Leaf {
    let key = KeyPair::generate_for(alg).unwrap();
    let mut p =
        CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    p.extended_key_usages = std::vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.key_usages = std::vec![KeyUsagePurpose::DigitalSignature];
    if let Some((from, to)) = years {
        p.not_before = rcgen::date_time_ymd(from, 1, 1);
        p.not_after = rcgen::date_time_ymd(to, 1, 1);
    }
    let cert = p.signed_by(&key, &root.cert, &root.key).unwrap();
    Leaf {
        chain: std::vec![cert.der().clone()],
        key: PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A server limited to `suites` and `groups` (all of `ring`'s when empty).
fn server(
    leaf: &Leaf,
    suites: &[SupportedCipherSuite],
    groups: &[&'static dyn rustls::crypto::SupportedKxGroup],
) -> ServerConnection {
    let mut provider = default_provider();
    if !suites.is_empty() {
        provider.cipher_suites = suites.to_vec();
    }
    if !groups.is_empty() {
        provider.kx_groups = groups.to_vec();
    }
    let versions: Vec<&rustls::SupportedProtocolVersion> = if suites
        .iter()
        .all(|s| matches!(s, SupportedCipherSuite::Tls12(_)))
        && !suites.is_empty()
    {
        std::vec![&rustls::version::TLS12]
    } else {
        std::vec![&rustls::version::TLS13, &rustls::version::TLS12]
    };
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(leaf.chain.clone(), leaf.key.clone_key())
        .unwrap();
    ServerConnection::new(Arc::new(config)).unwrap()
}

fn client(host: &str, root: &Root, time: aricode_x509::ValidityTime) -> Client {
    Client::new(host, [7; 32], &[], &[root.cert.der().to_vec()], time)
}

/// Moves bytes both ways until neither side has anything to say. The
/// server's bytes go to the client `chunk` at a time. Returns the client's
/// events, or its error.
fn pump(c: &mut Client, s: &mut ServerConnection, chunk: usize) -> Result<Vec<Event>, Error> {
    let mut to_server = Vec::new();
    if !c.connected && c.incoming.is_empty() && c.pending.is_empty() && !c.closed {
        c.start(&mut to_server)?;
    }
    let mut events = Vec::new();
    for _ in 0..64 {
        if !to_server.is_empty() {
            s.read_tls(&mut &to_server[..]).unwrap();
            to_server.clear();
            // A server-side failure shows on the client as its alert.
            let _ = s.process_new_packets();
        }
        let mut to_client = Vec::new();
        s.write_tls(&mut to_client).unwrap();
        if to_client.is_empty() && to_server.is_empty() {
            break;
        }
        for piece in to_client.chunks(chunk.max(1)) {
            c.feed(piece, &mut to_server, &mut events)?;
        }
    }
    events.append(&mut c.pending);
    Ok(events)
}

fn handshake(c: &mut Client, s: &mut ServerConnection) {
    let events = pump(c, s, usize::MAX).expect("handshake");
    assert!(events.contains(&Event::Connected), "{events:?}");
    assert!(c.is_connected() && !s.is_handshaking());
}

/// Request one way, answer and close_notify the other.
fn exchange(c: &mut Client, s: &mut ServerConnection) {
    let mut out = Vec::new();
    c.send(b"GET / HTTP/1.1\r\n\r\n", &mut out);
    s.read_tls(&mut &out[..]).unwrap();
    s.process_new_packets().unwrap();
    let mut got = [0u8; 64];
    let n = s.reader().read(&mut got).unwrap();
    assert_eq!(&got[..n], b"GET / HTTP/1.1\r\n\r\n");
    s.writer().write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
    s.send_close_notify();
    let events = pump(c, s, usize::MAX).unwrap();
    assert_eq!(
        events,
        std::vec![
            Event::Data(b"HTTP/1.1 200 OK\r\n\r\n".to_vec()),
            Event::Closed
        ]
    );
}

#[test]
fn every_tls13_suite() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    for suite in [
        cipher_suite::TLS13_AES_128_GCM_SHA256,
        cipher_suite::TLS13_AES_256_GCM_SHA384,
        cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        let mut s = server(&leaf, &[suite], &[]);
        let mut c = client(
            "server.test",
            &root,
            aricode_x509::ValidityTime::Clock(now()),
        );
        handshake(&mut c, &mut s);
        assert_eq!(s.negotiated_cipher_suite(), Some(suite));
        exchange(&mut c, &mut s);
    }
}

#[test]
fn every_tls12_suite() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    for suite in [
        cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        cipher_suite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    ] {
        let mut s = server(&leaf, &[suite], &[]);
        let mut c = client(
            "server.test",
            &root,
            aricode_x509::ValidityTime::Clock(now()),
        );
        handshake(&mut c, &mut s);
        assert_eq!(s.protocol_version(), Some(rustls::ProtocolVersion::TLSv1_2));
        exchange(&mut c, &mut s);
    }
}

/// X25519 is offered first; a server that wants P-256 or P-384 answers with
/// a HelloRetryRequest, which the old client could not follow.
#[test]
fn every_key_exchange_group() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    for group in [kx_group::X25519, kx_group::SECP256R1, kx_group::SECP384R1] {
        for suites in [
            &[cipher_suite::TLS13_AES_128_GCM_SHA256][..],
            &[cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256][..],
        ] {
            let mut s = server(&leaf, suites, &[group]);
            let mut c = client(
                "server.test",
                &root,
                aricode_x509::ValidityTime::Clock(now()),
            );
            handshake(&mut c, &mut s);
            assert_eq!(
                s.negotiated_key_exchange_group().map(|g| g.name()),
                Some(group.name())
            );
            exchange(&mut c, &mut s);
        }
    }
}

#[test]
fn ed25519_and_p384_servers() {
    let root = root("test root");
    for alg in [&rcgen::PKCS_ED25519, &rcgen::PKCS_ECDSA_P384_SHA384] {
        let leaf = leaf(&root, &["server.test"], alg, None);
        let mut s = server(&leaf, &[], &[]);
        let mut c = client(
            "server.test",
            &root,
            aricode_x509::ValidityTime::Clock(now()),
        );
        handshake(&mut c, &mut s);
        exchange(&mut c, &mut s);
    }
}

#[test]
fn records_split_into_single_bytes() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    let mut s = server(&leaf, &[], &[]);
    let mut c = client(
        "server.test",
        &root,
        aricode_x509::ValidityTime::Clock(now()),
    );
    let events = pump(&mut c, &mut s, 1).unwrap();
    assert!(events.contains(&Event::Connected));
    exchange(&mut c, &mut s);
}

/// What the client says for each kind of chain it must refuse.
#[test]
fn refused_certificates() {
    use aricode_x509::{Error as X, ValidityTime::*};
    let root = root("test root");
    let other = root_other();
    let p256 = &rcgen::PKCS_ECDSA_P256_SHA256;
    let cases: [(&str, Leaf, aricode_x509::ValidityTime, X); 5] = [
        (
            "server.test",
            leaf(&other, &["server.test"], p256, None),
            Clock(now()),
            X::UnknownIssuer,
        ),
        (
            "other.test",
            leaf(&root, &["server.test"], p256, None),
            Clock(now()),
            X::NameMismatch,
        ),
        (
            "server.test",
            leaf(&root, &["server.test"], p256, Some((2020, 2021))),
            Clock(now()),
            X::Expired,
        ),
        (
            "server.test",
            leaf(&root, &["server.test"], p256, Some((2045, 2075))),
            Clock(now()),
            X::NotYetValid,
        ),
        // ADR 0062: matched only through `*.co.uk`.
        (
            "shop.co.uk",
            leaf(&root, &["*.co.uk"], p256, None),
            Clock(now()),
            X::WildcardOverSuffix,
        ),
    ];
    for (host, leaf, time, why) in cases {
        let mut s = server(&leaf, &[], &[]);
        let mut c = client(host, &root, time);
        assert_eq!(
            pump(&mut c, &mut s, usize::MAX),
            Err(Error::Certificate(why)),
            "{host} {why:?}"
        );
        assert!(!c.is_connected());
    }
}

fn root_other() -> Root {
    root("unknown root")
}

/// Without a trusted clock (incident 0036): a certificate renewed after the
/// release's floor is accepted, one that expired before it is not.
#[test]
fn time_floor_without_a_trusted_clock() {
    use aricode_x509::ValidityTime::AtLeast;
    let root = root("test root");
    let floor = aricode_x509::TIME_FLOOR;
    // The test root and leaves are issued now, after the floor.
    let renewed = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    let mut s = server(&renewed, &[], &[]);
    let mut c = client("server.test", &root, AtLeast(floor));
    handshake(&mut c, &mut s);
    let expired = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        Some((2020, 2021)),
    );
    let mut s = server(&expired, &[], &[]);
    let mut c = client("server.test", &root, AtLeast(floor));
    assert_eq!(
        pump(&mut c, &mut s, usize::MAX),
        Err(Error::Certificate(aricode_x509::Error::Expired))
    );
}

/// A record altered on the way fails authentication.
#[test]
fn tampered_record() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    let mut s = server(&leaf, &[], &[]);
    let mut c = client(
        "server.test",
        &root,
        aricode_x509::ValidityTime::Clock(now()),
    );
    handshake(&mut c, &mut s);
    s.writer().write_all(b"secret answer").unwrap();
    let mut record = Vec::new();
    s.write_tls(&mut record).unwrap();
    let last = record.len() - 1;
    record[last] ^= 1;
    let (mut out, mut events) = (Vec::new(), Vec::new());
    assert_eq!(c.feed(&record, &mut out, &mut events), Err(Error::Decrypt));
    assert!(!c.is_connected());
    assert!(!events.iter().any(|e| matches!(e, Event::Data(_))));
}

/// The server sends a fatal alert: the client reports it.
#[test]
fn server_alert() {
    let root = root("test root");
    let leaf = leaf(
        &root,
        &["server.test"],
        &rcgen::PKCS_ECDSA_P256_SHA256,
        None,
    );
    // An ECDSA certificate with only an RSA suite: the server finds no
    // suite it can use and refuses with handshake_failure.
    let mut s = server(
        &leaf,
        &[cipher_suite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256],
        &[],
    );
    let mut c = client(
        "server.test",
        &root,
        aricode_x509::ValidityTime::Clock(now()),
    );
    match pump(&mut c, &mut s, usize::MAX) {
        Err(Error::Alert(40)) => {}
        other => panic!("{other:?}"),
    }
}

/// Audit of ADR 0067, M1: a server sending one handshake message in tiny
/// plaintext fragments is cut off at `MAX_INCOMING`, before the heap is.
#[test]
fn fragmented_handshake_is_bounded() {
    let root = root("test root");
    let mut c = client(
        "server.test",
        &root,
        aricode_x509::ValidityTime::Clock(now()),
    );
    let mut out = Vec::new();
    c.start(&mut out).unwrap();
    // A ServerHello declared 0xFFFF bytes long, then 1-byte fragments.
    let mut events = Vec::new();
    let header = [22u8, 3, 3, 0, 4, 2, 0, 0xFF, 0xFF];
    c.feed(&header, &mut out, &mut events).unwrap();
    let fragment = [22u8, 3, 3, 0, 1, 0];
    let mut fed = header.len();
    let result = loop {
        match c.feed(&fragment, &mut out, &mut events) {
            Ok(()) => fed += fragment.len(),
            Err(e) => break e,
        }
        assert!(fed <= MAX_INCOMING, "held {fed} bytes");
    };
    assert_eq!(
        result,
        Error::Protocol("handshake message spread over too many bytes")
    );
    assert!(c.feed(&fragment, &mut out, &mut events).is_err());
}

/// An invalid server name fails at once, not after a reply timeout.
#[test]
fn invalid_host_fails_at_start() {
    let root = root("test root");
    let mut c = client(
        "not a host name",
        &root,
        aricode_x509::ValidityTime::Clock(now()),
    );
    let mut out = Vec::new();
    assert_eq!(
        c.start(&mut out),
        Err(Error::Protocol("invalid server name"))
    );
    assert!(out.is_empty());
}
