// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `cargo xtask perf [--cpus N] [--update-baseline]` (performance audit
//! 731c398, finding G): runs the `perf.bench` scenario alone on the host
//! (the suite lock, one guest), reads the second `bench` run (the first one
//! pays for the program's pages), writes the figures to
//! `build/perf/result-Ncpu.txt` and compares them with the baseline of the
//! same CPU count kept in `tests/perf/`, recorded on the reference host.
//! A regression past the tolerances below fails; `--update-baseline`
//! records the run as the new baseline.
//!
//! The figures come from QEMU with TCG: they compare builds on one host,
//! not machines. The tolerances are wide on purpose, so TCG's noise does
//! not fail a run while the regressions worth stopping for (a timer-sized
//! wait instead of a wakeup, an order of magnitude) still do.

use crate::{image, qemu, scenario, suite_lock, test, Result};
use std::path::PathBuf;

/// A latency regresses when its p50 exceeds the baseline's times this...
const LATENCY_FACTOR: u64 = 2;
/// ...plus this much, in nanoseconds (TCG jitter on tiny operations).
const LATENCY_SLACK_NS: u64 = 2_000;
/// Overall quiet idle regresses when it drops more than this many permille.
const IDLE_SLACK_PERMILLE: u64 = 30;

/// One figure of a `bench` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    /// `syscall_thread_get_tid`, `quiet_idle_all`...
    pub name: String,
    /// p50, p95, p99 in nanoseconds; or the overall idle permille alone.
    pub values: Vec<u64>,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    LatencyNs,
    IdlePermille,
}

pub fn run(cli: &crate::Cli, release: bool) -> Result {
    let cpus: u32 = match cli.value("--cpus") {
        Some(v) => v
            .parse()
            .ok()
            .filter(|n| (1..=64).contains(n))
            .ok_or_else(|| format!("--cpus {v}: expected a number from 1 to 64"))?,
        None => 1,
    };
    let all = scenario::load_all()?;
    let bench = all
        .iter()
        .find(|s| s.name == "perf.bench")
        .ok_or("the perf.bench scenario is missing")?;
    let _lock = suite_lock::acquire(&format!("cargo xtask perf, --cpus {cpus}"))?;
    let _responder = qemu::Responder::start()?;
    let base = image::build_base_esp(release)?;
    println!("[xtask] perf: perf.bench alone, {cpus} CPU(s)");
    let outcome = test::run_scenario(&base, bench, Some(cpus))?;
    print!("{}", outcome.report);
    if !outcome.ok {
        return Err(String::from("perf.bench failed: no figures to compare"));
    }
    let serial_path = image::scenarios_root()
        .join("perf.bench")
        .join("serial.log");
    let serial = std::fs::read_to_string(&serial_path)
        .map_err(|e| format!("{}: {e}", serial_path.display()))?;
    let figures = parse_second_run(&serial)?;
    let out_dir = crate::build_dir().join("perf");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let result = out_dir.join(format!("result-{cpus}cpu.txt"));
    let text = render(&figures, &environment(cpus));
    std::fs::write(&result, &text).map_err(|e| format!("{}: {e}", result.display()))?;
    println!("[xtask] perf: figures in {}", result.display());
    let baseline_path = baseline_path(cpus);
    if cli.flag("--update-baseline") {
        std::fs::write(&baseline_path, &text)
            .map_err(|e| format!("{}: {e}", baseline_path.display()))?;
        println!(
            "[xtask] perf: baseline recorded in {}",
            baseline_path.display()
        );
        return Ok(());
    }
    let baseline = std::fs::read_to_string(&baseline_path).map_err(|e| {
        format!(
            "{}: {e} (record one with `cargo xtask perf --cpus {cpus} --update-baseline`)",
            baseline_path.display()
        )
    })?;
    let verdict = compare(&parse_figures(&baseline)?, &figures);
    for line in &verdict.lines {
        println!("[xtask] perf: {line}");
    }
    if verdict.regressions > 0 {
        return Err(format!(
            "{} figure(s) regressed past the tolerances against {}",
            verdict.regressions,
            baseline_path.display()
        ));
    }
    println!(
        "[xtask] perf: no regression against {}",
        baseline_path.display()
    );
    Ok(())
}

fn baseline_path(cpus: u32) -> PathBuf {
    crate::root()
        .join("tests/perf")
        .join(format!("baseline-{cpus}cpu.txt"))
}

/// What the figures depend on, written as comments above them.
fn environment(cpus: u32) -> Vec<String> {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .map(|r| r.trim_start_matches([' ', '\t', ':']).to_string())
            })
        })
        .unwrap_or_else(|| String::from("unknown"));
    let qemu_version = std::process::Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_else(|| String::from("unknown"));
    vec![
        String::from(
            "perf.bench, second run (cargo xtask perf; performance audit 731c398, finding G)",
        ),
        format!("QEMU TCG, {cpus} CPU(s), one guest on the host: {qemu_version}"),
        format!("host: {cpu}"),
    ]
}

/// The figures of the last `bench` run in a serial log: the lines after the
/// last `bench: calibration` up to `bench: done`.
pub fn parse_second_run(serial: &str) -> Result<Vec<Figure>> {
    let start = serial
        .rfind("bench: calibration")
        .ok_or("no bench run in the log")?;
    let run = &serial[start..];
    let run = &run[..run
        .find("bench: done")
        .ok_or("the last bench run did not finish")?];
    let mut figures = Vec::new();
    for line in run.lines() {
        let Some(rest) = line.trim().strip_prefix("bench: ") else {
            continue;
        };
        if let Some(f) = parse_latency(rest) {
            figures.push(f);
        } else if let Some(f) = parse_idle(rest) {
            figures.push(f);
        }
    }
    if figures.is_empty() {
        return Err(String::from("the last bench run printed no figures"));
    }
    Ok(figures)
}

/// `syscall thread_get_tid (10000): p50 178 ns p95 182 ns p99 184 ns`.
fn parse_latency(rest: &str) -> Option<Figure> {
    let (name, values) = rest.split_once("):")?;
    let name = name.rsplit_once(" (")?.0;
    let words: Vec<&str> = values.split_whitespace().collect();
    let mut out = Vec::new();
    for label in ["p50", "p95", "p99"] {
        let i = words.iter().position(|w| *w == label)?;
        let value: u64 = words.get(i + 1)?.parse().ok()?;
        let scale = match *words.get(i + 2)? {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            _ => return None,
        };
        out.push(value * scale);
    }
    Some(Figure {
        name: key(name),
        values: out,
        kind: Kind::LatencyNs,
    })
}

/// `quiet idle (2000 ms): cpu 0 993 permille ... all 995 permille`.
fn parse_idle(rest: &str) -> Option<Figure> {
    let (name, values) = rest.split_once("):")?;
    let name = name.rsplit_once(" (")?.0;
    let words: Vec<&str> = values.split_whitespace().collect();
    let i = words.iter().position(|w| *w == "all")?;
    let all: u64 = words.get(i + 1)?.parse().ok()?;
    (words.get(i + 2) == Some(&"permille")).then(|| Figure {
        name: format!("{}_all", key(name)),
        values: vec![all],
        kind: Kind::IdlePermille,
    })
}

fn key(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("_")
}

/// The text of a result or baseline: comments, then `name kind values`.
fn render(figures: &[Figure], comments: &[String]) -> String {
    let mut text = String::new();
    for c in comments {
        text.push_str(&format!("# {c}\n"));
    }
    for f in figures {
        let kind = match f.kind {
            Kind::LatencyNs => "ns",
            Kind::IdlePermille => "permille",
        };
        let values: Vec<String> = f.values.iter().map(u64::to_string).collect();
        text.push_str(&format!("{} {kind} {}\n", f.name, values.join(" ")));
    }
    text
}

/// [`render`]'s text back into figures (comments and blank lines skipped).
pub fn parse_figures(text: &str) -> Result<Vec<Figure>> {
    let mut figures = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(name), Some(kind)) = (words.next(), words.next()) else {
            return Err(format!("perf figures: bad line {line:?}"));
        };
        let kind = match kind {
            "ns" => Kind::LatencyNs,
            "permille" => Kind::IdlePermille,
            other => return Err(format!("perf figures: unknown kind {other:?}")),
        };
        let values = words
            .map(|w| {
                w.parse()
                    .map_err(|_| format!("perf figures: bad number in {line:?}"))
            })
            .collect::<Result<Vec<u64>>>()?;
        let expected = match kind {
            Kind::LatencyNs => 3,
            Kind::IdlePermille => 1,
        };
        if values.len() != expected {
            return Err(format!("perf figures: {line:?} needs {expected} number(s)"));
        }
        figures.push(Figure {
            name: name.to_string(),
            values,
            kind,
        });
    }
    Ok(figures)
}

pub struct Verdict {
    pub lines: Vec<String>,
    pub regressions: usize,
}

/// Each baseline figure against the run: latency by p50 (`LATENCY_FACTOR`
/// times plus `LATENCY_SLACK_NS`), idle by `IDLE_SLACK_PERMILLE`. A figure
/// the run lacks is a regression (a measurement silently gone); a new one
/// is reported, not judged.
pub fn compare(baseline: &[Figure], run: &[Figure]) -> Verdict {
    let mut lines = Vec::new();
    let mut regressions = 0;
    for b in baseline {
        let Some(r) = run.iter().find(|r| r.name == b.name && r.kind == b.kind) else {
            regressions += 1;
            lines.push(format!("{}: missing from this run", b.name));
            continue;
        };
        let (was, now) = (b.values[0], r.values[0]);
        let (ok, unit, limit) = match b.kind {
            Kind::LatencyNs => {
                let limit = was * LATENCY_FACTOR + LATENCY_SLACK_NS;
                (now <= limit, "ns", format!("at most {limit}"))
            }
            Kind::IdlePermille => {
                let limit = was.saturating_sub(IDLE_SLACK_PERMILLE);
                (now >= limit, "permille", format!("at least {limit}"))
            }
        };
        if !ok {
            regressions += 1;
        }
        lines.push(format!(
            "{}: {now} {unit} (baseline {was}, {limit}){}",
            b.name,
            if ok { "" } else { "  REGRESSION" }
        ));
    }
    for r in run {
        if !baseline.iter().any(|b| b.name == r.name) {
            lines.push(format!("{}: new figure, not in the baseline", r.name));
        }
    }
    Verdict { lines, regressions }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
bench: calibration 1797921 TSC cycles per ms
bench: syscall thread_get_tid (10000): p50 900 ns p95 950 ns p99 990 ns
bench: done
ari> run bench
bench: calibration 1797921 TSC cycles per ms
bench: syscall thread_get_tid (10000): p50 178 ns p95 182 ns p99 184 ns
bench: ipc net info (200): p50 16 us p95 17 us p99 20 us
bench: quiet idle (2000 ms): cpu 0 993 permille cpu 1 995 permille all 995 permille
bench: done
run: bench exited with 0
";

    #[test]
    fn the_second_run_is_read_in_nanoseconds() {
        let figures = parse_second_run(LOG).unwrap();
        assert_eq!(
            figures,
            [
                Figure {
                    name: String::from("syscall_thread_get_tid"),
                    values: vec![178, 182, 184],
                    kind: Kind::LatencyNs
                },
                Figure {
                    name: String::from("ipc_net_info"),
                    values: vec![16_000, 17_000, 20_000],
                    kind: Kind::LatencyNs
                },
                Figure {
                    name: String::from("quiet_idle_all"),
                    values: vec![995],
                    kind: Kind::IdlePermille
                },
            ]
        );
        // What is written is what is read back.
        let text = render(&figures, &[String::from("a comment")]);
        assert_eq!(parse_figures(&text).unwrap(), figures);
        assert!(parse_second_run("bench: calibration 1\nbench: syscall").is_err());
    }

    #[test]
    fn regressions_past_the_tolerances_fail_and_noise_does_not() {
        let base = parse_second_run(LOG).unwrap();
        // TCG noise: 1.8 times slower, 20 permille less idle.
        let noisy = parse_figures(
            "syscall_thread_get_tid ns 320 400 500\nipc_net_info ns 29000 30000 31000\nquiet_idle_all permille 975\n",
        )
        .unwrap();
        assert_eq!(compare(&base, &noisy).regressions, 0);
        // The regression of finding B: a 10 ms wait instead of a wakeup.
        let timer = parse_figures(
            "syscall_thread_get_tid ns 180 182 184\nipc_net_info ns 10001000 10002000 10003000\nquiet_idle_all permille 900\n",
        )
        .unwrap();
        let v = compare(&base, &timer);
        assert_eq!(v.regressions, 2, "{:?}", v.lines);
        // A figure that disappears is a regression too.
        let gone = parse_figures("syscall_thread_get_tid ns 180 182 184\n").unwrap();
        assert_eq!(compare(&base, &gone).regressions, 2);
    }
}
