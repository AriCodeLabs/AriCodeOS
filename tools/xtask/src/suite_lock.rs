// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! One QEMU suite at a time on the whole host (the owner's hardware rule,
//! ADR 0040): every `cargo xtask test` that runs QEMU scenarios takes an
//! exclusive `flock` on one file before it starts, on the host and in every
//! container, the CI container included; a second suite waits for the first
//! and says who holds the lock. Nobody has to stop another suite by hand.
//!
//! The file is `ARICODEOS_SUITE_LOCK`, by default
//! `$HOME/.cache/aricodeos-lock/suite.lock`. Containers mount that host
//! directory at `/run/aricodeos-lock` (README.md, "Docker"; the
//! development image sets `ARICODEOS_SUITE_LOCK` there). Inside a container
//! without that mount the lock would be private to the container and
//! guard nothing, so the runner refuses to start (fail closed).
//! `ARICODEOS_SUITE_LOCK_HELD` says a parent process already holds it (the
//! CI script, which holds it across both of its passes).
//!
//! The lock is the kernel's `flock`, released when the process ends however
//! it ends; the holder's details written into the file are information only.
//! Rust opens files close-on-exec, so QEMU and other children never inherit
//! the descriptor.

use crate::Result;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Where containers mount the host's lock directory.
pub const CONTAINER_DIR: &str = "/run/aricodeos-lock";

/// What to do about the lock, from the environment.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// A parent process holds it for us (`ARICODEOS_SUITE_LOCK_HELD`).
    ParentHolds(String),
    /// Take the lock on this file.
    Lock(PathBuf),
}

/// The environment `plan` looks at, gathered by `Env::current`.
pub struct Env {
    pub held: Option<String>,
    pub path: Option<String>,
    pub home: Option<String>,
    pub in_container: bool,
}

impl Env {
    pub fn current() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            held: var("ARICODEOS_SUITE_LOCK_HELD"),
            path: var("ARICODEOS_SUITE_LOCK"),
            home: var("HOME"),
            in_container: Path::new("/.dockerenv").exists(),
        }
    }
}

/// Decides from `env`; `mounted(dir)` says whether `dir` is a mount point.
/// Inside a container the lock's directory must be mounted (from the
/// host): otherwise the lock would only exclude suites of this container.
pub fn plan(env: &Env, mounted: impl Fn(&Path) -> bool) -> Result<Plan> {
    if let Some(holder) = &env.held {
        return Ok(Plan::ParentHolds(holder.clone()));
    }
    let path = match (&env.path, &env.home) {
        (Some(p), _) => PathBuf::from(p),
        (None, Some(h)) => Path::new(h).join(".cache/aricodeos-lock/suite.lock"),
        (None, None) => {
            return Err("suite lock: neither ARICODEOS_SUITE_LOCK nor HOME is set".into())
        }
    };
    if env.in_container {
        let dir = path.parent().unwrap_or(Path::new("/"));
        if !mounted(dir) {
            return Err(format!(
                "suite lock: inside a container {} must be mounted from the host, or this suite \
                 could run beside another one on the same machine: add -v \"$HOME/.cache/aricodeos-lock\":{CONTAINER_DIR} \
                 and ARICODEOS_SUITE_LOCK={CONTAINER_DIR}/suite.lock (README.md, \"Docker\")",
                dir.display()
            ));
        }
    }
    Ok(Plan::Lock(path))
}

/// `true` if `dir` is a mount point of this process (`/proc/self/mountinfo`,
/// fifth field; spaces escaped as `\040`).
pub fn is_mount_point(dir: &Path) -> bool {
    let want = dir.to_string_lossy().replace(' ', "\\040");
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|info| {
        info.lines()
            .any(|l| l.split(' ').nth(4).is_some_and(|m| m == want))
    })
}

/// The lock, held until dropped (or the process ends).
pub struct SuiteLock {
    _file: Option<File>,
}

/// Takes the suite lock as the environment says, waiting for another suite
/// if one holds it; `suite` describes this one for whoever waits next.
pub fn acquire(suite: &str) -> Result<SuiteLock> {
    match plan(&Env::current(), is_mount_point)? {
        Plan::ParentHolds(holder) => {
            println!("[xtask] suite lock held by the parent process ({holder})");
            Ok(SuiteLock { _file: None })
        }
        Plan::Lock(path) => {
            let file = lock_file(&path, &holder_details(suite), |holder| {
                println!(
                    "[xtask] waiting for another QEMU suite on this host (lock {}; holder: {holder})",
                    path.display()
                );
            })?;
            Ok(SuiteLock { _file: Some(file) })
        }
    }
}

/// Locks `path` (creating it and its directory), calling `waiting` with the
/// current holder's details if it has to wait, then writes `details` into
/// the file. Returns the open file: the lock lasts as long as it.
pub fn lock_file(path: &Path, details: &str, waiting: impl FnOnce(&str)) -> Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut holder = String::new();
            let _ = file.read_to_string(&mut holder);
            let holder = holder.trim().replace('\n', "; ");
            waiting(if holder.is_empty() {
                "unknown"
            } else {
                &holder
            });
            file.lock()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(TryLockError::Error(e)) => return Err(format!("{}: {e}", path.display())),
    }
    let write = |file: &mut File| -> std::io::Result<()> {
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(details.as_bytes())?;
        file.flush()
    };
    write(&mut file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(file)
}

/// Who holds the lock: process, machine (the container id inside one),
/// checkout, its HEAD, start time (UTC) and the suite.
fn holder_details(suite: &str) -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| String::from("?"));
    let container = if Path::new("/.dockerenv").exists() {
        " (container)"
    } else {
        ""
    };
    let root = crate::root();
    let head = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| String::from("?"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!(
        "pid {}\nhost {host}{container}\ncheckout {}\nhead {head}\nstarted {}\nsuite {suite}\n",
        std::process::id(),
        root.display(),
        utc(now)
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch (civil from
/// days, proleptic Gregorian calendar).
fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn env(held: Option<&str>, path: Option<&str>, in_container: bool) -> Env {
        Env {
            held: held.map(String::from),
            path: path.map(String::from),
            home: Some(String::from("/home/u")),
            in_container,
        }
    }

    #[test]
    fn the_lock_file_comes_from_the_environment() {
        let never = |_: &Path| false;
        assert_eq!(
            plan(&env(None, None, false), never),
            Ok(Plan::Lock(PathBuf::from(
                "/home/u/.cache/aricodeos-lock/suite.lock"
            )))
        );
        assert_eq!(
            plan(&env(None, Some("/x/suite.lock"), false), never),
            Ok(Plan::Lock(PathBuf::from("/x/suite.lock")))
        );
    }

    #[test]
    fn a_parent_holding_the_lock_is_not_waited_for() {
        assert_eq!(
            plan(&env(Some("4242"), None, true), |_| false),
            Ok(Plan::ParentHolds(String::from("4242")))
        );
    }

    /// A container without the host's lock directory mounted would lock a
    /// private file and exclude nothing: refused.
    #[test]
    fn a_container_without_the_mounted_directory_fails_closed() {
        let path = "/run/aricodeos-lock/suite.lock";
        let err = plan(&env(None, Some(path), true), |_| false).unwrap_err();
        assert!(err.contains("must be mounted from the host"), "{err}");
        let mounted = |d: &Path| d == Path::new(CONTAINER_DIR);
        assert_eq!(
            plan(&env(None, Some(path), true), mounted),
            Ok(Plan::Lock(PathBuf::from(path)))
        );
        // The default path ($HOME=/tmp in the image) is not mounted either.
        assert!(plan(&env(None, None, true), mounted).is_err());
    }

    #[test]
    fn mount_points_are_read_from_mountinfo() {
        assert!(is_mount_point(Path::new("/")));
        assert!(is_mount_point(Path::new("/proc")));
        assert!(!is_mount_point(Path::new("/proc/self")));
    }

    /// Two suites: the second waits, is told who holds the lock, and gets
    /// it once the first lets go.
    #[test]
    fn a_second_locker_waits_and_sees_the_holder() {
        let dir = crate::test_util::TempDir::new("suite-lock");
        let path = dir.path().join("nested/suite.lock");
        let first = lock_file(&path, "pid 1\nsuite first\n", |_| panic!("free lock")).unwrap();
        let (tx, rx) = mpsc::channel();
        let second = {
            let path = path.clone();
            std::thread::spawn(move || {
                let tx2 = tx.clone();
                let f = lock_file(&path, "pid 2\nsuite second\n", move |holder| {
                    tx2.send(format!("waiting: {holder}")).unwrap()
                })
                .unwrap();
                tx.send(String::from("locked")).unwrap();
                f
            })
        };
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "waiting: pid 1; suite first"
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the second locker must wait while the first holds the lock"
        );
        drop(first);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "locked");
        let held = second.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "pid 2\nsuite second\n"
        );
        drop(held);
    }

    #[test]
    fn utc_formats_civil_time() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_792_713_578), "2026-10-22T23:59:38Z");
    }
}
