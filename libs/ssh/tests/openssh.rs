// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The state machine against the real OpenSSH client (`ssh` on the host):
//! key exchange, host key, cipher, public-key authentication, exec with
//! exit status, an interactive session with a pty, and a refused key.
//! Skipped when `ssh`/`ssh-keygen` are not installed.

use aricode_ssh::{base64, ed25519_blob, parse_authorized_keys, Event, Server};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn tools() -> bool {
    Command::new("ssh").arg("-V").output().is_ok()
        && Command::new("ssh-keygen").arg("-?").output().is_ok()
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aricode-ssh-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Client key pair in `dir`; returns the authorized_keys line.
fn client_key(dir: &Path) -> Vec<u8> {
    let key = dir.join("id");
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(dir.join("id.pub")).unwrap()
}

/// Runs one connection: `on_event` decides what the server does.
fn serve(
    listener: TcpListener,
    authorized: Vec<[u8; 32]>,
    mut on_event: impl FnMut(&Event, &mut Server, &mut Vec<u8>),
) {
    let (mut sock, _) = listener.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut server = Server::new([0x42; 32], [0x17; 32], authorized);
    let mut out = Vec::new();
    server.start(&mut out);
    sock.write_all(&out).unwrap();
    let mut buf = [0u8; 4096];
    loop {
        let n = match sock.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut out = Vec::new();
        let mut events = Vec::new();
        let r = server.feed(&buf[..n], &mut out, &mut events);
        for e in &events {
            on_event(e, &mut server, &mut out);
        }
        let _ = sock.write_all(&out);
        if r.is_err() || server.is_closed() || events.contains(&Event::Closed) {
            return;
        }
    }
}

fn ssh(dir: &Path, port: u16, extra: &[&str]) -> Command {
    let mut c = Command::new("ssh");
    c.args([
        "-F",
        "/dev/null",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=no",
    ])
    .arg("-o")
    .arg(format!(
        "UserKnownHostsFile={}",
        dir.join("known_hosts").display()
    ))
    .args(["-o", "ConnectTimeout=10", "-i"])
    .arg(dir.join("id"))
    .args(["-p", &port.to_string()])
    .args(extra)
    .arg("ari@127.0.0.1");
    c
}

#[test]
fn exec_with_an_authorized_key() {
    if !tools() {
        eprintln!("ssh not installed: skipped");
        return;
    }
    let dir = tmpdir("exec");
    let line = client_key(&dir);
    let keys = parse_authorized_keys(&line).keys;
    assert_eq!(keys.len(), 1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut user = String::new();
        serve(listener, keys, |e, s, out| match e {
            Event::Authenticated { user: u } => user = u.clone(),
            Event::Exec { command, pty } => {
                assert!(!pty);
                let reply = [b"you said: ".as_slice(), command, b"\n"].concat();
                s.send_data(&reply, out);
                s.exit(3, out);
            }
            _ => {}
        });
        user
    });
    let o = ssh(&dir, port, &[]).arg("hello world").output().unwrap();
    let user = server.join().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "you said: hello world\n",
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(3), "exit status travels back");
    assert_eq!(user, "ari");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interactive_session_with_a_pty() {
    if !tools() {
        return;
    }
    let dir = tmpdir("pty");
    let keys = parse_authorized_keys(&client_key(&dir)).keys;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut typed = Vec::new();
        serve(listener, keys, |e, s, out| match e {
            Event::Shell { pty } => {
                assert!(pty);
                s.send_data(b"welcome\n> ", out);
            }
            Event::Data(d) => {
                typed.extend_from_slice(d);
                if typed.ends_with(b"bye\r") || typed.ends_with(b"bye\n") {
                    s.send_data(b"\ngoodbye\n", out);
                    s.exit(0, out);
                }
            }
            _ => {}
        });
        typed
    });
    let mut child = ssh(&dir, port, &["-tt"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    child.stdin.as_mut().unwrap().write_all(b"bye\r").unwrap();
    let o = child.wait_with_output().unwrap();
    let typed = server.join().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.contains("welcome\r\n> "),
        "newlines become CR LF on a pty: {stdout:?}"
    );
    assert!(
        stdout.contains("goodbye"),
        "{stdout:?} / {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(typed.ends_with(b"bye\r") || typed.ends_with(b"bye\n"));
    assert_eq!(o.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_key_is_refused() {
    if !tools() {
        return;
    }
    let dir = tmpdir("refused");
    let _ = client_key(&dir);
    let other = [9u8; 32];
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let mut authenticated = false;
        let mut refused = Vec::new();
        serve(listener, vec![other], |e, _, _| match e {
            Event::Authenticated { .. } => authenticated = true,
            Event::AuthFailed { user } => refused.push(user.clone()),
            _ => {}
        });
        (authenticated, refused)
    });
    let o = ssh(&dir, port, &[]).arg("true").output().unwrap();
    // The client gives up once its only key is refused and closes.
    let _ = TcpStream::connect(("127.0.0.1", port));
    let (authenticated, refused) = server.join().unwrap();
    assert!(!authenticated, "must not authenticate");
    assert!(
        refused.iter().any(|u| u == "ari"),
        "the refused key must be reported: {refused:?}"
    );
    assert_eq!(o.status.code(), Some(255));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("Permission denied"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let _ = (base64::encode(&ed25519_blob(&other), true), &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Output larger than the client's initial window (2 MiB in OpenSSH) must
/// arrive whole before the exit status: `exit` waits for the window.
#[test]
fn large_output_is_delivered_whole() {
    if !tools() {
        return;
    }
    let dir = tmpdir("large-output");
    let keys = parse_authorized_keys(&client_key(&dir)).keys;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let expected = 3 * 1024 * 1024;
    let server = std::thread::spawn(move || {
        serve(listener, keys, |e, s, out| {
            if matches!(e, Event::Exec { .. }) {
                s.send_data(&vec![b'x'; expected], out);
                s.exit(0, out);
            }
        });
    });
    let o = ssh(&dir, port, &[]).arg("large-output").output().unwrap();
    server.join().unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.stdout.len(), expected, "output lost before the close");
    assert!(o.stdout.iter().all(|&b| b == b'x'));
    let _ = std::fs::remove_dir_all(&dir);
}

/// One connection whose command gets `size` bytes, produced as the
/// client's window allows (never more than 1 MiB waiting), then exit 0.
fn serve_stream(listener: TcpListener, authorized: Vec<[u8; 32]>, size: u64) {
    let (mut sock, _) = listener.accept().unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut server = Server::new([0x42; 32], [0x17; 32], authorized);
    let mut out = Vec::new();
    server.start(&mut out);
    let chunk = vec![b'x'; 64 * 1024];
    let mut left: Option<u64> = None;
    let mut buf = vec![0u8; 64 * 1024];
    let mut events = Vec::new();
    loop {
        while let Some(n) = left.filter(|&n| n > 0 && server.pending_len() < 1 << 20) {
            let k = n.min(chunk.len() as u64) as usize;
            server.send_data(&chunk[..k], &mut out);
            left = Some(n - k as u64);
        }
        if left == Some(0) {
            server.exit(0, &mut out);
            left = None;
        }
        if sock.write_all(&out).is_err() || server.is_closed() {
            return;
        }
        out.clear();
        let n = match sock.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if server.feed(&buf[..n], &mut out, &mut events).is_err() {
            let _ = sock.write_all(&out);
            return;
        }
        for e in events.drain(..) {
            match e {
                Event::Exec { .. } => left = Some(size),
                Event::Closed => {
                    let _ = sock.write_all(&out);
                    return;
                }
                _ => {}
            }
        }
    }
}

/// Runs `ssh -v` with `extra` against a `serve_stream` of `size` bytes:
/// the bytes received, the exit status and the client's log.
fn stream(name: &str, size: u64, extra: &[&str]) -> (u64, Option<i32>, String) {
    let dir = tmpdir(name);
    let keys = parse_authorized_keys(&client_key(&dir)).keys;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || serve_stream(listener, keys, size));
    let mut child = ssh(&dir, port, &[&["-v"], extra].concat())
        .arg("stream")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let log = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut got = 0u64;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                assert!(buf[..n].iter().all(|&b| b == b'x'), "output corrupted");
                got += n as u64;
            }
        }
    }
    let status = child.wait().unwrap().code();
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (got, status, log.join().unwrap())
}

/// The client's choice of key exchange: the post-quantum hybrid by default
/// (OpenSSH 10), X25519 alone for a client that offers only that (ADR 0068).
#[test]
fn post_quantum_and_classic_key_exchanges() {
    if !tools() {
        return;
    }
    let (got, status, log) = stream("kex-default", 1000, &[]);
    assert_eq!((got, status), (1000, Some(0)), "{log}");
    let pq = "kex: algorithm: mlkem768x25519-sha256";
    // Older clients do not offer it; they fall back below.
    if log.contains("mlkem768x25519-sha256") {
        assert!(log.contains(pq), "{log}");
    }
    for kex in [
        "mlkem768x25519-sha256",
        "curve25519-sha256",
        "curve25519-sha256@libssh.org",
    ] {
        let option = format!("KexAlgorithms={kex}");
        let (got, status, log) = stream("kex-one", 1000, &["-o", &option]);
        if kex.starts_with("mlkem") && log.contains("Bad SSH2 KexAlgorithms") {
            continue; // a client older than OpenSSH 9.9
        }
        assert_eq!((got, status), (1000, Some(0)), "{kex}: {log}");
        assert!(log.contains(&format!("kex: algorithm: {kex}")), "{log}");
    }
}

/// A re-exchange the client asks for (every MiB here) keeps the session
/// going: before ADR 0068 the server refused it, and OpenSSH sessions
/// ended after about 1 GiB with this cipher.
#[test]
fn client_rekeys_mid_transfer() {
    if !tools() {
        return;
    }
    let size = 8 << 20;
    let (got, status, log) = stream("client-rekey", size, &["-o", "RekeyLimit=1M"]);
    assert_eq!((got, status), (size, Some(0)), "{log}");
    // OpenSSH checks its limit lazily on a connection it only reads from:
    // three re-exchanges in 8 MiB with 10.0.
    let exchanges = log.matches("SSH2_MSG_NEWKEYS received").count();
    assert!(exchanges >= 3, "{exchanges} key exchanges: {log}");
}

/// The server asks for new keys itself after 1 GiB either way, whatever
/// the client's own limit.
#[test]
fn server_rekeys_after_a_gibibyte() {
    if !tools() {
        return;
    }
    let size = (1 << 30) + (256 << 20);
    let (got, status, log) = stream("server-rekey", size, &["-o", "RekeyLimit=8G"]);
    assert_eq!((got, status), (size, Some(0)), "{log}");
    let exchanges = log.matches("SSH2_MSG_NEWKEYS received").count();
    assert_eq!(
        exchanges, 2,
        "the first exchange and one the server started: {log}"
    );
}
