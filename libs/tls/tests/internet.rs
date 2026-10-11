//! Real servers on the Internet with the embedded roots (`--ignored`: needs
//! network). `cargo test -p aricode-tls --test internet -- --ignored`.

use aricode_tls::{Client, Event};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn get(host: &str) -> Result<String, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut c = Client::new(
        host,
        [7; 32],
        aricode_x509::roots::DEFAULT.to_vec(),
        Some(now),
    );
    let mut sock = TcpStream::connect((host, 443)).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut out = Vec::new();
    c.start(&mut out);
    sock.write_all(&out).unwrap();
    let mut body = Vec::new();
    let mut buf = [0u8; 8192];
    let mut sent = false;
    loop {
        let n = match sock.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut out = Vec::new();
        let mut ev = Vec::new();
        c.feed(&buf[..n], &mut out, &mut ev)
            .map_err(|e| format!("{e:?}"))?;
        for e in ev {
            match e {
                Event::Connected if !sent => {
                    c.send(
                        format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                            .as_bytes(),
                        &mut out,
                    );
                    sent = true;
                }
                Event::Data(d) => body.extend_from_slice(&d),
                Event::Closed => {}
                _ => {}
            }
        }
        sock.write_all(&out).unwrap();
        if body.len() > 2000 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&body);
    Ok(text.lines().next().unwrap_or("").to_string())
}

#[test]
#[ignore]
fn well_known_sites() {
    for host in [
        "www.google.com",
        "example.com",
        "github.com",
        "www.cloudflare.com",
        "en.wikipedia.org",
        "letsencrypt.org",
        "www.amazon.com",
    ] {
        match get(host) {
            Ok(status) => println!("{host}: {status}"),
            Err(e) => println!("{host}: ERROR {e}"),
        }
    }
}
