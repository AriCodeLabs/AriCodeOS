// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! xtask: AriCodeOS automation.
//!
//! ```text
//! cargo xtask build [--debug]                 builds AriBoot and AriKernel
//! cargo xtask image [--debug]                 build + assembles build/esp/
//! cargo xtask run   [--debug] [--cmdline S] [--cpus N] [--usb-image] [--usb-hid]   image + interactive QEMU
//! cargo xtask usb-image [--cmdline S] [--authorized-key KEY.pub] [--public-source] [--update-source URL] [--dev-source URL] [--signing-key DIR]  GPT image for a USB stick at build/aricodeos-usb.img
//!                                             (--public-source: the project's GitHub releases, ADR 0048)
//!                                             (--dev-source: the machine opts in to the development channel, ADR 0045)
//! cargo xtask publish-update [--version N | --no-system] [--app DIR:VERSION]... [--no-legacy-manifest] [--signing-key DIR]
//!                                             update source in build/update/: signed image, packages, index + index.sig, ADR 0027 manifest.txt (ADR 0029)
//! cargo xtask publish-update --channel dev [--signing-key DIR]
//!                                             development build in build/update-dev/: the next build number and this checkout's commit (ADR 0045)
//! cargo xtask serve-update [publish-update options] [--port P]  publish-update + HTTP server for `update` on the machines (default port 8080, 8081 for --channel dev)
//!     --signing-key DIR                       sign with DIR/ed25519.seed and build with DIR/ed25519.pub (default keys/dev, with a warning;
//!                                             --channel dev: the development channel's key, e.g. ~/.config/aricodeos/dev-signing-key)
//! cargo xtask test  [options]                 test levels (see tests/README.md)
//! cargo xtask fs-cat DISK [NAME]             reads a file from ARI0 (or lists it) in an image or dump
//!     --level unit|smoke|full                 default: unit + smoke
//!     --all                                   unit + full (required before main)
//!     --area boot[,memory,...]                only QEMU scenarios from those areas
//!     --cpus N                                N processors for scenarios that do not declare cpus
//!     --jobs N                                N QEMU scenarios at a time (default: CPUs / 2, CPUs / 4 with
//!                                             --cpus > 1, capped by memory; 1 = one after another)
//!     --list                                  lists the scenarios and exits
//! cargo xtask perf [--cpus N] [--update-baseline]  perf.bench alone, compared with tests/perf/baseline-Ncpu.txt (finding G)
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
mod perf;
mod ports;
mod qemu;
mod scenario;
mod stack;
mod suite_lock;
mod test;
#[cfg(test)]
mod test_util;
mod typing;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// How an option takes its value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Opt {
    /// No value (`--all`).
    Flag,
    /// One value, given once (`--cpus 4`).
    Value,
    /// One value, given any number of times (`--app A --app B`).
    Repeated,
}

/// What a command accepts: its options and how many positional arguments
/// at most. Anything else is refused before the command runs, so a
/// mistyped option never turns into a silently different run.
struct Spec {
    options: &'static [(&'static str, Opt)],
    positional: usize,
}

const DEBUG: (&str, Opt) = ("--debug", Opt::Flag);
const CPUS: (&str, Opt) = ("--cpus", Opt::Value);
const CMDLINE: (&str, Opt) = ("--cmdline", Opt::Value);
const SIGNING_KEY: (&str, Opt) = ("--signing-key", Opt::Value);
const PUBLISH: [(&str, Opt); 7] = [
    DEBUG,
    ("--channel", Opt::Value),
    SIGNING_KEY,
    ("--version", Opt::Value),
    ("--no-system", Opt::Flag),
    ("--app", Opt::Repeated),
    ("--no-legacy-manifest", Opt::Flag),
];
const SERVE: [(&str, Opt); 8] = [
    PUBLISH[0],
    PUBLISH[1],
    PUBLISH[2],
    PUBLISH[3],
    PUBLISH[4],
    PUBLISH[5],
    PUBLISH[6],
    ("--port", Opt::Value),
];

/// The spec of `cmd`, `None` for an unknown command.
fn spec(cmd: &str) -> Option<Spec> {
    let (options, positional): (&'static [(&'static str, Opt)], usize) = match cmd {
        "build" | "image" => (&[DEBUG], 0),
        "run" => (
            &[
                DEBUG,
                CMDLINE,
                CPUS,
                ("--usb-image", Opt::Flag),
                ("--usb-hid", Opt::Flag),
            ],
            0,
        ),
        "publish-update" => (&PUBLISH, 0),
        "serve-update" => (&SERVE, 0),
        "usb-image" => (
            &[
                DEBUG,
                CMDLINE,
                ("--authorized-key", Opt::Value),
                ("--public-source", Opt::Flag),
                ("--update-source", Opt::Value),
                ("--dev-source", Opt::Value),
                SIGNING_KEY,
            ],
            0,
        ),
        "test" => (
            &[
                DEBUG,
                ("--level", Opt::Value),
                ("--all", Opt::Flag),
                ("--area", Opt::Value),
                CPUS,
                ("--jobs", Opt::Value),
                ("--list", Opt::Flag),
            ],
            0,
        ),
        "perf" => (&[DEBUG, CPUS, ("--update-baseline", Opt::Flag)], 0),
        "check" | "stack-check" => (&[], 0),
        "app-pack" => (&[DEBUG, ("--version", Opt::Value)], 1),
        "fs-cat" | "sign" => (&[], 2),
        "keygen" => (&[], 1),
        "verify" => (&[], 3),
        _ => return None,
    };
    Some(Spec {
        options,
        positional,
    })
}

/// A command's arguments, checked against its [`Spec`].
pub struct Cli {
    /// Options in the order given, with their values.
    options: Vec<(&'static str, Option<String>)>,
    positional: Vec<String>,
}

impl Cli {
    fn parse(spec: &Spec, args: &[String]) -> Result<Self> {
        let known = |name: &str| spec.options.iter().find(|(n, _)| *n == name).copied();
        let names = || {
            let list: Vec<&str> = spec.options.iter().map(|(n, _)| *n).collect();
            if list.is_empty() {
                String::from("it takes no options")
            } else {
                format!("options: {}", list.join(" "))
            }
        };
        let mut cli = Cli {
            options: Vec::new(),
            positional: Vec::new(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if !arg.starts_with('-') || arg == "-" {
                if cli.positional.len() == spec.positional {
                    return Err(format!("unexpected argument '{arg}'"));
                }
                cli.positional.push(arg.clone());
                continue;
            }
            let Some((name, kind)) = known(arg) else {
                return Err(format!("unknown option '{arg}' ({})", names()));
            };
            if kind != Opt::Repeated && cli.options.iter().any(|(n, _)| *n == name) {
                return Err(format!("{name} given twice"));
            }
            let value = match kind {
                Opt::Flag => None,
                Opt::Value | Opt::Repeated => match args.next() {
                    Some(v) if !v.starts_with("--") => Some(v.clone()),
                    _ => return Err(format!("{name} needs a value")),
                },
            };
            cli.options.push((name, value));
        }
        Ok(cli)
    }

    fn flag(&self, name: &str) -> bool {
        self.options.iter().any(|(n, _)| *n == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values(name).into_iter().next()
    }

    /// Every value of an option that may repeat (`--app A --app B`).
    fn values(&self, name: &str) -> Vec<&str> {
        self.options
            .iter()
            .filter(|(n, _)| *n == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// Positional argument `n` (1-based); option values never count.
    fn positional(&self, n: usize) -> Option<&str> {
        self.positional.get(n - 1).map(String::as_str)
    }

    /// The number `name` gives, `default` without it; a value that does not
    /// parse is refused, never replaced by the default.
    fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T> {
        match self.value(name) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("{name} {v}: invalid number")),
        }
    }

    /// `--cpus N`, from 1 to 64; `None` without it.
    fn cpus(&self) -> Result<Option<u32>> {
        self.value("--cpus")
            .map(|v| {
                v.parse()
                    .ok()
                    .filter(|n| (1..=64).contains(n))
                    .ok_or_else(|| format!("--cpus {v}: expected a number from 1 to 64"))
            })
            .transpose()
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let Some(spec) = spec(&cmd) else {
        eprintln!("usage: cargo xtask <build|image|run|test|perf|check|stack-check|usb-image|publish-update|serve-update|fs-cat|app-pack|keygen|sign|verify> [options] (see tools/xtask/src/main.rs)");
        return ExitCode::from(2);
    };
    let cli = match Cli::parse(&spec, &args[1.min(args.len())..]) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("xtask {cmd}: {e}");
            return ExitCode::from(2);
        }
    };
    let release = !cli.flag("--debug");

    let result = match cmd.as_str() {
        "build" => build::build_os(release).map(|_| ()),
        "image" => image::build_base_esp(release).map(|_| ()),
        "run" => cli.cpus().and_then(|cpus| {
            qemu::run_interactive(
                release,
                cli.value("--cmdline").unwrap_or(""),
                cpus.unwrap_or(1),
                cli.flag("--usb-image"),
                cli.flag("--usb-hid"),
            )
        }),
        "publish-update" => publication(&cli)
            .and_then(|p| image::publish_update_to(&image::update_dir_for(p.channel), release, &p))
            .map(|_| ()),
        "serve-update" => publication(&cli).and_then(|p| {
            let default_port = match p.channel {
                aricode_ab::Channel::Stable => 8080,
                aricode_ab::Channel::Dev => 8081,
            };
            let port = cli.number("--port", default_port)?;
            let dir = image::publish_update_to(&image::update_dir_for(p.channel), release, &p)?;
            qemu::serve_update(&dir, port)
        }),
        "usb-image" => signing_key(&cli).and_then(|()| image::build_usb_image(
            release,
            cli.value("--cmdline").unwrap_or("shell"),
            cli.value("--authorized-key").map(Path::new),
            cli.flag("--public-source"),
            cli.value("--update-source"),
            cli.value("--dev-source"),
        ))
            .map(|p| {
                println!(
                    "[xtask] write with: sudo dd if={} of=/dev/sdX bs=4M conv=fsync status=progress (see docs/hardware.md)",
                    p.display()
                )
            }),
        "test" => test::run(&cli, release),
        "perf" => perf::run(&cli, release),
        "check" => build::check(),
        "stack-check" => stack::check(),
        "app-pack" => match cli.positional(1) {
            Some(dir) => cli
                .number("--version", 1)
                .and_then(|version| build::app_pack(Path::new(dir), version, release)),
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
        _ => unreachable!("every command with a spec is dispatched"),
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

/// `--channel stable|dev` (default stable), the key that signs it, and
/// what it publishes (ADR 0045).
fn publication(cli: &Cli) -> Result<image::Publication> {
    let channel = match cli.value("--channel") {
        None => aricode_ab::Channel::Stable,
        Some(c) => {
            aricode_ab::Channel::parse(c).ok_or_else(|| format!("--channel {c}: stable or dev"))?
        }
    };
    keys::select_keys(cli.value("--signing-key").map(Path::new), channel)?;
    match channel {
        aricode_ab::Channel::Stable => channel_flags(cli),
        aricode_ab::Channel::Dev => {
            // The build number is the source's own (one above the last
            // published), never typed: no hand-picked versions.
            for flag in ["--version", "--no-system", "--app", "--no-legacy-manifest"] {
                if cli.flag(flag) {
                    return Err(format!(
                        "--channel dev: {flag} does not apply (a development source offers one system image, numbered automatically)"
                    ));
                }
            }
            Ok(image::Publication {
                channel,
                ..image::Publication::default()
            })
        }
    }
}

/// What `publish-update` publishes: `--version N` (default 1) or
/// `--no-system`, and `--app DIR:VERSION` for each package.
fn channel_flags(cli: &Cli) -> Result<image::Publication> {
    let system = if cli.flag("--no-system") {
        None
    } else {
        Some(cli.number("--version", 1)?)
    };
    Ok(image::Publication {
        system,
        apps: cli
            .values("--app")
            .into_iter()
            .map(app_spec)
            .collect::<Result<_>>()?,
        legacy_manifest: !cli.flag("--no-legacy-manifest"),
        ..image::Publication::default()
    })
}

/// `--signing-key DIR` (default: the development key, with a warning).
fn signing_key(cli: &Cli) -> Result {
    keys::select_signing_key(cli.value("--signing-key").map(Path::new))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(cmd: &str, args: &[&str]) -> Result<Cli> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        Cli::parse(&spec(cmd).expect("known command"), &args)
    }

    #[test]
    fn unknown_options_are_refused() {
        let e = parse("test", &["--all", "--only", "update.x"])
            .err()
            .unwrap();
        assert!(
            e.starts_with("unknown option '--only' (options: --debug --level"),
            "{e}"
        );
        assert!(parse("check", &["--debug"]).is_err());
        assert!(parse("test", &["--cpus=4"]).is_err());
        assert!(parse("test", &["-v"]).is_err());
        assert!(spec("tset").is_none());
    }

    #[test]
    fn values_never_count_as_positional_arguments() {
        let cli = parse("app-pack", &["--version", "3", "apps/demo"]).unwrap();
        assert_eq!(cli.positional(1), Some("apps/demo"));
        assert_eq!(cli.number("--version", 1), Ok(3));
        assert!(parse("app-pack", &["apps/demo", "extra"]).is_err());
        assert!(parse("check", &["now"]).is_err());
    }

    #[test]
    fn values_are_required_and_given_once() {
        assert_eq!(
            parse("test", &["--area"]).err().unwrap(),
            "--area needs a value"
        );
        assert!(parse("test", &["--area", "--all"]).is_err());
        assert_eq!(
            parse("test", &["--cpus", "4", "--cpus", "1"])
                .err()
                .unwrap(),
            "--cpus given twice"
        );
        assert!(parse("test", &["--all", "--all"]).is_err());
        let cli = parse("publish-update", &["--app", "a:1", "--app", "b:2"]).unwrap();
        assert_eq!(cli.values("--app"), ["a:1", "b:2"]);
    }

    #[test]
    fn invalid_numbers_are_refused_not_defaulted() {
        let cli = parse("serve-update", &["--port", "80x"]).unwrap();
        assert!(cli.number::<u16>("--port", 8080).is_err());
        assert_eq!(
            parse("serve-update", &[])
                .unwrap()
                .number("--port", 8080u16),
            Ok(8080)
        );
        for bad in ["0", "65", "two"] {
            assert!(
                parse("run", &["--cpus", bad]).unwrap().cpus().is_err(),
                "{bad}"
            );
        }
        assert_eq!(parse("run", &["--cpus", "4"]).unwrap().cpus(), Ok(Some(4)));
        assert_eq!(parse("run", &[]).unwrap().cpus(), Ok(None));
    }

    #[test]
    fn every_documented_invocation_parses() {
        for (cmd, args) in [
            (
                "test",
                &["--all", "--area", "update", "--cpus", "4", "--jobs", "6"][..],
            ),
            ("test", &["--debug", "--level", "unit", "--list"]),
            (
                "run",
                &["--cmdline", "qemu_exit shell", "--usb-image", "--usb-hid"],
            ),
            (
                "usb-image",
                &[
                    "--public-source",
                    "--signing-key",
                    "k",
                    "--authorized-key",
                    "a.pub",
                ],
            ),
            (
                "serve-update",
                &["--channel", "dev", "--signing-key", "k", "--port", "8081"],
            ),
            (
                "publish-update",
                &[
                    "--version",
                    "16",
                    "--no-legacy-manifest",
                    "--signing-key",
                    "k",
                ],
            ),
            ("perf", &["--cpus", "4", "--update-baseline"]),
            ("verify", &["k.pub", "index", "index.sig"]),
            ("fs-cat", &["disk.img", "diag.txt"]),
        ] {
            assert!(parse(cmd, args).is_ok(), "{cmd} {args:?}");
        }
    }
}
