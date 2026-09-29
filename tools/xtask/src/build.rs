//! Compilación de las crates del sistema y comprobaciones estáticas.

use crate::{root, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::string::String;

pub const UEFI_TARGET: &str = "x86_64-unknown-uefi";
pub const KERNEL_TARGET: &str = "x86_64-unknown-none";
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Paquetes del workspace y el target con el que se compilan/lintan.
/// Única fuente de verdad para `check` y para los tests unitarios de host.
pub const PACKAGES: &[(&str, Option<&str>)] = &[
    ("boot-protocol", None),
    ("aricode-elf", None),
    ("aricode-x86-paging", None),
    ("aricode-cmdline", None),
    ("aricode-fb-console", None),
    ("aricode-x86-cpu", None),
    ("aricode-pmm", None),
    ("aricode-heap", None),
    ("aricode-sync", None),
    ("aricode-sched", None),
    ("aricode-abi", None),
    ("aricode-handles", None),
    ("aricode-initrd", None),
    ("aricode-acpi", None),
    ("aricode-pci", None),
    ("aricode-virtio", None),
    ("aricode-gpt", None),
    ("aricode-arifs", None),
    ("aricode-net", None),
    ("aricode-http", None),
    ("aricode-input", None),
    ("aricode-gfx", None),
    ("aricode-manifest", None),
    ("aricode-pkg", None),
    ("aricode-wasm", None),
    ("aricode-crypto", None),
    ("aricode-ab", None),
    ("aricode-proto", None),
    ("aricode-sys", None),
    ("aricode-build", None),
    ("aricode-rt", Some(KERNEL_TARGET)),
    ("ari-app-hello", Some(KERNEL_TARGET)),
    ("ari-app-perms-check", Some(KERNEL_TARGET)),
    ("ari-app-updater", Some(KERNEL_TARGET)),
    ("ari-app-fs-probe", Some(KERNEL_TARGET)),
    ("ari-app-win-probe", Some(KERNEL_TARGET)),
    ("ari-app-wasm-runner", Some(KERNEL_TARGET)),
    ("ari-app-notes-reader", Some(KERNEL_TARGET)),
    ("ari-app-installer", Some(KERNEL_TARGET)),
    ("wasm-demo", Some(WASM_TARGET)),
    ("user-testprogs", Some(KERNEL_TARGET)),
    ("ariinit", Some(KERNEL_TARGET)),
    ("ari-virtio-blk", Some(KERNEL_TARGET)),
    ("ari-virtio-net", Some(KERNEL_TARGET)),
    ("ari-virtio-input", Some(KERNEL_TARGET)),
    ("ari-usb-input", Some(KERNEL_TARGET)),
    ("ari-console-bridge", Some(KERNEL_TARGET)),
    ("ari-compositor", Some(KERNEL_TARGET)),
    ("ari-fsd", Some(KERNEL_TARGET)),
    ("ari-shell", Some(KERNEL_TARGET)),
    ("ariboot", Some(UEFI_TARGET)),
    ("arikernel", Some(KERNEL_TARGET)),
    ("xtask", None),
];

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

pub fn run(mut c: Command) -> Result {
    let st = c
        .status()
        .map_err(|e| format!("{:?}: {e}", c.get_program()))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("comando fallido: {c:?}"))
    }
}

fn profile_dir(release: bool) -> &'static str {
    if release {
        "release"
    } else {
        "debug"
    }
}

pub struct Artifacts {
    pub ariboot: PathBuf,
    pub kernel: PathBuf,
    /// Directorio con `init` y los programas de usuario.
    pub user_bin: PathBuf,
}

/// Programas que van al initrd: `init`, los servicios, las aplicaciones y los
/// binarios de `user/testprogs`.
pub fn initrd_programs() -> Result<Vec<String>> {
    let mut names = vec![
        String::from("init"),
        String::from("virtio_blk"),
        String::from("virtio_net"),
        String::from("virtio_input"),
        String::from("usb_input"),
        String::from("console_bridge"),
        String::from("compositor"),
        String::from("fsd"),
        String::from("hello_app"),
        String::from("perms_check"),
        String::from("updater"),
        String::from("fs_victim"),
        String::from("fs_probe"),
        String::from("win_probe"),
        String::from("wasm_runner"),
        String::from("notes_reader"),
        String::from("installer"),
        String::from("shell"),
    ];
    let bins = root().join("user/testprogs/src/bin");
    for entry in std::fs::read_dir(&bins).map_err(|e| format!("{}: {e}", bins.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "rs") {
            names.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Manifiestos de aplicaciones (`apps/*/NOMBRE.manifest`): van al initrd con
/// su nombre de fichero, junto al programa que describen.
pub fn initrd_manifests() -> Result<Vec<(String, PathBuf)>> {
    let apps = root().join("apps");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&apps).map_err(|e| format!("{}: {e}", apps.display()))? {
        let dir = entry.map_err(|e| e.to_string())?.path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = f.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|x| x == "manifest") {
                out.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    path,
                ));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Compila `init` y los programas de usuario (ELF estáticos en 0x400000) y
/// devuelve el directorio con los binarios.
pub fn build_user_programs(release: bool) -> Result<PathBuf> {
    for pkg in [
        "user-testprogs",
        "ariinit",
        "ari-virtio-blk",
        "ari-virtio-net",
        "ari-virtio-input",
        "ari-usb-input",
        "ari-console-bridge",
        "ari-compositor",
        "ari-fsd",
        "ari-app-hello",
        "ari-app-perms-check",
        "ari-app-updater",
        "ari-app-fs-probe",
        "ari-app-win-probe",
        "ari-app-wasm-runner",
        "ari-app-notes-reader",
        "ari-app-installer",
        "ari-shell",
    ] {
        println!("[xtask] build {pkg} ({KERNEL_TARGET})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", KERNEL_TARGET]);
        if release {
            c.arg("--release");
        }
        // Mismos flags que el kernel (.cargo/config.toml): el modelo de código
        // `kernel` admite direcciones absolutas de 32 bits con signo, así que
        // 0x400000 es válido, y coincide con el `core` precompilado (LTO).
        run(c)?;
    }
    Ok(root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile_dir(release)))
}

/// Programa construido fuera del árbol y empaquetado (ADR 0017).
pub struct ExternalApp {
    pub name: String,
    pub version: u32,
    /// Paquete firmado (`aricode-pkg`).
    pub package: Vec<u8>,
}

impl ExternalApp {
    /// Fichero que va a AriFS como programa instalado.
    pub fn installed(&self) -> (String, Vec<u8>) {
        (
            format!("apps/{}{}", self.name, aricode_pkg::EXTENSION),
            self.package.clone(),
        )
    }
}

/// Compila `dir/Cargo.toml` (un workspace propio con el SDK por ruta) desde
/// la raíz del repositorio, para que `rust-toolchain.toml` y
/// `.cargo/config.toml` se apliquen igual que a los programas del initrd, con
/// `APP_VERSION` fijada; firma el paquete con la clave de desarrollo. `dir`
/// lleva un único `NOMBRE.manifest`, que da el nombre del binario. Con
/// `corrupt`, la firma no verifica (pruebas).
pub fn build_external(
    dir: &Path,
    version: u32,
    release: bool,
    corrupt: bool,
) -> Result<ExternalApp> {
    let dir = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        root().join(dir)
    };
    let mut manifests = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "manifest"));
    let (Some(manifest_path), None) = (manifests.next(), manifests.next()) else {
        return Err(format!(
            "{}: hace falta exactamente un NOMBRE.manifest",
            dir.display()
        ));
    };
    let name = manifest_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let target_dir = crate::build_dir()
        .join("external")
        .join(format!("{name}-v{version}"));
    println!(
        "[xtask] build externo {name} v{version} ({}, {KERNEL_TARGET})",
        dir.display()
    );
    let mut c = cargo();
    c.env("APP_VERSION", version.to_string())
        .args(["build", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(["--target", KERNEL_TARGET, "--target-dir"])
        .arg(&target_dir);
    if release {
        c.arg("--release");
    }
    run(c)?;
    let elf = target_dir
        .join(KERNEL_TARGET)
        .join(profile_dir(release))
        .join(&name);
    let elf = crate::image::stripped_elf(&elf)?;
    let manifest =
        std::fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let seed: [u8; 32] = crate::keys::read_hex(
        &root()
            .join(crate::keys::DEV_KEY_DIR)
            .join(crate::keys::SEED_FILE),
    )?;
    let package = aricode_pkg::build(&name, version, &elf, &manifest, &seed, corrupt)
        .ok_or_else(|| format!("{name}: nombre de paquete invalido"))?;
    Ok(ExternalApp {
        name,
        version,
        package,
    })
}

/// `cargo xtask app-pack DIR [--version N]`: deja `build/pkgs/NOMBRE-N.aripkg`.
pub fn app_pack(dir: &Path, version: u32, release: bool) -> Result {
    let app = build_external(dir, version, release, false)?;
    let out = crate::build_dir().join("pkgs");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let file = out.join(format!(
        "{}-{}{}",
        app.name,
        app.version,
        aricode_pkg::EXTENSION
    ));
    std::fs::write(&file, &app.package).map_err(|e| format!("{}: {e}", file.display()))?;
    println!(
        "[xtask] paquete {} v{} ({} bytes) en {}",
        app.name,
        app.version,
        app.package.len(),
        file.display()
    );
    Ok(())
}

/// Compila los módulos WebAssembly de ejemplo y deja `demo.wasm` en `build/`,
/// desde donde `write_test_disk` lo copia al directorio privado del runner.
pub fn build_wasm_programs(release: bool) -> Result {
    println!("[xtask] build wasm-demo ({WASM_TARGET})");
    let mut c = cargo();
    c.args(["build", "-p", "wasm-demo", "--target", WASM_TARGET]);
    if release {
        c.arg("--release");
    }
    run(c)?;
    let src = root()
        .join("target")
        .join(WASM_TARGET)
        .join(profile_dir(release))
        .join("wasm_demo.wasm");
    let dst = crate::build_dir().join("demo.wasm");
    std::fs::create_dir_all(crate::build_dir()).map_err(|e| e.to_string())?;
    std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
    Ok(())
}

/// Compila AriBoot y AriKernel. Un único binario de cada uno sirve para todos
/// los escenarios: el comportamiento de test se controla por cmdline.
pub fn build_os(release: bool) -> Result<Artifacts> {
    let user_bin = build_user_programs(release)?;
    build_wasm_programs(release)?;
    for (pkg, target) in [("ariboot", UEFI_TARGET), ("arikernel", KERNEL_TARGET)] {
        println!("[xtask] build {pkg} ({target})");
        let mut c = cargo();
        c.args(["build", "-p", pkg, "--target", target]);
        if release {
            c.arg("--release");
        }
        run(c)?;
    }
    let t = root().join("target");
    Ok(Artifacts {
        ariboot: t
            .join(UEFI_TARGET)
            .join(profile_dir(release))
            .join("ariboot.efi"),
        kernel: t
            .join(KERNEL_TARGET)
            .join(profile_dir(release))
            .join("arikernel"),
        user_bin,
    })
}

/// Tests unitarios de host de todas las crates que no son bare-metal.
pub fn unit_tests() -> Result {
    let mut c = cargo();
    c.args(["test", "--workspace"]);
    for (pkg, target) in PACKAGES {
        if target.is_some() {
            c.args(["--exclude", pkg]);
        }
    }
    run(c)
}

pub fn check() -> Result {
    let mut c = cargo();
    c.args(["fmt", "--all", "--", "--check"]);
    run(c)?;
    for (pkg, target) in PACKAGES {
        let mut c = cargo();
        c.args(["clippy", "-p", pkg]);
        if let Some(t) = target {
            c.args(["--target", t]);
        }
        c.args(["--", "-D", "warnings"]);
        run(c)?;
    }
    Ok(())
}
