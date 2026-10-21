// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test level orchestration: unit (host) -> smoke/full (QEMU).

use crate::scenario::{self, Level, Scenario};
use crate::{build, image, qemu, Cli, Result};
use std::time::Duration;

pub fn run(cli: &Cli, release: bool) -> Result {
    let level: Level = match (cli.flag("--all"), cli.value("--level")) {
        (true, _) => Level::Full,
        (false, Some(l)) => l.parse()?,
        (false, None) => Level::Smoke,
    };
    let areas: Vec<String> = cli
        .value("--area")
        .map(|a| {
            a.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let cpus_override = match cli.value("--cpus") {
        Some(v) => Some(
            v.parse::<u32>()
                .ok()
                .filter(|n| (1..=64).contains(n))
                .ok_or_else(|| format!("--cpus {v}: expected a number from 1 to 64"))?,
        ),
        None => None,
    };

    let all = scenario::load_all()?;
    let selected = scenario::select(&all, level, &areas)?;

    if cli.flag("--list") {
        for s in &all {
            let mark = if selected.iter().any(|x| x.name == s.name) {
                "*"
            } else {
                " "
            };
            println!(
                "{mark} {:<36} {:<6} {:<20} {}",
                s.name,
                format!("{:?}", s.level).to_lowercase(),
                s.areas.join(","),
                s.description
            );
        }
        return Ok(());
    }

    println!(
        "[xtask] level {level:?}{}{}",
        if areas.is_empty() {
            String::new()
        } else {
            format!(", areas {}", areas.join(","))
        },
        cpus_override.map_or(String::new(), |n| format!(", {n} CPUs in every scenario"))
    );

    // Unit level: always, even when filtering by area (a feature may want
    // only its QEMU scenarios; unit tests are still cheap and run anyway).
    println!("[xtask] unit tests (host)");
    build::unit_tests()?;
    if level == Level::Unit {
        return Ok(());
    }

    let base = image::build_base_esp(release)?;
    let mut failed = Vec::new();
    for s in &selected {
        if !run_scenario(&base, s, cpus_override, release)? {
            failed.push(s.name.clone());
        }
    }
    println!(
        "[xtask] {} QEMU scenarios, {} failed",
        selected.len(),
        failed.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("failed scenarios: {}", failed.join(", ")))
    }
}

/// One boot to run: the scenario's only boot, or each one in `[[boots]]`.
struct BootRun<'a> {
    cmdline: String,
    stdin: &'a [String],
    kill_on: Option<&'a str>,
    expect: &'a scenario::Expect,
}

fn run_scenario(
    base: &std::path::Path,
    s: &Scenario,
    cpus_override: Option<u32>,
    release: bool,
) -> Result<bool> {
    let root = image::scenarios_root();
    let workdir = image::scenario_dir(&root, &s.name)?;
    let runs: Vec<BootRun> = if s.boots.is_empty() {
        vec![BootRun {
            cmdline: s.boot_cmdline(&s.cmdline),
            stdin: &s.stdin,
            kill_on: None,
            expect: &s.expect,
        }]
    } else {
        s.boots
            .iter()
            .map(|b| BootRun {
                cmdline: s.boot_cmdline(&b.cmdline),
                stdin: &b.stdin,
                kill_on: b.kill_on.as_deref(),
                expect: &b.expect,
            })
            .collect()
    };
    let multi = !s.boots.is_empty();
    let disk_variant = image::DiskVariant::parse(&s.disk)
        .ok_or_else(|| format!("{}: unknown disk '{}'", s.name, s.disk))?;
    let mut installed: Vec<(String, Vec<u8>)> = Vec::new();
    for dir in &s.install {
        installed
            .push(build::build_external(std::path::Path::new(dir), 1, release, false)?.installed());
    }
    for spec in &s.packages {
        let mut parts = spec.split(':');
        let (Some(dir), Some(version)) = (parts.next(), parts.next()) else {
            return Err(format!(
                "{}: package '{spec}': DIR:VERSION[:corrupt]",
                s.name
            ));
        };
        let version: u32 = version
            .parse()
            .map_err(|_| format!("{}: package '{spec}': invalid version", s.name))?;
        let corrupt = match parts.next() {
            None => false,
            Some("corrupt") => true,
            Some(other) => return Err(format!("{}: package '{spec}': unknown '{other}'", s.name)),
        };
        let app = build::build_external(std::path::Path::new(dir), version, release, corrupt)?;
        installed.push((
            format!(
                "pkgs/{}-{}{}",
                app.name,
                app.version,
                aricode_pkg::EXTENSION
            ),
            app.package,
        ));
    }
    // Packages already staged for the installer (`installer` STAGED mode):
    // the stage files and the `[staged]` section of `updater/state`.
    if !s.staged.is_empty() || s.staged_bad_path {
        use aricode_update_index::lists::{self, StagedEntry};
        let mut packages = Vec::new();
        for spec in &s.staged {
            let (base, wrong_hash) = match spec.strip_suffix(":wrong-hash") {
                Some(b) => (b, true),
                None => (spec.as_str(), false),
            };
            let (dir, version) = crate::app_spec(base).map_err(|e| format!("{}: {e}", s.name))?;
            let app = build::build_external(std::path::Path::new(&dir), version, release, false)?;
            packages.push((app, wrong_hash));
        }
        let mut entries = Vec::new();
        for (i, (app, wrong_hash)) in packages.iter().enumerate() {
            let file = aricode_update_index::staged_package_file(i)
                .ok_or_else(|| format!("{}: too many staged packages", s.name))?;
            let mut sha256 = aricode_crypto::sha256::sha256(&app.package);
            if *wrong_hash {
                sha256[0] ^= 0xFF;
            }
            entries.push(StagedEntry {
                file,
                name: &app.name,
                version: app.version,
                sha256,
            });
            installed.push((aricode_update_index::main_path(file), app.package.clone()));
        }
        let mut body = lists::encode_staged(&entries);
        if s.staged_bad_path {
            body.push_str(&format!("pkgs/evil.aripkg evil 1 {}\n", "00".repeat(32)));
        }
        installed.push((
            aricode_update_index::main_path(aricode_update_index::STATE_FILE),
            lists::with_section("", lists::STAGED, &body).into_bytes(),
        ));
    }
    // SSH scenarios (and a flood of port 22, which needs sshd listening): a
    // client key pair in the scenario directory, its public half installed
    // as `authorized_keys`.
    let needs_ssh_key = s.host_ssh.is_some() || s.host_flood.as_ref().is_some_and(|f| f.port == 22);
    let ssh_key = match needs_ssh_key {
        true => {
            let dir = workdir.join("ssh");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let key = dir.join("id_ed25519");
            let st = std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "aricode-test", "-f"])
                .arg(&key)
                .status()
                .map_err(|e| format!("{}: ssh-keygen: {e}", s.name))?;
            if !st.success() {
                return Err(format!("{}: ssh-keygen failed", s.name));
            }
            let public = std::fs::read(dir.join("id_ed25519.pub")).map_err(|e| e.to_string())?;
            installed.push((String::from("authorized_keys"), public));
            Some(key)
        }
        false => None,
    };
    if let Some(u) = &s.host_update {
        let channel = image::Channel {
            system: u.system,
            apps: u
                .apps
                .iter()
                .map(|spec| crate::app_spec(spec).map_err(|e| format!("{}: {e}", s.name)))
                .collect::<Result<_>>()?,
            legacy_manifest: false,
            tamper_index: u.tamper_index,
            tamper_image: u.tamper_image,
        };
        image::publish_update_to(&image::test_update_dir(), release, &channel)?;
        let url = format!(
            "http://10.0.2.2:{}/update/\n",
            aricode_proto::net::TEST_HTTP_PORT
        );
        if u.stale_index {
            let ahead = image::stored_copy_ahead(&image::test_update_dir(), url.trim())?;
            installed.push((
                aricode_update_index::main_path(aricode_update_index::INDEXES_FILE),
                aricode_update_index::stored::encode_all(&[ahead]),
            ));
        }
        match u.source.as_str() {
            "sources" => installed.push((
                aricode_update_index::main_path(aricode_update_index::SOURCES_FILE),
                url.into_bytes(),
            )),
            "legacy-url" => installed.push((
                aricode_update_index::main_path(aricode_update_index::LEGACY_URL_FILE),
                url.into_bytes(),
            )),
            "none" => {}
            other => {
                return Err(format!(
                    "{}: host_update.source '{other}': sources, legacy-url or none",
                    s.name
                ))
            }
        }
    }
    let _https = if s.host_https {
        let (servers, ca) = https_servers(&workdir.join("tls"))?;
        installed.push((String::from("trusted_ca.der"), ca));
        Some(servers)
    } else {
        None
    };
    let mut problems = Vec::new();
    let mut elapsed = Duration::ZERO;
    let mut logs: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        // Each boot has its own ESP (its cmdline); the disk is shared.
        let esp_name = if multi {
            format!("{}.boot{}", s.name, i + 1)
        } else {
            s.name.clone()
        };
        let esp = image::build_scenario_esp(base, &root, &esp_name, &run.cmdline, &s.inject)?;
        let input = qemu::Input {
            prompt: &s.prompt,
            lines: run.stdin,
        };
        let probe = s.host_tcp.as_ref().map(|h| qemu::TcpProbe {
            marker: &h.marker,
            payload: &h.payload,
        });
        let input_probe = s.host_input.as_ref().map(|h| qemu::InputProbe {
            marker: &h.marker,
            keys: &h.keys,
            mouse_rel: h.mouse_rel,
            click: h.click.as_deref(),
            key_delay_ms: h.key_delay_ms,
        });
        let ssh_probe = match (&s.host_ssh, &ssh_key) {
            (Some(h), Some(key)) => Some(qemu::SshProbe {
                marker: &h.marker,
                commands: &h.commands,
                key: key.clone(),
                idle_client: h.idle_client,
            }),
            _ => None,
        };
        let flood = s.host_flood.as_ref().map(|f| qemu::FloodProbe {
            marker: &f.marker,
            port: f.port,
            connections: f.connections,
        });
        let options = qemu::Headless {
            input,
            probe,
            ssh_probe,
            flood,
            input_probe,
            disk_variant,
            disk_readonly: s.disk_readonly,
            keep_disk: i > 0,
            kill_on: run.kill_on,
            installed: &installed,
            cpus: s.cpus(cpus_override),
            usb: qemu::UsbOptions {
                hid: s.usb_hid,
                hid_msi: s.usb_hid_msi,
                hid_only_input: s.usb_hid_only_input,
                legacy_controller: s.usb_legacy_controller,
                storage: s.usb_storage,
                ehci_hid: s.usb_ehci_hid,
                ehci_storage: s.usb_ehci_storage,
                ehci_companion_kbd: s.usb_ehci_companion_kbd,
            },
            nic: !s.no_nic,
            flr_device: s.flr_device,
        };
        let cap = qemu::run_headless(
            &esp,
            &workdir,
            Duration::from_secs(s.timeout_secs),
            &options,
        )?;
        let log = workdir.join(if multi {
            format!("serial-{}.log", i + 1)
        } else {
            String::from("serial.log")
        });
        let _ = std::fs::write(&log, &cap.serial);
        elapsed += cap.elapsed;
        let prefix = if multi {
            format!("boot {}: ", i + 1)
        } else {
            String::new()
        };
        let expect = run.expect;
        if let Some(exit) = expect.exit {
            if cap.exit_code != Some(exit) {
                problems.push(format!("{prefix}exit {:?} != {exit}", cap.exit_code));
            }
        }
        if expect.killed != cap.killed {
            problems.push(format!(
                "{prefix}{}",
                if expect.killed {
                    "QEMU was not killed on kill_on"
                } else {
                    "QEMU killed unexpectedly"
                }
            ));
        }
        for needle in &expect.serial_contains {
            if !cap.serial.contains(needle) {
                problems.push(format!("{prefix}missing '{needle}'"));
            }
        }
        for needle in &expect.serial_absent {
            if cap.serial.contains(needle) {
                problems.push(format!("{prefix}unexpected '{needle}'"));
            }
        }
        if let Some(i) = scenario::first_out_of_order(&cap.serial, &expect.serial_order) {
            problems.push(format!(
                "{prefix}'{}' not found after the texts before it in serial_order",
                expect.serial_order[i]
            ));
        }
        if cap.timed_out {
            problems.push(format!("{prefix}timeout {}s", s.timeout_secs));
        }
        logs.push((log, cap.serial));
        if !problems.is_empty() {
            // Later boots depend on this one: they would tell nothing new.
            break;
        }
    }

    let ok = problems.is_empty();
    println!(
        "[{}] {:<36} {:>5.1}s  {}",
        if ok { "PASS" } else { "FAIL" },
        s.name,
        elapsed.as_secs_f32(),
        if ok {
            String::new()
        } else {
            problems.join("; ")
        }
    );
    if !ok {
        if let Some((log, serial)) = logs.last() {
            for line in serial.lines().filter(|l| !l.trim().is_empty()) {
                println!("      | {line}");
            }
            println!("      log: {}", log.display());
        }
    }
    Ok(ok)
}

/// HTTPS servers for `host_https` scenarios, killed when dropped.
struct HttpsServers(Vec<std::process::Child>);

impl Drop for HttpsServers {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Generates a test CA, a leaf for `10.0.2.2` signed by it and a
/// self-signed leaf, and starts one `openssl s_server -www` for each leaf.
/// Returns the servers and the CA certificate (DER).
fn https_servers(dir: &std::path::Path) -> Result<(HttpsServers, Vec<u8>)> {
    use std::process::{Command, Stdio};
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> Result {
        let st = Command::new("openssl")
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("openssl: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("openssl {} failed", args.join(" ")))
        }
    };
    let ec = [
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:prime256v1",
        "-nodes",
    ];
    run(&[
        &["req", "-x509"],
        &ec[..],
        &[
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "30",
            "-subj",
            "/CN=AriCodeOS test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign",
        ],
    ]
    .concat())?;
    run(&[
        &["req"],
        &ec[..],
        &[
            "-keyout",
            "leaf.key",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=10.0.2.2",
        ],
    ]
    .concat())?;
    std::fs::write(
        dir.join("leaf.ext"),
        "subjectAltName=DNS:10.0.2.2\nbasicConstraints=CA:FALSE\n",
    )
    .map_err(|e| e.to_string())?;
    run(&[
        "x509",
        "-req",
        "-in",
        "leaf.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "leaf.pem",
        "-days",
        "30",
        "-extfile",
        "leaf.ext",
    ])?;
    run(&[
        &["req", "-x509"],
        &ec[..],
        &[
            "-keyout",
            "self.key",
            "-out",
            "self.pem",
            "-days",
            "30",
            "-subj",
            "/CN=10.0.2.2",
            "-addext",
            "subjectAltName=DNS:10.0.2.2",
        ],
    ]
    .concat())?;
    run(&["x509", "-in", "ca.pem", "-outform", "der", "-out", "ca.der"])?;
    let ca = std::fs::read(dir.join("ca.der")).map_err(|e| e.to_string())?;
    let mut servers = HttpsServers(Vec::new());
    for (port, cert, key) in [
        (aricode_proto::net::TEST_HTTPS_PORT, "leaf.pem", "leaf.key"),
        (
            aricode_proto::net::TEST_HTTPS_UNTRUSTED_PORT,
            "self.pem",
            "self.key",
        ),
    ] {
        let child = Command::new("openssl")
            .args([
                "s_server", "-www", "-tls1_3", "-quiet", "-cert", cert, "-key", key, "-accept",
            ])
            .arg(format!("127.0.0.1:{port}"))
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("openssl s_server: {e}"))?;
        servers.0.push(child);
    }
    std::thread::sleep(Duration::from_millis(300));
    // Fail loudly if a server died or the port is held by a leftover
    // process (a killed earlier run): the scenario would otherwise talk to
    // a stale server with another CA and report a certificate error.
    for (child, port) in servers.0.iter_mut().zip([
        aricode_proto::net::TEST_HTTPS_PORT,
        aricode_proto::net::TEST_HTTPS_UNTRUSTED_PORT,
    ]) {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "HTTPS test server on port {port} exited at start ({status}); is the port in use by a leftover openssl?"
            ));
        }
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_err() {
            return Err(format!(
                "HTTPS test server on port {port} is not accepting connections"
            ));
        }
    }
    Ok((servers, ca))
}
