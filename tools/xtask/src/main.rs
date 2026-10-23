// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! xtask: AriCodeOS automation.
//!
//! ```text
//! cargo xtask build [--debug]                 builds AriBoot and AriKernel
//! cargo xtask image [--debug]                 build + assembles build/esp/
//! cargo xtask run   [--debug] [--cmdline S] [--cpus N] [--usb-image] [--usb-hid]   image + interactive QEMU
//! cargo xtask usb-image [--cmdline S] [--authorized-key KEY.pub] [--update-source URL] [--signing-key DIR]  GPT image for a USB stick at build/aricodeos-usb.img
//! cargo xtask publish-update [--version N | --no-system] [--app DIR:VERSION]... [--no-legacy-manifest] [--signing-key DIR]
//!                                             update source in build/update/: signed image, packages, index + index.sig, ADR 0027 manifest.txt (ADR 0029)
//! cargo xtask serve-update [publish-update options] [--port P]  publish-update + HTTP server for `update` on the machines (default port 8080)
//!     --signing-key DIR                       sign with DIR/ed25519.seed and build with DIR/ed25519.pub (default keys/dev, with a warning)
//! cargo xtask test  [opciones]                test levels (see tests/README.md)
//! cargo xtask fs-cat DISK [NAME]             reads a file from ARI0 (or lists it) in an image or dump
//!     --level unit|smoke|full                 default: unit + smoke
//!     --all                                   unit + full (required before main)
//!     --area boot[,memory,...]                only QEMU scenarios from those areas
//!     --cpus N                                N processors for scenarios that do not declare cpus
//!     --jobs N                                N QEMU scenarios at a time (default: CPUs / 2, CPUs / 4 with
//!                                             --cpus > 1, capped by memory; 1 = one after another)
//!     --list                                  lists the scenarios and exits
//! cargo xtask check                           fmt --check + clippy -D warnings + stack-check
//! cargo xtask stack-check                     stack budget of every initrd program, release and dev (incident 0018)
//! cargo xtask app-pack DIR [--version N] [--debug]   builds, packs and signs an external app (DIR/Cargo.toml + NAME.manifest) into build/pkgs/
//! cargo xtask keygen [DIR]                    Ed25519 key pair (default keys/dev)
//! cargo xtask sign SEED FILE                  writes FILE.sig
//! cargo xtask verify PUB FILE [SIG]           checks a signature
//! ```

mod build;
mod fscat;
mod image;
mod keys;
mod parallel;
mod ports;
mod qemu;
mod scenario;
mod stack;
mod test;
#[cfg(test)]
mod test_util;
mod typing;

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
    /// Every value of an option that may repeat (`--app A --app B`).
    fn values(&self, name: &str) -> Vec<&str> {
        self.args
            .windows(2)
            .filter(|w| w[0] == name)
            .map(|w| w[1].as_str())
            .collect()
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
        "publish-update" => signing_key(&cli)
            .and_then(|()| channel_flags(&cli))
            .and_then(|c| image::publish_update_to(&image::update_dir(), release, &c))
            .map(|_| ()),
        "serve-update" => signing_key(&cli)
            .and_then(|()| channel_flags(&cli))
            .and_then(|c| image::publish_update_to(&image::update_dir(), release, &c))
            .and_then(|dir| {
                qemu::serve_update(
                    &dir,
                    cli.value("--port")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(8080),
                )
            }),
        "usb-image" => signing_key(&cli).and_then(|()| image::build_usb_image(
            release,
            cli.value("--cmdline").unwrap_or("shell"),
            cli.value("--authorized-key").map(Path::new),
            cli.value("--update-source"),
        ))
            .map(|p| {
                println!(
                    "[xtask] write with: sudo dd if={} of=/dev/sdX bs=4M conv=fsync status=progress (see docs/hardware.md)",
                    p.display()
                )
            }),
        "test" => test::run(&cli, release),
        "check" => build::check(),
        "stack-check" => stack::check(),
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
            eprintln!("usage: cargo xtask <build|image|run|test|check|stack-check|usb-image|fs-cat|app-pack|keygen|sign|verify> [--debug] (see tools/xtask/src/main.rs)");
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

/// `DIR:VERSION` of an external app (scenarios and `--app`).
pub fn app_spec(spec: &str) -> Result<(String, u32)> {
    let (dir, version) = spec
        .rsplit_once(':')
        .ok_or_else(|| format!("'{spec}': expected DIR:VERSION"))?;
    let version = version
        .parse()
        .map_err(|_| format!("'{spec}': invalid version"))?;
    Ok((dir.to_string(), version))
}

/// What `publish-update` publishes: `--version N` (default 1) or
/// `--no-system`, and `--app DIR:VERSION` for each package.
fn channel_flags(cli: &Cli) -> Result<image::Channel> {
    let system = if cli.flag("--no-system") {
        None
    } else {
        Some(
            cli.value("--version")
                .map(|v| v.parse().map_err(|_| format!("--version {v}: invalid")))
                .transpose()?
                .unwrap_or(1),
        )
    };
    Ok(image::Channel {
        system,
        apps: cli
            .values("--app")
            .into_iter()
            .map(app_spec)
            .collect::<Result<_>>()?,
        legacy_manifest: !cli.flag("--no-legacy-manifest"),
        tamper_index: false,
        tamper_image: false,
    })
}

/// `--signing-key DIR` (default: the development key, with a warning).
fn signing_key(cli: &Cli) -> Result {
    keys::select_signing_key(cli.value("--signing-key").map(Path::new))
}
