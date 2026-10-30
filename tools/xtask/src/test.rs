// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Test level orchestration: unit (host) -> smoke/full (QEMU), the QEMU
//! scenarios run `--jobs N` at a time (ADR 0040).

use crate::scenario::{self, Level, Scenario};
use crate::{build, image, parallel, ports, qemu, suite_lock, Cli, Result};
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

/// The host key `[host_ssh] old_host_key` installs: public, tests only.
const OLD_HOST_KEY_SEED: [u8; 32] = [0x41; 32];

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

    let cpus_override = cli.cpus()?;
    let jobs_requested = match cli.value("--jobs") {
        Some(v) => Some(
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=64).contains(n))
                .ok_or_else(|| format!("--jobs {v}: expected a number from 1 to 64"))?,
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

    // One QEMU suite at a time on the whole host, containers included
    // (`suite_lock`): waits here for another suite to finish. Then the
    // test hosts the guest's test programs have compiled in: one set for
    // the whole suite, taken before anything is built so a second suite in
    // this network namespace stops at once (ADR 0040).
    let (_suite_lock, _responder) = match level {
        Level::Unit => (None, None),
        _ => {
            let suite = format!(
                "cargo xtask test, level {level:?}{}{}",
                if areas.is_empty() {
                    String::new()
                } else {
                    format!(", areas {}", areas.join(","))
                },
                cpus_override.map_or(String::new(), |n| format!(", --cpus {n}"))
            );
            let lock = suite_lock::acquire(&suite)?;
            (Some(lock), Some(qemu::Responder::start()?))
        }
    };

    // Unit level: always, even when filtering by area (a feature may want
    // only its QEMU scenarios; unit tests are still cheap and run anyway).
    println!("[xtask] unit tests (host)");
    build::unit_tests()?;
    if level == Level::Unit {
        return Ok(());
    }

    let base = image::build_base_esp(release)?;
    prebuild_packages(&selected)?;
    let jobs = jobs(jobs_requested, cpus_override, selected.len());
    let order = schedule(&selected, jobs);
    println!(
        "[xtask] {} QEMU scenarios, {jobs} at a time",
        selected.len()
    );
    let start = Instant::now();
    let mut failed = Vec::new();
    let mut busy = Duration::ZERO;
    parallel::run_ordered(
        &order,
        jobs,
        |i| selected[i].serial,
        |i| run_scenario(&base, selected[i], cpus_override),
        |i, outcome| {
            print!("{}", outcome.report);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            busy += outcome.elapsed;
            if !outcome.ok {
                failed.push(selected[i].name.clone());
            }
        },
    )?;
    println!(
        "[xtask] QEMU wall clock {}, {} of scenario time",
        minutes(start.elapsed()),
        minutes(busy)
    );
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

/// `1m05s`.
fn minutes(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

/// How many scenarios run at once: `--jobs N`, or the default for this
/// machine's CPUs ([`parallel::default_jobs`]), never more than the
/// available memory holds nor than there are scenarios.
fn jobs(requested: Option<usize>, cpus_override: Option<u32>, scenarios: usize) -> usize {
    let host_cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let wanted =
        requested.unwrap_or_else(|| parallel::default_jobs(host_cpus, cpus_override.unwrap_or(1)));
    let available = parallel::available_memory();
    let jobs = parallel::cap_by_memory(wanted, available);
    if jobs < wanted {
        println!(
            "[xtask] {jobs} jobs instead of {wanted}: {} MiB of memory available, {} MiB per QEMU run",
            available.unwrap_or(0) >> 20,
            parallel::MEMORY_PER_JOB >> 20
        );
    }
    jobs.min(scenarios.max(1))
}

/// Order the scenarios start in. One at a time: as listed. Several: the
/// ones that run alone (`serial`) first, while nothing else is going, then
/// the longest first (the time of their previous run, kept in their
/// directory, or their timeout when they have none), so a long scenario
/// never starts last and holds the suite alone. Results are printed in
/// listed order either way.
fn schedule(selected: &[&Scenario], jobs: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..selected.len()).collect();
    if jobs > 1 {
        let root = image::scenarios_root();
        let expected: Vec<f32> = selected
            .iter()
            .map(|s| {
                std::fs::read_to_string(root.join(&s.name).join(ELAPSED_FILE))
                    .ok()
                    .and_then(|t| t.trim().parse().ok())
                    .unwrap_or(s.timeout_secs as f32)
            })
            .collect();
        order.sort_by(|&a, &b| {
            (selected[b].serial, expected[b])
                .partial_cmp(&(selected[a].serial, expected[a]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    order
}

/// The scenario's last run time (seconds), in its directory.
const ELAPSED_FILE: &str = "elapsed";

/// Builds every external package the selected scenarios install, stage or
/// publish before they run concurrently: `build_external` keeps them, so
/// no scenario starts cargo and the build output stays in one place.
fn prebuild_packages(selected: &[&Scenario]) -> Result {
    for s in selected {
        let mut wanted: Vec<(String, u32, bool)> =
            s.install.iter().map(|d| (d.clone(), 1, false)).collect();
        for spec in &s.packages {
            let mut parts = spec.split(':');
            if let (Some(dir), Some(Ok(version))) = (parts.next(), parts.next().map(str::parse)) {
                wanted.push((dir.to_string(), version, parts.next() == Some("corrupt")));
            }
        }
        for spec in &s.staged {
            let base = spec.strip_suffix(":wrong-hash").unwrap_or(spec);
            let (dir, version) = crate::app_spec(base).map_err(|e| format!("{}: {e}", s.name))?;
            wanted.push((dir, version, false));
        }
        for spec in s.host_update.iter().flat_map(|u| &u.apps) {
            let (dir, version) = crate::app_spec(spec).map_err(|e| format!("{}: {e}", s.name))?;
            wanted.push((dir, version, false));
        }
        for (dir, version, corrupt) in wanted {
            build::build_external(Path::new(&dir), version, build::PACKAGES_RELEASE, corrupt)?;
        }
    }
    Ok(())
}

/// What one scenario run leaves for the summary: the lines to print (in
/// scenario order, whatever order the runs finished in), its QEMU time and
/// whether it passed.
pub struct Outcome {
    pub report: String,
    elapsed: Duration,
    pub ok: bool,
}

/// One boot to run: the scenario's only boot, or each one in `[[boots]]`,
/// with the run's ports filled in.
struct BootRun {
    cmdline: String,
    stdin: Vec<String>,
    kill_on: Option<String>,
    expect: scenario::Expect,
}

pub fn run_scenario(
    base: &std::path::Path,
    s: &Scenario,
    cpus_override: Option<u32>,
) -> Result<Outcome> {
    let mut report = String::new();
    let root = image::scenarios_root();
    let workdir = image::scenario_dir(&root, &s.name)?;
    // This run's host ports (ADR 0040): the two `hostfwd` ports and, with
    // `host_https`, the two HTTPS servers'.
    let lease = ports::PortLease::take(if s.host_https { 4 } else { 2 })?;
    let forwards = qemu::Forwards {
        tcp: lease.ports()[0],
        ssh: lease.ports()[1],
    };
    let mut values: Vec<(&str, u16)> = Vec::new();
    let multi = !s.boots.is_empty();
    let disk_variant = image::DiskVariant::parse(&s.disk)
        .ok_or_else(|| format!("{}: unknown disk '{}'", s.name, s.disk))?;
    let mut installed: Vec<(String, Vec<u8>)> = Vec::new();
    for dir in &s.install {
        installed.push(
            build::build_external(std::path::Path::new(dir), 1, build::PACKAGES_RELEASE, false)?
                .installed(),
        );
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
        let app = build::build_external(
            std::path::Path::new(dir),
            version,
            build::PACKAGES_RELEASE,
            corrupt,
        )?;
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
            let app = build::build_external(
                std::path::Path::new(&dir),
                version,
                build::PACKAGES_RELEASE,
                false,
            )?;
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
            if s.host_ssh.as_ref().is_some_and(|h| h.old_host_key) {
                let mut hex = [0u8; 64];
                let text = format!("{}\n", aricode_crypto::hex(&OLD_HOST_KEY_SEED, &mut hex));
                installed.push((String::from("ssh_host_ed25519_key"), text.into_bytes()));
            }
            Some(key)
        }
        false => None,
    };
    // The scenario's own update source: published into its directory and
    // served by its own server, so concurrent update scenarios never share
    // a channel (ADR 0040).
    let _update_server = match &s.host_update {
        Some(u) => {
            let channel = image::Publication {
                system: u.system,
                apps: u
                    .apps
                    .iter()
                    .map(|spec| crate::app_spec(spec).map_err(|e| format!("{}: {e}", s.name)))
                    .collect::<Result<_>>()?,
                legacy_manifest: false,
                tamper_index: u.tamper_index,
                tamper_image: u.tamper_image,
                large_image: u.large_image,
                ..image::Publication::default()
            };
            let dir = workdir.join("update");
            writeln!(report, "{}", image::publish_channel(&dir, &channel)?).expect("String");
            if u.withdraw_image {
                image::withdraw_system_image(&dir)
                    .map_err(|e| format!("{}: host_update.withdraw_image: {e}", s.name))?;
            }
            // The development source beside it (ADR 0045), served at /dev/
            // by the same server.
            if let Some(build) = u.dev_build {
                let dev = image::Publication {
                    channel: aricode_ab::Channel::Dev,
                    system: Some(build),
                    commit: Some(u.dev_commit.clone()),
                    sign_as: u.dev_wrong_key.then_some(aricode_ab::Channel::Stable),
                    stable_image: u.dev_stable_image,
                    ..image::Publication::default()
                };
                let dev_dir = workdir.join(qemu::DEV_SOURCE_DIR);
                image::forget_test_dev_source(&dev_dir)?;
                writeln!(report, "{}", image::publish_channel(&dev_dir, &dev)?).expect("String");
            }
            let server = qemu::UpdateServer::start(&dir)?;
            values.push((scenario::UPDATE_PORT, server.port()));
            let url = format!("http://10.0.2.2:{}/update/\n", server.port());
            let dev_line = match (u.dev_build, u.dev_source) {
                (Some(_), true) => format!("dev http://10.0.2.2:{}/dev/\n", server.port()),
                (None, true) => {
                    return Err(format!(
                        "{}: host_update.dev_source needs dev_build",
                        s.name
                    ))
                }
                _ => String::new(),
            };
            if u.stable_floor_damaged && u.stable_floor.is_none() {
                return Err(format!(
                    "{}: host_update.stable_floor_damaged needs stable_floor",
                    s.name
                ));
            }
            if let Some(floor) = u.stable_floor {
                let state = aricode_update_index::main_path(aricode_update_index::STATE_FILE);
                let mut body = aricode_update_index::lists::encode_floor(floor);
                if u.stable_floor_damaged {
                    body.insert(body.len() - 1, 'x');
                }
                if installed.iter().any(|(name, _)| *name == state) {
                    return Err(format!(
                        "{}: host_update.stable_floor and staged packages both write updater/state",
                        s.name
                    ));
                }
                installed.push((
                    state,
                    aricode_update_index::lists::with_section(
                        "",
                        aricode_update_index::lists::FLOOR,
                        &body,
                    )
                    .into_bytes(),
                ));
            }
            if u.stale_index {
                let ahead = image::stored_copy_ahead(&dir, url.trim())?;
                installed.push((
                    aricode_update_index::main_path(aricode_update_index::INDEXES_FILE),
                    aricode_update_index::stored::encode_all(&[ahead]),
                ));
            }
            match u.source.as_str() {
                "sources" => installed.push((
                    aricode_update_index::main_path(aricode_update_index::SOURCES_FILE),
                    format!("{url}{dev_line}").into_bytes(),
                )),
                "none" if !dev_line.is_empty() => installed.push((
                    aricode_update_index::main_path(aricode_update_index::SOURCES_FILE),
                    dev_line.into_bytes(),
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
            Some(server)
        }
        None => None,
    };
    let _https = if s.host_https {
        let ports = (lease.ports()[2], lease.ports()[3]);
        let (servers, ca) = https_servers(&workdir.join("tls"), ports)?;
        installed.push((String::from("trusted_ca.der"), ca));
        values.push((scenario::HTTPS_PORT, ports.0));
        values.push((scenario::HTTPS_UNTRUSTED_PORT, ports.1));
        Some(servers)
    } else {
        None
    };
    let fill_all =
        |v: &[String]| -> Vec<String> { v.iter().map(|t| scenario::fill(t, &values)).collect() };
    let runs: Vec<BootRun> = if s.boots.is_empty() {
        vec![BootRun {
            cmdline: s.boot_cmdline(&s.cmdline),
            stdin: fill_all(&s.stdin),
            kill_on: None,
            expect: s.expect.filled(&values),
        }]
    } else {
        s.boots
            .iter()
            .map(|b| BootRun {
                cmdline: s.boot_cmdline(&b.cmdline),
                stdin: fill_all(&b.stdin),
                kill_on: b.kill_on.as_ref().map(|k| scenario::fill(k, &values)),
                expect: b.expect.filled(&values),
            })
            .collect()
    };
    let mut problems = Vec::new();
    let mut qemu_lines = Vec::new();
    let mut elapsed = Duration::ZERO;
    let mut logs: Vec<(std::path::PathBuf, String)> = Vec::new();
    // The kernel seed the shared disk held before each boot (ADR 0043): a
    // fresh disk holds none.
    let mut seed_before = None;
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
            lines: &run.stdin,
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
            echo_prompt: h.echo_prompt.as_deref(),
        });
        let hotplug = s.host_hotplug.as_ref().map(|h| qemu::Hotplug {
            marker: &h.marker,
            delay: Duration::from_millis(h.delay_ms),
            driver: &h.driver,
            bus: &h.bus,
            id: &h.id,
        });
        let ssh_probe = match (&s.host_ssh, &ssh_key) {
            (Some(h), Some(key)) => Some(qemu::SshProbe {
                marker: &h.marker,
                commands: &h.commands,
                key: key.clone(),
                idle_client: h.idle_client,
                retry_connect_secs: h.retry_connect_secs,
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
            hotplug,
            disk_variant,
            disk_readonly: s.disk_readonly,
            keep_disk: i > 0,
            kill_on: run.kill_on.as_deref(),
            installed: &installed,
            cpus: s.cpus(cpus_override),
            cpu_rdrand: s.cpu_rdrand,
            usb: qemu::UsbOptions {
                hid: s.usb_hid,
                hid_msi: s.usb_hid_msi,
                hid_msi_and_msix: s.usb_hid_msi_and_msix,
                hid_only_input: s.usb_hid_only_input,
                legacy_controller: s.usb_legacy_controller,
                storage: s.usb_storage,
                ehci_hid: s.usb_ehci_hid,
                ehci_storage: s.usb_ehci_storage,
                ehci_companion_kbd: s.usb_ehci_companion_kbd,
            },
            nic: !s.no_nic,
            flr_device: s.flr_device,
            forwards,
        };
        let suffix = if multi {
            format!("-{}", i + 1)
        } else {
            String::new()
        };
        let stderr_log = workdir.join(format!("qemu-stderr{suffix}.log"));
        let cap = qemu::run_headless(
            &esp,
            &workdir,
            &stderr_log,
            Duration::from_secs(s.timeout_secs),
            &options,
        )?;
        // QEMU's own messages (a refused `hostfwd` port, a warning) belong
        // to this scenario's block of the output, after its result line.
        let stderr = std::fs::read_to_string(&stderr_log).unwrap_or_default();
        for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
            qemu_lines.push(format!("      qemu: {line}"));
        }
        let log = workdir.join(format!("serial{suffix}.log"));
        let _ = std::fs::write(&log, &cap.serial);
        elapsed += cap.elapsed;
        let prefix = if multi {
            format!("boot {}: ", i + 1)
        } else {
            String::new()
        };
        let expect = &run.expect;
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
        if s.host_ssh.as_ref().is_some_and(|h| h.old_host_key) {
            let public = aricode_crypto::ed25519::public_key(&OLD_HOST_KEY_SEED);
            let needle = format!("host key {}", aricode_ssh::fingerprint(&public));
            if !cap.serial.contains(&needle) {
                problems.push(format!("{prefix}missing '{needle}' (the moved host key)"));
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
        if expect.seed_store.is_some() || multi {
            let seed_after = std::fs::read(workdir.join("disk.img"))
                .ok()
                .and_then(|img| image::seed_record(&img));
            if let Some(why) = expect
                .seed_store
                .and_then(|want| want.check(seed_before.as_ref(), seed_after.as_ref()))
            {
                problems.push(format!("{prefix}{why}"));
            }
            seed_before = seed_after;
        }
        logs.push((log, cap.serial));
        if !problems.is_empty() {
            // Later boots depend on this one: they would tell nothing new.
            break;
        }
    }
    let _ = std::fs::write(
        workdir.join(ELAPSED_FILE),
        format!("{:.1}\n", elapsed.as_secs_f32()),
    );

    let ok = problems.is_empty();
    writeln!(
        report,
        "[{}] {:<36} {:>5.1}s  {}",
        if ok { "PASS" } else { "FAIL" },
        s.name,
        elapsed.as_secs_f32(),
        if ok {
            String::new()
        } else {
            problems.join("; ")
        }
    )
    .expect("String");
    if !ok {
        if let Some((log, serial)) = logs.last() {
            for line in serial.lines().filter(|l| !l.trim().is_empty()) {
                writeln!(report, "      | {line}").expect("String");
            }
            writeln!(report, "      log: {}", log.display()).expect("String");
        }
    }
    for line in qemu_lines {
        writeln!(report, "{line}").expect("String");
    }
    Ok(Outcome {
        report,
        elapsed,
        ok,
    })
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
/// self-signed leaf, and starts one `openssl s_server -www` for each leaf,
/// on `ports` (trusted, untrusted). Returns the servers and the CA
/// certificate (DER).
fn https_servers(dir: &std::path::Path, ports: (u16, u16)) -> Result<(HttpsServers, Vec<u8>)> {
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
        (ports.0, "leaf.pem", "leaf.key"),
        (ports.1, "self.pem", "self.key"),
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
    // Fail loudly if a server died (its port was taken by another process
    // between the lease and its bind): the scenario would otherwise talk
    // to whatever holds the port and report a certificate error.
    for (child, port) in servers.0.iter_mut().zip([ports.0, ports.1]) {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "HTTPS test server on port {port} exited at start ({status}); is the port in use?"
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
