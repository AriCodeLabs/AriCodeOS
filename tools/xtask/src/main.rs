// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! xtask: AriCodeOS automation.
//!
//! ```text
//! cargo xtask build [--debug]                 builds AriBoot and AriKernel
//! cargo xtask image [--debug]                 build + assembles build/esp/
//! cargo xtask run   [--debug] [--cmdline S] [--cpus N] [--usb-image] [--usb-hid]   image + interactive QEMU
//! cargo xtask usb-image [--cmdline S] [--authorized-key KEY.pub] [--update-url URL]  GPT image for a USB stick at build/aricodeos-usb.img
//! cargo xtask publish-update [--version N]   signed system image + manifest in build/update/ (ADR 0027)
//! cargo xtask serve-update [--version N] [--port P]  publish-update + HTTP server for `update` on the machines (default port 8080)
//! cargo xtask test  [opciones]                test levels (see tests/README.md)
//! cargo xtask fs-cat DISK [NAME]             reads a file from ARI0 (or lists it) in an image or dump
//!     --level unit|smoke|full                 default: unit + smoke
//!     --all                                   unit + full (required before main)
//!     --area boot[,memory,...]                only QEMU scenarios from those areas
//!     --cpus N                                N processors for scenarios that do not declare cpus
//!     --list                                  lists the scenarios and exits
//! cargo xtask check                           fmt --check + clippy -D warnings
//! cargo xtask app-pack DIR [--version N] [--debug]   builds, packs and signs an external app (DIR/Cargo.toml + NAME.manifest) into build/pkgs/
//! cargo xtask keygen [DIR]                    Ed25519 key pair (default keys/dev)
//! cargo xtask sign SEED FILE                  writes FILE.sig
//! cargo xtask verify PUB FILE [SIG]           checks a signature
//! ```

mod build;
mod fscat;
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
    /// Positional argument `n` (1-based), ignoring `--x` options.
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
        "run" => qemu::run_interactive(
            release,
            cli.value("--cmdline").unwrap_or(""),
            cli.value("--cpus")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            cli.flag("--usb-image"),
            cli.flag("--usb-hid"),
        ),
        "publish-update" => image::publish_update(release, version_flag(&cli)).map(|_| ()),
        "serve-update" => image::publish_update(release, version_flag(&cli)).and_then(|dir| {
            qemu::serve_update(
                &dir,
                cli.value("--port")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8080),
            )
        }),
        "usb-image" => image::build_usb_image(
            release,
            cli.value("--cmdline").unwrap_or("shell"),
            cli.value("--authorized-key").map(Path::new),
            cli.value("--update-url"),
        )
            .map(|p| {
                println!(
                    "[xtask] write with: sudo dd if={} of=/dev/sdX bs=4M conv=fsync status=progress (see docs/hardware.md)",
                    p.display()
                )
            }),
        "test" => test::run(&cli, release),
        "check" => build::check(),
        "app-pack" => match cli.positional(1) {
            Some(dir) => build::app_pack(
                Path::new(dir),
                cli.value("--version")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1),
                release,
            ),
            None => Err(String::from("usage: xtask app-pack DIR [--version N]")),
        },
        "fs-cat" => match cli.positional(1) {
            Some(disk) => fscat::run(Path::new(disk), cli.positional(2)),
            None => Err(String::from("usage: xtask fs-cat DISK [NAME]")),
        },
        "keygen" => keys::keygen(&root().join(cli.positional(1).unwrap_or(keys::DEV_KEY_DIR))),
        "sign" => match (cli.positional(1), cli.positional(2)) {
            (Some(seed), Some(file)) => keys::sign(Path::new(seed), Path::new(file)),
            _ => Err(String::from("usage: xtask sign SEED FILE")),
        },
        "verify" => match (cli.positional(1), cli.positional(2)) {
            (Some(pk), Some(file)) => keys::verify(
                Path::new(pk),
                Path::new(file),
                cli.positional(3).map(Path::new),
            ),
            _ => Err(String::from("usage: xtask verify PUB FILE [SIG]")),
        },
        _ => {
            eprintln!("usage: cargo xtask <build|image|run|test|check|usb-image|fs-cat|app-pack|keygen|sign|verify> [--debug] (see tools/xtask/src/main.rs)");
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

/// `--version N` (default 1).
fn version_flag(cli: &Cli) -> u32 {
    cli.value("--version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}
