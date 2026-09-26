//! Lanzamiento de QEMU + OVMF, interactivo o con captura de serie.

use crate::image::{self, OVMF_CODE};
use crate::{build_dir, root, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn base_command(esp: &Path, vars: &Path) -> Command {
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
        image::build_scenario_esp(&base, "interactive", cmdline, &Default::default())?
    };
    let vars = image::ovmf_vars_for(&build_dir())?;
    println!("[xtask] QEMU (salir: Ctrl-A X)");
    let st = base_command(&esp, &vars)
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

/// Arranca sin pantalla, captura la serie y espera hasta `timeout`.
pub fn run_headless(esp: &Path, workdir: &Path, timeout: Duration) -> Result<Capture> {
    let vars = image::ovmf_vars_for(workdir)?;
    let start = Instant::now();
    let mut child = base_command(esp, &vars)
        .args(["-display", "none", "-monitor", "none", "-serial", "stdio"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("qemu: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
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
        std::thread::sleep(Duration::from_millis(50));
    };
    let bytes = reader.join().map_err(|_| "lector de serie")?;
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
    use super::strip_ansi;

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(strip_ansi("\x1b[2J\x1b[01;01Hhola\x1b[=3h"), "hola");
        assert_eq!(strip_ansi("sin escapes"), "sin escapes");
    }
}
