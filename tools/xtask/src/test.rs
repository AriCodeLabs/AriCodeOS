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
        "[xtask] nivel {level:?}{}",
        if areas.is_empty() {
            String::new()
        } else {
            format!(", areas {}", areas.join(","))
        }
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
        if !run_scenario(&base, s)? {
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

fn run_scenario(base: &std::path::Path, s: &Scenario) -> Result<bool> {
    let root = image::scenarios_root();
    let workdir = image::scenario_dir(&root, &s.name)?;
    let esp = image::build_scenario_esp(base, &root, &s.name, &s.cmdline, &s.inject)?;
    let input = qemu::Input {
        prompt: &s.prompt,
        lines: &s.stdin,
    };
    let probe = s.host_tcp.as_ref().map(|h| qemu::TcpProbe {
        marker: &h.marker,
        payload: &h.payload,
    });
    let cap = qemu::run_headless(
        &esp,
        &workdir,
        Duration::from_secs(s.timeout_secs),
        &input,
        probe.as_ref(),
    )?;
    let log = workdir.join("serial.log");
    let _ = std::fs::write(&log, &cap.serial);

    let mut problems = Vec::new();
    if let Some(exit) = s.expect.exit {
        if cap.exit_code != Some(exit) {
            problems.push(format!("exit {:?} != {exit}", cap.exit_code));
        }
    }
    for needle in &s.expect.serial_contains {
        if !cap.serial.contains(needle) {
            problems.push(format!("falta '{needle}'"));
        }
    }
    for needle in &s.expect.serial_absent {
        if cap.serial.contains(needle) {
            problems.push(format!("aparece '{needle}'"));
        }
    }
    if cap.timed_out {
        problems.push(format!("timeout {}s", s.timeout_secs));
    }

    let ok = problems.is_empty();
    println!(
        "[{}] {:<36} {:>5.1}s  {}",
        if ok { "PASS" } else { "FAIL" },
        s.name,
        cap.elapsed.as_secs_f32(),
        if ok {
            String::new()
        } else {
            problems.join("; ")
        }
    );
    if !ok {
        for line in cap.serial.lines().filter(|l| !l.trim().is_empty()) {
            println!("      | {line}");
        }
        println!("      log: {}", log.display());
    }
    Ok(ok)
}
