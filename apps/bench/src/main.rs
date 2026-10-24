// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `bench` (stage 5): reproducible measurements from a regular broker app
//! (`docs/performance.md`): latency of a syscall, of a full IPC call to fsd,
//! of file operations and of a metadata call to the network service, with
//! p50, p95 and p99 percentiles over the TSC calibrated against the system
//! clock, and the idle time of every CPU over a quiet interval. Numbers are
//! only comparable between runs with the same build and the same QEMU mode.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::net::NetClient;
use aricode_rt::sys::{self, abi, kind, permissions, time, Handle};

const SYSCALLS: usize = 10_000;
const IPC_CALLS: usize = 1_000;
const FILE_OPS: usize = 100;
const NET_CALLS: usize = 200;
const CHUNK: usize = 4096;
/// Quiet interval over which the idle time of every CPU is measured: the
/// system's own wakeups (timers, services waiting for deadlines) are what
/// keeps it below 1000 permille.
const QUIET_MS: u64 = 2_000;
/// CPUs whose idle counters fit in the sample buffer.
const MAX_CPUS: usize = 16;

fn rdtsc() -> u64 {
    // SAFETY: rdtsc only reads the time stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(fmt_u64(v, &mut b));
}

/// Percentile `p` (0..=100) of already sorted samples.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len() - 1) * p / 100;
    sorted[idx]
}

/// Prints `bench: NAME (N): p50 X UNIT p95 Y UNIT p99 Z UNIT`.
fn report(name: &[u8], samples: &mut [u64], cycles_per_unit: u64, unit: &[u8]) {
    samples.sort_unstable();
    sys::print(b"bench: ");
    sys::print(name);
    sys::print(b" (");
    num(samples.len() as u64);
    sys::print(b"):");
    for (label, p) in [(b" p50 " as &[u8], 50), (b" p95 ", 95), (b" p99 ", 99)] {
        sys::print(label);
        num(percentile(samples, p) / cycles_per_unit.max(1));
        sys::print(b" ");
        sys::print(unit);
    }
    sys::print(b"\n");
}

/// TSC cycles per millisecond, measured against the system clock.
fn calibrate(system: Handle) -> Option<u64> {
    let start = time::uptime_us(system).ok()?;
    let t0 = rdtsc();
    time::sleep_ms(200);
    let t1 = rdtsc();
    let end = time::uptime_us(system).ok()?;
    let us = end.saturating_sub(start);
    (us > 0).then(|| (t1 - t0) * 1000 / us)
}

/// `sysinfo::IDLE` words: elapsed cycles, online CPUs, idle cycles per CPU.
fn idle_sample(system: Handle) -> Option<[u64; 2 + MAX_CPUS]> {
    let mut raw = [0u8; 8 * (2 + MAX_CPUS)];
    let n = sys::system_info(system, abi::sysinfo::IDLE, &mut raw).ok()?;
    let mut words = [0u64; 2 + MAX_CPUS];
    for (w, c) in words.iter_mut().zip(raw[..n].as_chunks::<8>().0) {
        *w = u64::from_le_bytes(*c);
    }
    Some(words)
}

/// Prints `bench: quiet idle (MS ms): cpu 0 X permille ... all Y permille`:
/// the share of a quiet interval each CPU spent in its idle thread, and of
/// all of them together (incident 0020 made the counters exact).
fn quiet_idle(system: Handle) -> Option<()> {
    let before = idle_sample(system)?;
    time::sleep_ms(QUIET_MS);
    let after = idle_sample(system)?;
    let elapsed = after[0].saturating_sub(before[0]).max(1);
    let cpus = (after[1] as usize).min(MAX_CPUS);
    sys::print(b"bench: quiet idle (");
    num(QUIET_MS);
    sys::print(b" ms):");
    let mut total = 0u64;
    for cpu in 0..cpus {
        let idle = after[2 + cpu].saturating_sub(before[2 + cpu]);
        total += idle;
        sys::print(b" cpu ");
        num(cpu as u64);
        sys::print(b" ");
        num(idle.saturating_mul(1000) / elapsed);
        sys::print(b" permille");
    }
    sys::print(b" all ");
    num(total.saturating_mul(1000) / (elapsed * cpus.max(1) as u64));
    sys::print(b" permille\n");
    Some(())
}

fn main(_arg: u64) -> u64 {
    // The broker grants in manifest order: the system, then the file
    // space (the first endpoint), the network session (the second) and
    // the hardware object for the client buffers.
    let (mut system, mut fs_ep, mut net_ep, mut hardware) = (None, None, None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::ENDPOINT) {
            if fs_ep.is_none() {
                fs_ep = Some(h);
            } else {
                net_ep = Some(h);
            }
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(system), Some(ep), Some(net_ep), Some(hw)) = (system, fs_ep, net_ep, hardware) else {
        sys::print(b"bench: need system, fs and net (see the manifest)\n");
        return 1;
    };
    let Some(per_ms) = calibrate(system) else {
        sys::print(b"bench: could not calibrate the TSC\n");
        return 2;
    };
    let per_us = (per_ms / 1000).max(1);
    let per_ns = per_ms / 1_000_000; // 0 below 1 GHz: per_us*1000 is used
    sys::print(b"bench: calibration ");
    num(per_ms);
    sys::print(b" TSC cycles per ms\n");
    // 1. Minimal syscall.
    let mut samples = Vec::with_capacity(SYSCALLS);
    for _ in 0..SYSCALLS {
        let t0 = rdtsc();
        let _ = sys::thread_get_tid();
        samples.push(rdtsc() - t0);
    }
    // In ns: cycles * 1000 / cycles_per_us avoids division by zero.
    for s in samples.iter_mut() {
        *s = *s * 1000 / per_us;
    }
    let _ = per_ns;
    report(b"syscall thread_get_tid", &mut samples, 1, b"ns");
    // 2. Full IPC to fsd (request, reply, endpoint per call).
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 3;
    };
    let mut name = [0u8; 32];
    let mut samples = Vec::with_capacity(IPC_CALLS);
    for _ in 0..IPC_CALLS {
        let t0 = rdtsc();
        if fs.list(0, &mut name).is_err() {
            return 4;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"ipc fsd list", &mut samples, per_us, b"us");
    // 3. Files: atomic replace and 4 KiB read.
    let fd = match fs.open("medidas").or_else(|_| fs.create("medidas")) {
        Ok(fd) => fd,
        Err(_) => return 5,
    };
    let data = alloc::vec![0x5Au8; CHUNK];
    let mut samples = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let t0 = rdtsc();
        if fs.replace(fd, &data) != Ok(CHUNK) {
            return 6;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"replace 4 KiB", &mut samples, per_us, b"us");
    let mut back = alloc::vec![0u8; CHUNK];
    let mut samples = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let t0 = rdtsc();
        if fs.read(fd, 0, &mut back) != Ok(CHUNK) {
            return 7;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"read 4 KiB", &mut samples, per_us, b"us");
    let _ = fs.close(fd);
    // 4. A metadata call to the network service: no frame leaves the
    // machine, so it measures how soon the service sees the request
    // (performance audit of 731c398, finding B).
    let Ok(net) = NetClient::new(net_ep, hw) else {
        return 8;
    };
    let mut samples = Vec::with_capacity(NET_CALLS);
    for _ in 0..NET_CALLS {
        let t0 = rdtsc();
        if net.info().is_err() {
            return 9;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"ipc net info", &mut samples, per_us, b"us");
    // 5. Idle time over a quiet interval.
    if quiet_idle(system).is_none() {
        return 10;
    }
    sys::print(b"bench: done\n");
    0
}

aricode_rt::entry!(main);
