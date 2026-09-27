//! xtask: automatización de AriCodeOS.
//!
//! ```text
//! cargo xtask build [--debug]                 compila AriBoot y AriKernel
//! cargo xtask image [--debug]                 build + monta build/esp/
//! cargo xtask run   [--debug] [--cmdline S]   image + QEMU interactivo (serie en stdio)
//! cargo xtask test  [opciones]                niveles de test (ver tests/README.md)
//!     --level unit|smoke|full                 por defecto: unit + smoke
//!     --all                                   unit + full (obligatorio antes de main)
//!     --area boot[,memory,...]                solo escenarios QEMU de esas áreas
//!     --list                                  muestra los escenarios y sale
//! cargo xtask check                           fmt --check + clippy -D warnings
//! cargo xtask keygen [DIR]                    par Ed25519 (por defecto keys/dev)
//! cargo xtask sign SEED FICHERO               escribe FICHERO.sig
//! cargo xtask verify PUB FICHERO [FIRMA]      comprueba una firma
//! ```

mod build;
mod image;
mod keys;
mod qemu;
mod scenario;
mod test;
#[cfg(test)]
mod test_util;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub struct Cli {
    pub args: Vec<String>,
}

impl Cli {
    fn flag(&self, name: &str) -> bool {
        self.args.iter().any(|a| a == name)
    }
    fn value(&self, name: &str) -> Option<&str> {
        let i = self.args.iter().position(|a| a == name)?;
        self.args.get(i + 1).map(String::as_str)
    }
    /// Argumento posicional `n` (desde 1), ignorando opciones `--x`.
    fn positional(&self, n: usize) -> Option<&str> {
        self.args
            .iter()
            .filter(|a| !a.starts_with("--"))
            .nth(n - 1)
            .map(String::as_str)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let cli = Cli {
        args: args[1.min(args.len())..].to_vec(),
    };
    let release = !cli.flag("--debug");

    let result = match cmd.as_str() {
        "build" => build::build_os(release).map(|_| ()),
        "image" => image::build_base_esp(release).map(|_| ()),
        "run" => qemu::run_interactive(release, cli.value("--cmdline").unwrap_or("")),
        "test" => test::run(&cli, release),
        "check" => build::check(),
        "keygen" => keys::keygen(&root().join(cli.positional(1).unwrap_or(keys::DEV_KEY_DIR))),
        "sign" => match (cli.positional(1), cli.positional(2)) {
            (Some(seed), Some(file)) => keys::sign(Path::new(seed), Path::new(file)),
            _ => Err(String::from("uso: xtask sign SEED FICHERO")),
        },
        "verify" => match (cli.positional(1), cli.positional(2)) {
            (Some(pk), Some(file)) => keys::verify(
                Path::new(pk),
                Path::new(file),
                cli.positional(3).map(Path::new),
            ),
            _ => Err(String::from("uso: xtask verify PUB FICHERO [FIRMA]")),
        },
        _ => {
            eprintln!("uso: cargo xtask <build|image|run|test|check> [--debug] (ver tools/xtask/src/main.rs)");
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

pub type Result<T = ()> = std::result::Result<T, String>;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

pub fn build_dir() -> PathBuf {
    root().join("build")
}
