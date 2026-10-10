//! Orquestación de niveles de test: unit (host) -> smoke/full (QEMU).

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
                .ok_or_else(|| format!("--cpus {v}: se esperaba un numero de 1 a 64"))?,
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
        "[xtask] nivel {level:?}{}{}",
        if areas.is_empty() {
            String::new()
        } else {
            format!(", areas {}", areas.join(","))
        },
        cpus_override.map_or(String::new(), |n| format!(
            ", {n} CPUs en todos los escenarios"
        ))
    );

    // Nivel unit: siempre, salvo que se filtre por área (una feature puede
    // querer solo sus escenarios QEMU; los unit siguen siendo baratos y se
    // ejecutan igual).
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
        "[xtask] {} escenarios QEMU, {} fallidos",
        selected.len(),
        failed.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("escenarios fallidos: {}", failed.join(", ")))
    }
}

/// Un arranque a ejecutar: el único del escenario o cada uno de `[[boots]]`.
struct BootRun<'a> {
    cmdline: &'a str,
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
            cmdline: &s.cmdline,
            stdin: &s.stdin,
            kill_on: None,
            expect: &s.expect,
        }]
    } else {
        s.boots
            .iter()
            .map(|b| BootRun {
                cmdline: &b.cmdline,
                stdin: &b.stdin,
                kill_on: b.kill_on.as_deref(),
                expect: &b.expect,
            })
            .collect()
    };
    let multi = !s.boots.is_empty();
    let disk_variant = image::DiskVariant::parse(&s.disk)
        .ok_or_else(|| format!("{}: disk '{}' desconocido", s.name, s.disk))?;
    let mut installed: Vec<(String, Vec<u8>)> = Vec::new();
    for dir in &s.install {
        installed
            .push(build::build_external(std::path::Path::new(dir), 1, release, false)?.installed());
    }
    for spec in &s.packages {
        let mut parts = spec.split(':');
        let (Some(dir), Some(version)) = (parts.next(), parts.next()) else {
            return Err(format!(
                "{}: paquete '{spec}': DIR:VERSION[:corrupt]",
                s.name
            ));
        };
        let version: u32 = version
            .parse()
            .map_err(|_| format!("{}: paquete '{spec}': version invalida", s.name))?;
        let corrupt = match parts.next() {
            None => false,
            Some("corrupt") => true,
            Some(other) => {
                return Err(format!(
                    "{}: paquete '{spec}': '{other}' desconocido",
                    s.name
                ))
            }
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
    let mut problems = Vec::new();
    let mut elapsed = Duration::ZERO;
    let mut logs: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        // Cada arranque tiene su ESP (su cmdline); el disco es el mismo.
        let esp_name = if multi {
            format!("{}.boot{}", s.name, i + 1)
        } else {
            s.name.clone()
        };
        let esp = image::build_scenario_esp(base, &root, &esp_name, run.cmdline, &s.inject)?;
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
        });
        let options = qemu::Headless {
            input,
            probe,
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
                legacy_controller: s.usb_legacy_controller,
                storage: s.usb_storage,
            },
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
            format!("arranque {}: ", i + 1)
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
                    "QEMU no llego a matarse en kill_on"
                } else {
                    "QEMU matado sin esperarlo"
                }
            ));
        }
        for needle in &expect.serial_contains {
            if !cap.serial.contains(needle) {
                problems.push(format!("{prefix}falta '{needle}'"));
            }
        }
        for needle in &expect.serial_absent {
            if cap.serial.contains(needle) {
                problems.push(format!("{prefix}aparece '{needle}'"));
            }
        }
        if cap.timed_out {
            problems.push(format!("{prefix}timeout {}s", s.timeout_secs));
        }
        logs.push((log, cap.serial));
        if !problems.is_empty() {
            // Los arranques siguientes dependen de este: no dicen nada nuevo.
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
