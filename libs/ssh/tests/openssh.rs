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
