// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The state machine against the real OpenSSL server (`openssl s_server
//! -tls1_3 -rev`, which echoes each line reversed): every cipher suite,
//! ECDSA P-256/P-384, RSA-PSS and Ed25519 server certificates issued by a
//! test CA, a CertificateRequest, and rejection of a wrong host name and
//! of an untrusted CA. Skipped when `openssl` is not installed.

use aricode_tls::{Client, Error, Event};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn tools() -> bool {
    Command::new("openssl").arg("version").output().is_ok()
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aricode-tls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn openssl(dir: &Path, args: &[&str]) {
    let o = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// `-newkey` arguments for each key type.
fn newkey(kind: &str) -> Vec<&'static str> {
    match kind {
        "p256" => vec!["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256"],
        "p384" => vec!["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-384"],
        "rsa" => vec!["-newkey", "rsa:2048"],
        "ed25519" => vec!["-newkey", "ed25519"],
        _ => unreachable!(),
    }
}

const CA_NAME: &str = "AriCode Test CA";

/// A self-signed CA `name`.pem / `name`.key (P-256) with common name
/// `cn`, and its DER.
fn make_ca(dir: &Path, name: &str, cn: &str) -> Vec<u8> {
    let (key, pem, der) = (
        format!("{name}.key"),
        format!("{name}.pem"),
        format!("{name}.der"),
    );
    let subject = format!("/CN={cn}");
    let mut args = vec!["req", "-x509", "-new", "-nodes"];
    args.extend(newkey("p256"));
    args.extend([
        "-keyout",
        &key,
        "-out",
        &pem,
        "-subj",
        &subject,
        "-days",
        "30",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]);
    openssl(dir, &args);
    openssl(dir, &["x509", "-in", &pem, "-outform", "der", "-out", &der]);
    std::fs::read(dir.join(der)).unwrap()
}

/// A leaf `kind`.pem / `kind`.key for `localhost`, issued by CA `ca`.
fn make_leaf(dir: &Path, ca: &str, kind: &str) {
    let (key, csr, pem) = (
        format!("{kind}.key"),
        format!("{kind}.csr"),
        format!("{kind}.pem"),
    );
    let mut args = vec!["req", "-new", "-nodes"];
    args.extend(newkey(kind));
    args.extend([
        "-keyout",
        &key,
        "-out",
        &csr,
        "-subj",
        "/CN=localhost",
        "-addext",
        "subjectAltName=DNS:localhost,DNS:*.test.local",
    ]);
    openssl(dir, &args);
    let (ca_pem, ca_key) = (format!("{ca}.pem"), format!("{ca}.key"));
    openssl(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            &csr,
            "-CA",
            &ca_pem,
            "-CAkey",
            &ca_key,
            "-days",
            "30",
            "-copy_extensions",
            "copyall",
            "-out",
            &pem,
        ],
    );
}

/// `openssl s_server` on a free port; returns it and the port.
fn server(dir: &Path, leaf: &str, extra: &[&str]) -> (Child, u16) {
    let mut child = Command::new("openssl")
        .current_dir(dir)
        .args([
            "s_server",
            "-tls1_3",
            "-accept",
            "127.0.0.1:0",
            "-naccept",
            "1",
            "-rev",
        ])
        .args([
            "-cert",
            &format!("{leaf}.pem"),
            "-key",
            &format!("{leaf}.key"),
        ])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let port = loop {
        let line = lines.next().expect("s_server exited").unwrap();
        if let Some(addr) = line.strip_prefix("ACCEPT ") {
            break addr.rsplit(':').next().unwrap().parse().unwrap();
        }
    };
    // Keep draining its output so it never blocks on a full pipe.
    std::thread::spawn(move || lines.for_each(drop));
    (child, port)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Connects, sends `line` once connected and returns what came back
/// before the reversed line arrived (or the error).
fn exchange(port: u16, host: &str, root: Vec<u8>, line: &[u8]) -> Result<Vec<u8>, Error> {
    let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&now().to_le_bytes());
    seed[8..12].copy_from_slice(&port.to_le_bytes().repeat(2));
    let mut client = Client::new(host, seed, Vec::new(), Some(now()));
    client.add_root(root);
    let mut out = Vec::new();
    client.start(&mut out);
    sock.write_all(&out).unwrap();
    let mut expected: Vec<u8> = line.strip_suffix(b"\n").unwrap().to_vec();
    expected.reverse();
    let mut received = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = sock.read(&mut buf).unwrap();
        assert!(n > 0, "server closed the connection early");
        let mut out = Vec::new();
        let mut events = Vec::new();
        let r = client.feed(&buf[..n], &mut out, &mut events);
        for e in &events {
            match e {
                Event::Connected => client.send(line, &mut out),
                Event::Data(d) => received.extend_from_slice(d),
                Event::Closed => panic!("closed before the echo"),
            }
        }
        let _ = sock.write_all(&out);
        r?;
        if received.windows(expected.len()).any(|w| w == expected) {
            let mut out = Vec::new();
            client.close(&mut out);
            sock.write_all(&out).unwrap();
            return Ok(received);
        }
    }
}

fn round_trip(name: &str, leaf: &str, extra: &[&str]) {
    if !tools() {
        eprintln!("openssl not installed: skipped");
        return;
    }
    let dir = tmpdir(name);
    let ca = make_ca(&dir, "ca", CA_NAME);
    make_leaf(&dir, "ca", leaf);
    let (mut child, port) = server(&dir, leaf, extra);
    let got = exchange(port, "localhost", ca, b"hello from AriCodeOS\n");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(got.unwrap(), b"SOedoCirA morf olleh\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chacha20_poly1305_with_ecdsa_p256() {
    round_trip(
        "chacha20_poly1305_with_ecdsa_p256",
        "p256",
        &["-ciphersuites", "TLS_CHACHA20_POLY1305_SHA256"],
    );
}

#[test]
fn aes_128_gcm_with_ecdsa_p256() {
    round_trip(
        "aes_128_gcm_with_ecdsa_p256",
        "p256",
        &["-ciphersuites", "TLS_AES_128_GCM_SHA256"],
    );
}

#[test]
fn aes_256_gcm_sha384_with_ecdsa_p256() {
    round_trip(
        "aes_256_gcm_sha384_with_ecdsa_p256",
        "p256",
        &["-ciphersuites", "TLS_AES_256_GCM_SHA384"],
    );
}

#[test]
fn rsa_pss_certificate() {
    round_trip("rsa_pss_certificate", "rsa", &[]);
}

#[test]
fn ecdsa_p384_certificate() {
    round_trip(
        "ecdsa_p384_certificate",
        "p384",
        &["-ciphersuites", "TLS_AES_256_GCM_SHA384"],
    );
}

#[test]
fn ed25519_certificate() {
    round_trip("ed25519_certificate", "ed25519", &[]);
}

#[test]
fn certificate_request_is_answered_without_a_certificate() {
    round_trip(
        "certificate_request_is_answered_without_a_certificate",
        "p256",
        &["-verify", "1"],
    );
}

/// The leaf is issued by CA_NAME; the client trusts only a CA called
/// `trusted` (the real one if it is CA_NAME, else a different key).
fn try_connect(host: &str, trusted: &str) -> Result<Vec<u8>, Error> {
    let dir = tmpdir(&format!("connect-{host}-{}", trusted.len()));
    let mut root = make_ca(&dir, "ca", CA_NAME);
    if trusted != CA_NAME {
        root = make_ca(&dir, "other", trusted);
    }
    make_leaf(&dir, "ca", "p256");
    let (mut child, port) = server(&dir, "p256", &[]);
    let r = exchange(port, host, root, b"x\n");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[test]
fn wrong_host_name_is_rejected() {
    if !tools() {
        return;
    }
    assert_eq!(
        try_connect("example.com", CA_NAME),
        Err(Error::Certificate(aricode_x509::Error::NameMismatch))
    );
}

#[test]
fn wildcard_name_matches() {
    if !tools() {
        return;
    }
    assert_eq!(try_connect("a.test.local", CA_NAME), Ok(b"x\n".to_vec()));
}

#[test]
fn untrusted_ca_is_rejected() {
    if !tools() {
        return;
    }
    assert_eq!(
        try_connect("localhost", "Some Other CA"),
        Err(Error::Certificate(aricode_x509::Error::UnknownIssuer))
    );
}

#[test]
fn impostor_ca_with_the_same_name_is_rejected() {
    if !tools() {
        return;
    }
    // Same subject as the issuer, different key: the signature fails.
    let dir = tmpdir("impostor");
    let _ = make_ca(&dir, "ca", CA_NAME);
    let impostor = make_ca(&dir, "impostor", CA_NAME);
    make_leaf(&dir, "ca", "p256");
    let (mut child, port) = server(&dir, "p256", &[]);
    let r = exchange(port, "localhost", impostor, b"x\n");
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        r,
        Err(Error::Certificate(aricode_x509::Error::BadSignature))
    );
}
