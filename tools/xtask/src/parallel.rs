// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Concurrent scenario runs with ordered results (ADR 0040), and how many
//! of them a machine can take.

use crate::Result;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Runs `work(i)` for every `i` in `order` on up to `jobs` threads, taking
/// them in that order, and hands each result to `done` on the calling
/// thread in index order (0, 1, 2, ...) as soon as it and every one before
/// it are in, so the output reads the same whatever finished first. After
/// an error no new work starts; the runs already going finish, `done` gets
/// every result before the first error in index order, and that error is
/// returned (with `jobs = 1` this is exactly the sequential loop). An index
/// for which `alone` holds runs with nothing else at the same time: it
/// waits for the runs going to finish and holds the others back.
pub fn run_ordered<T: Send>(
    order: &[usize],
    jobs: usize,
    alone: impl Fn(usize) -> bool + Sync,
    work: impl Fn(usize) -> Result<T> + Sync,
    mut done: impl FnMut(usize, T),
) -> Result {
    let n = order.len();
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    // Shared by ordinary runs, exclusive for the ones that run alone.
    let gate = std::sync::RwLock::new(());
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<T>)>();
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, n.max(1)) {
            let tx = tx.clone();
            let (next, stop, work, alone, gate) = (&next, &stop, &work, &alone, &gate);
            s.spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    let Some(&i) = order.get(k) else { break };
                    let r = if alone(i) {
                        let _only = gate.write().unwrap_or_else(|e| e.into_inner());
                        work(i)
                    } else {
                        let _shared = gate.read().unwrap_or_else(|e| e.into_inner());
                        work(i)
                    };
                    if r.is_err() {
                        stop.store(true, Ordering::SeqCst);
                    }
                    if tx.send((i, r)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut pending: Vec<Option<Result<T>>> = (0..n).map(|_| None).collect();
        let mut first = 0;
        for (i, r) in rx {
            pending[i] = Some(r);
            while let Some(r) = pending.get_mut(first).and_then(Option::take) {
                match r {
                    Ok(v) => done(first, v),
                    Err(e) => {
                        stop.store(true, Ordering::SeqCst);
                        return Err(e);
                    }
                }
                first += 1;
            }
        }
        // Every worker has exited: an index never reported was never run
        // because an error stopped the dispatch; that error comes first.
        match pending.into_iter().flatten().find_map(|r| r.err()) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    })
}

/// Memory one QEMU run takes: the guest's 512 MiB, QEMU and its TCG
/// translation cache, plus the host-side servers and probes.
pub const MEMORY_PER_JOB: u64 = 1 << 30;

/// Most runs at once by default (the owner's limits for the reference
/// host, 2026-10-23): ten single-CPU guests, six multi-CPU ones (each of
/// those keeps up to N host threads busy under TCG).
pub const MAX_DEFAULT_JOBS: usize = 10;
pub const MAX_DEFAULT_JOBS_SMP: usize = 6;

/// Default concurrency: one run per CPU this process may use (a run is
/// one vCPU thread plus QEMU's I/O thread and the host-side servers, which
/// mostly wait), two thirds of that with multi-CPU guests (`--cpus N`,
/// N > 1), within [`MAX_DEFAULT_JOBS`] and [`MAX_DEFAULT_JOBS_SMP`].
pub fn default_jobs(host_cpus: usize, guest_cpus: u32) -> usize {
    let jobs = if guest_cpus > 1 {
        (host_cpus * 2 / 3).min(MAX_DEFAULT_JOBS_SMP)
    } else {
        host_cpus.min(MAX_DEFAULT_JOBS)
    };
    jobs.max(1)
}

/// `jobs` lowered to what `available` bytes of memory can hold (at least
/// one run), or unchanged when the available memory is unknown.
pub fn cap_by_memory(jobs: usize, available: Option<u64>) -> usize {
    match available {
        Some(bytes) => jobs.min((bytes / MEMORY_PER_JOB).max(1) as usize),
        None => jobs,
    }
}

/// Memory available to this process: the smaller of the system's
/// `MemAvailable` and what the cgroup (a `docker run --memory` limit)
/// still allows.
pub fn available_memory() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| mem_available(&t));
    let read = |name: &str| std::fs::read_to_string(format!("/sys/fs/cgroup/{name}")).ok();
    let cgroup = match (read("memory.max"), read("memory.current")) {
        (Some(max), Some(current)) => {
            cgroup_room(&max, &current, &read("memory.stat").unwrap_or_default())
        }
        _ => None,
    };
    match (meminfo, cgroup) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `MemAvailable` of `/proc/meminfo`, in bytes.
fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Bytes a cgroup v2 still allows: `memory.max` minus `memory.current`,
/// whose page cache the kernel reclaims under the limit (`inactive_file`
/// of `memory.stat` counts as free, as `MemAvailable` does); `None`
/// without a limit (`max`).
fn cgroup_room(max: &str, current: &str, stat: &str) -> Option<u64> {
    let max: u64 = max.trim().parse().ok()?;
    let current: u64 = current.trim().parse().ok()?;
    let reclaimable: u64 = stat
        .lines()
        .find_map(|l| l.strip_prefix("inactive_file "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    Some(max.saturating_sub(current.saturating_sub(reclaimable)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn results_come_in_index_order_whatever_finishes_first() {
        let order = [3, 2, 1, 0, 4];
        let mut seen = Vec::new();
        run_ordered(
            &order,
            5,
            |_| false,
            |i| {
                // Index 0 finishes last.
                std::thread::sleep(Duration::from_millis(40 * (4 - i.min(4)) as u64));
                Ok(i * 10)
            },
            |i, v| seen.push((i, v)),
        )
        .unwrap();
        assert_eq!(seen, [(0, 0), (1, 10), (2, 20), (3, 30), (4, 40)]);
    }

    #[test]
    fn never_more_than_jobs_at_once_and_one_job_follows_the_order() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let order: Vec<usize> = (0..12).collect();
        run_ordered(
            &order,
            3,
            |_| false,
            |_| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(15));
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
            |_, ()| {},
        )
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), 3);

        let started = Mutex::new(Vec::new());
        run_ordered(
            &[2, 0, 1],
            1,
            |_| false,
            |i| {
                started.lock().unwrap().push(i);
                Ok(())
            },
            |_, ()| {},
        )
        .unwrap();
        assert_eq!(*started.lock().unwrap(), [2, 0, 1]);
    }

    /// A run marked alone never overlaps another one, whether it starts
    /// first, in the middle or last.
    #[test]
    fn alone_runs_never_overlap_another() {
        for alone_at in [0, 4, 9] {
            let running = AtomicUsize::new(0);
            let overlapped = AtomicBool::new(false);
            let order: Vec<usize> = (0..10).collect();
            run_ordered(
                &order,
                4,
                |i| i == alone_at,
                |i| {
                    let others = running.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    if i == alone_at && (others > 0 || running.load(Ordering::SeqCst) > 1) {
                        overlapped.store(true, Ordering::SeqCst);
                    }
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                },
                |_, ()| {},
            )
            .unwrap();
            assert!(!overlapped.load(Ordering::SeqCst), "alone at {alone_at}");
        }
    }

    #[test]
    fn an_error_stops_new_work_and_keeps_earlier_results() {
        let ran = AtomicUsize::new(0);
        let mut seen = Vec::new();
        let order: Vec<usize> = (0..10).collect();
        let err = run_ordered(
            &order,
            1,
            |_| false,
            |i| {
                ran.fetch_add(1, Ordering::SeqCst);
                if i == 3 {
                    Err(format!("broke at {i}"))
                } else {
                    Ok(i)
                }
            },
            |i, _| seen.push(i),
        )
        .unwrap_err();
        assert_eq!(err, "broke at 3");
        assert_eq!(seen, [0, 1, 2]);
        assert_eq!(ran.load(Ordering::SeqCst), 4, "nothing after the error");
    }

    #[test]
    fn concurrency_follows_cpus_and_memory() {
        assert_eq!(default_jobs(16, 1), 10);
        assert_eq!(default_jobs(10, 1), 10);
        assert_eq!(default_jobs(8, 1), 8);
        assert_eq!(default_jobs(16, 4), 6);
        assert_eq!(default_jobs(10, 4), 6);
        assert_eq!(default_jobs(8, 4), 5);
        assert_eq!(default_jobs(1, 1), 1);
        assert_eq!(default_jobs(1, 4), 1);
        assert_eq!(cap_by_memory(8, Some(5 * MEMORY_PER_JOB + 7)), 5);
        assert_eq!(cap_by_memory(8, Some(MEMORY_PER_JOB / 2)), 1);
        assert_eq!(cap_by_memory(8, None), 8);
    }

    #[test]
    fn reads_meminfo_and_cgroup_limits() {
        let meminfo = "MemTotal:       32000000 kB\nMemFree:  100 kB\nMemAvailable:   2048 kB\n";
        assert_eq!(mem_available(meminfo), Some(2048 * 1024));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
        let stat = "anon 100\ninactive_file 200\nactive_file 50\n";
        assert_eq!(cgroup_room("max\n", "100\n", stat), None);
        assert_eq!(cgroup_room("1000\n", "300\n", ""), Some(700));
        assert_eq!(cgroup_room("1000\n", "300\n", stat), Some(900));
        assert_eq!(cgroup_room("100\n", "300\n", ""), Some(0));
    }
}
