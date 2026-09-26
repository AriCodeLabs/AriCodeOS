//! Lanzamiento de QEMU + OVMF, interactivo o con captura de serie.

use crate::image::{self, OVMF_CODE};
use crate::{build_dir, root, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn base_command(esp: &Path, vars: &Path, disk: &Path) -> Command {
    let mut q = Command::new("qemu-system-x86_64");
    q.current_dir(root());
    q.args([
        "-machine",
        "q35",
        "-cpu",
        "qemu64",
        "-m",
        "512M",
        "-no-reboot",
    ]);
    // Disco virtio-blk moderno (sin interfaz legacy) para el driver de usuario.
    q.args([
        "-drive",
        &format!("if=none,id=disk0,format=raw,file={}", disk.display()),
    ]);
    q.args([
        "-device",
        "virtio-blk-pci,drive=disk0,disable-legacy=on,disable-modern=off",
    ]);
    q.args([
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"),
    ]);
    q.args([
        "-drive",
        &format!("if=pflash,format=raw,file={}", vars.display()),
    ]);
    q.args([
        "-drive",
        &format!("format=raw,file=fat:rw:{}", esp.display()),
    ]);
    q.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    q
}

pub fn run_interactive(release: bool, cmdline: &str) -> Result {
    let base = image::build_base_esp(release)?;
    let esp = if cmdline.is_empty() {
        base
    } else {
        image::build_scenario_esp(
            &base,
            &image::scenarios_root(),
            "interactive",
            cmdline,
            &Default::default(),
        )?
    };
    let vars = image::ovmf_vars_for(&build_dir())?;
    let disk = build_dir().join("disk.img");
    image::write_test_disk(&disk)?;
    println!("[xtask] QEMU (salir: Ctrl-A X)");
    let st = base_command(&esp, &vars, &disk)
        .arg("-nographic")
        .status()
        .map_err(|e| format!("qemu: {e}"))?;
    println!("[xtask] qemu exit: {:?}", st.code());
    Ok(())
}

pub struct Capture {
    pub serial: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed: Duration,
}

/// Entrada guiada por la salida: cada línea se envía cuando la serie ha
/// mostrado `prompt` tantas veces como líneas ya enviadas más una.
pub struct Input<'a> {
    pub prompt: &'a str,
    pub lines: &'a [String],
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Arranca sin pantalla, captura la serie, alimenta `input` y espera hasta
/// `timeout`.
pub fn run_headless(
    esp: &Path,
    workdir: &Path,
    timeout: Duration,
    input: &Input,
) -> Result<Capture> {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    let vars = image::ovmf_vars_for(workdir)?;
    let disk = workdir.join("disk.img");
    image::write_test_disk(&disk)?;
    let start = Instant::now();
    let mut child = base_command(esp, &vars, &disk)
        .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
        .stdin(if input.lines.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("qemu: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stdin = child.stdin.take();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().expect("captura").extend_from_slice(&chunk[..n]),
            }
        }
    });
    let mut sent = 0;
    let mut timed_out = false;
    let exit_code = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st.code();
        }
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        if sent < input.lines.len() {
            let prompts =
                count_occurrences(&captured.lock().expect("captura"), input.prompt.as_bytes());
            if prompts > sent {
                if let Some(w) = stdin.as_mut() {
                    let _ = w.write_all(format!("{}\n", input.lines[sent]).as_bytes());
                    let _ = w.flush();
                }
                sent += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    reader.join().map_err(|_| "lector de serie")?;
    let bytes = captured.lock().expect("captura").clone();
    Ok(Capture {
        serial: strip_ansi(&String::from_utf8_lossy(&bytes)),
        exit_code,
        timed_out,
        elapsed: start.elapsed(),
    })
}

/// Elimina las secuencias de escape que OVMF emite por la consola serie.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_prompts() {
        assert_eq!(
            super::count_occurrences(b"ari> x\nari> y\nari> ", b"ari> "),
            3
        );
        assert_eq!(super::count_occurrences(b"nada", b"ari> "), 0);
        assert_eq!(super::count_occurrences(b"aaaa", b""), 0);
    }

    use super::strip_ansi;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[2J\x1b[01;01Hhola\x1b[=3h"), "hola");
        assert_eq!(strip_ansi("sin escapes"), "sin escapes");
    }
}
