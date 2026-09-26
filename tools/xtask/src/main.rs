//! xtask: automatización de AriCodeOS.
//!
//!   cargo xtask build [--debug]        compila AriBoot y AriKernel
//!   cargo xtask image [--debug]        build + monta build/esp/
//!   cargo xtask run   [--debug]        image + QEMU interactivo (serie en stdio)
//!   cargo xtask test  [--debug]        image (kernel con qemu-exit) + test de boot
//!   cargo xtask check                  fmt --check + clippy en todas las crates
//!
//! Requiere qemu-system-x86_64 y OVMF (/usr/share/OVMF) instalados.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";
const READY_MARKER: &str = "ARICODEOS_KERNEL_READY";
const QEMU_EXIT_SUCCESS: i32 = 33; // (0x10 << 1) | 1
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

struct Opts {
    release: bool,
    test_mode: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let release = !args.iter().any(|a| a == "--debug");

    let result = match cmd {
        "build" => build(&Opts {
            release,
            test_mode: false,
        })
        .map(|_| ()),
        "image" => image(&Opts {
            release,
            test_mode: false,
        })
        .map(|_| ()),
        "run" => run(release),
        "test" => test(release),
        "check" => check(),
        _ => {
            eprintln!("uso: cargo xtask <build|image|run|test|check> [--debug]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

fn profile_dir(release: bool) -> &'static str {
    if release {
        "release"
    } else {
        "debug"
    }
}

struct Artifacts {
    ariboot: PathBuf,
    kernel: PathBuf,
}

fn build(o: &Opts) -> Result<Artifacts, String> {
    println!("[xtask] build ariboot ({UEFI_TARGET})");
    let mut c = cargo();
    c.args(["build", "-p", "ariboot", "--target", UEFI_TARGET]);
    if o.release {
        c.arg("--release");
    }
    status(c)?;

    println!(
        "[xtask] build arikernel ({KERNEL_TARGET}){}",
        if o.test_mode { " [qemu-exit]" } else { "" }
    );
    let mut c = cargo();
    c.args(["build", "-p", "arikernel", "--target", KERNEL_TARGET]);
    if o.release {
        c.arg("--release");
    }
    if o.test_mode {
        c.args(["--features", "qemu-exit"]);
    }
    status(c)?;

    let t = root().join("target");
    Ok(Artifacts {
        ariboot: t
            .join(UEFI_TARGET)
            .join(profile_dir(o.release))
            .join("ariboot.efi"),
        kernel: t
            .join(KERNEL_TARGET)
            .join(profile_dir(o.release))
            .join("arikernel"),
    })
}

fn image(o: &Opts) -> Result<PathBuf, String> {
    let a = build(o)?;
    let esp = root().join("build/esp");
    let boot_dir = esp.join("EFI/BOOT");
    let os_dir = esp.join("aricodeos");
    std::fs::create_dir_all(&boot_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&os_dir).map_err(|e| e.to_string())?;
    std::fs::copy(&a.ariboot, boot_dir.join("BOOTX64.EFI"))
        .map_err(|e| format!("copiar ariboot: {e}"))?;
    std::fs::copy(&a.kernel, os_dir.join("kernel.elf"))
        .map_err(|e| format!("copiar kernel: {e}"))?;
    let vars = root().join("build/OVMF_VARS.fd");
    if !vars.exists() {
        std::fs::copy(OVMF_VARS, &vars).map_err(|e| format!("copiar {OVMF_VARS}: {e}"))?;
    }
    println!("[xtask] imagen ESP en {}", esp.display());
    Ok(esp)
}

fn qemu(esp: &Path, test_mode: bool) -> Command {
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
        &format!(
            "if=pflash,format=raw,file={}",
            root().join("build/OVMF_VARS.fd").display()
        ),
    ]);
    q.args([
        "-drive",
        &format!("format=raw,file=fat:rw:{}", esp.display()),
    ]);
    q.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    if test_mode {
        q.args(["-display", "none", "-serial", "stdio", "-monitor", "none"]);
    } else {
        q.args(["-nographic"]);
    }
    q
}

fn run(release: bool) -> Result<(), String> {
    let esp = image(&Opts {
        release,
        test_mode: false,
    })?;
    println!("[xtask] QEMU (salir: Ctrl-A X)");
    let st = qemu(&esp, false)
        .status()
        .map_err(|e| format!("qemu: {e}"))?;
    println!("[xtask] qemu exit: {:?}", st.code());
    Ok(())
}

fn test(release: bool) -> Result<(), String> {
    let esp = image(&Opts {
        release,
        test_mode: true,
    })?;
    println!("[xtask] TEST boot.kernel_ready");
    let start = Instant::now();
    let mut child = qemu(&esp, true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("qemu: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let code = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st.code();
        }
        if start.elapsed() > TEST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = String::from_utf8_lossy(&reader.join().unwrap()).into_owned();
    let log = root().join("build/test-serial.log");
    let _ = std::fs::write(&log, &out);

    let ready = out.contains(READY_MARKER);
    let panicked = out.contains("KERNEL PANIC");
    let ok = ready && !panicked && code == Some(QEMU_EXIT_SUCCESS);
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        println!("    | {line}");
    }
    println!(
        "[{}] boot.kernel_ready  (marker={ready}, panic={panicked}, qemu_exit={code:?}, {:.1}s, log: {})",
        if ok { "PASS" } else { "FAIL" },
        start.elapsed().as_secs_f32(),
        log.display()
    );
    if ok {
        Ok(())
    } else {
        Err("boot test failed".into())
    }
}

fn check() -> Result<(), String> {
    let mut c = cargo();
    c.args(["fmt", "--all", "--", "--check"]);
    status(c)?;
    for (pkg, target) in [
        ("boot-protocol", None),
        ("ariboot", Some(UEFI_TARGET)),
        ("arikernel", Some(KERNEL_TARGET)),
        ("xtask", None),
    ] {
        let mut c = cargo();
        c.args(["clippy", "-p", pkg]);
        if let Some(t) = target {
            c.args(["--target", t]);
        }
        c.args(["--", "-D", "warnings"]);
        status(c)?;
    }
    Ok(())
}

fn status(mut c: Command) -> Result<(), String> {
    let st = c
        .status()
        .map_err(|e| format!("{:?}: {e}", c.get_program()))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("comando fallido: {:?}", c))
    }
}
