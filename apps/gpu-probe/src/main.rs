// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `gpu_probe`: stage A of ADR 0080 (ADR 0083). Fills and copies a surface
//! with the GPU's blitter and with the CPU, timed side by side and checked
//! pixel by pixel; measures what a non-secure batch can reach; hangs the
//! engine on purpose and checks it is reset and works again.
//!
//! Exit codes: 0 all as expected; 2 no GPU engine here, and its calls
//! refused cleanly; 10 setup failed; 11 a fill was wrong; 12 a copy was
//! wrong; 13 a non-secure batch reached what it must not; 14 a hang was not
//! caught; 15 the engine did not work after its reset; 16 a refusal was not
//! as documented.
#![no_std]
#![no_main]

use aricode_intel_gt::cmd::{self, Rect, Surface};
use aricode_rt::sys::abi::sysinfo::{self, GpuReport};
use aricode_rt::sys::{self, kind, permissions, time, Error, Handle};
use aricode_rt::{fmt_hex, fmt_u64};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const PITCH: u32 = WIDTH * 4;
const BYTES: u64 = PITCH as u64 * HEIGHT as u64;
/// Where each buffer sits in the GPU's address space.
const BATCH_AT: u64 = 0x1_0000;
const SRC_AT: u64 = 0x10_0000;
const DST_AT: u64 = 0x50_0000;
const BATCH_BYTES: u64 = 16 * 1024;
const RUNS: usize = 8;
const WAIT_MS: u64 = 5_000;

fn say(parts: &[&[u8]]) {
    sys::print(b"gpu_probe: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

fn n(v: u64) -> [u8; 20] {
    let mut b = [0u8; 20];
    let len = fmt_u64(v, &mut b).len();
    let mut out = [b' '; 20];
    out[..len].copy_from_slice(&b[..len]);
    out
}

/// `v` as text, trimmed.
fn t(buf: &[u8; 20]) -> &[u8] {
    let end = buf.iter().position(|&c| c == b' ').unwrap_or(20);
    &buf[..end]
}

fn hex(v: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    fmt_hex(v, 8, &mut b);
    b
}

struct Gpu {
    gpu: Handle,
    system: Handle,
    batch: &'static mut [u32],
    src: &'static mut [u32],
    dst: &'static mut [u32],
}

impl Gpu {
    fn report(&self) -> Option<GpuReport> {
        let mut b = [0u8; GpuReport::LEN];
        sys::system_info(self.system, sysinfo::GPU, &mut b).ok()?;
        GpuReport::decode(&b)
    }

    fn now(&self) -> u64 {
        time::uptime_us(self.system).unwrap_or(0)
    }

    /// Writes `words` and `MI_BATCH_BUFFER_END` at the batch's start, runs
    /// it, and returns its time in microseconds.
    fn run(&mut self, words: &[u32], timeout_ms: u64) -> Result<u64, Error> {
        let mut i = 0;
        for &w in words {
            self.batch[i] = w;
            i += 1;
        }
        self.batch[i] = cmd::BATCH_END;
        self.batch[i + 1] = cmd::NOOP;
        let t0 = self.now();
        let number = sys::gpu_submit(self.gpu, BATCH_AT)?;
        sys::gpu_wait(self.gpu, number, timeout_ms)?;
        Ok(self.now() - t0)
    }
}

fn surface(address: u64) -> Surface {
    Surface {
        address: address as u32,
        pitch: PITCH,
        width: WIDTH,
        height: HEIGHT,
    }
}

const WHOLE: Rect = Rect {
    x1: 0,
    y1: 0,
    x2: WIDTH,
    y2: HEIGHT,
};

/// Best and median of `runs`.
fn spread(runs: &mut [u64]) -> (u64, u64) {
    runs.sort_unstable();
    (runs[0], runs[runs.len() / 2])
}

/// MB/s for `BYTES` in `us`.
fn rate(us: u64) -> u64 {
    BYTES / us.max(1)
}

fn line(what: &[u8], gpu: (u64, u64), cpu: (u64, u64)) {
    say(&[
        what,
        b": gpu best ",
        t(&n(gpu.0)),
        b" us, median ",
        t(&n(gpu.1)),
        b" us (",
        t(&n(rate(gpu.0))),
        b" MB/s); cpu best ",
        t(&n(cpu.0)),
        b" us, median ",
        t(&n(cpu.1)),
        b" us (",
        t(&n(rate(cpu.0))),
        b" MB/s)",
    ]);
}

fn map(hardware: Handle, gpu: Handle, at: u64, bytes: u64) -> Result<&'static mut [u32], Error> {
    let dma = sys::dma_alloc(hardware, bytes)?;
    let virt = sys::mmio_map(dma)?;
    sys::gpu_map(gpu, dma, at)?;
    // SAFETY: `mmio_map` mapped `bytes` of this process's own buffer at
    // `virt`, kept while the process lives; nothing else aliases it here.
    Ok(unsafe { core::slice::from_raw_parts_mut(virt as *mut u32, (bytes / 4) as usize) })
}

fn main(_arg: u64) -> u64 {
    let (mut gpu, mut system, mut hardware) = (None, None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::GPU) {
            gpu = Some(h);
        } else if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(gpu), Some(system), Some(hardware)) = (gpu, system, hardware) else {
        say(&[b"missing a capability (gpu, system, hardware)"]);
        return 10;
    };
    let mut b = [0u8; GpuReport::LEN];
    let report = sys::system_info(system, sysinfo::GPU, &mut b)
        .ok()
        .and_then(|_| GpuReport::decode(&b))
        .unwrap_or_default();
    if report.state != sysinfo::GPU_READY {
        let why = match report.state {
            sysinfo::GPU_REFUSED => sysinfo::gpu_refusal(report.refusal).unwrap_or("?"),
            sysinfo::GPU_OFF => "gpu=off",
            _ => "no supported GPU",
        };
        say(&[b"no GPU engine here: ", why.as_bytes()]);
        // Its calls must say so, and take nothing.
        let refused = sys::gpu_submit(gpu, BATCH_AT) == Err(Error::NotFound)
            && sys::gpu_wait(gpu, 1, 0) == Err(Error::NotFound)
            && sys::gpu_unmap(gpu, BATCH_AT) == Err(Error::NotFound);
        let dma = sys::dma_alloc(hardware, 4096);
        let mapped = dma.map(|d| sys::gpu_map(gpu, d, BATCH_AT));
        if !refused || mapped != Ok(Err(Error::NotFound)) {
            say(&[b"a call on the absent GPU was not refused with NotFound"]);
            return 16;
        }
        say(&[b"calls refused with NotFound, as documented"]);
        return 2;
    }
    let setup = (|| {
        Ok::<_, Error>(Gpu {
            gpu,
            system,
            batch: map(hardware, gpu, BATCH_AT, BATCH_BYTES)?,
            src: map(hardware, gpu, SRC_AT, BYTES)?,
            dst: map(hardware, gpu, DST_AT, BYTES)?,
        })
    })();
    let mut g = match setup {
        Ok(g) => g,
        Err(e) => {
            aricode_rt::print_error(b"gpu_probe: setup", e);
            return 10;
        }
    };
    let r = g.report().unwrap_or_default();
    say(&[
        b"engine ready at ",
        t(&n(r.mhz as u64)),
        b" MHz (",
        t(&n(r.mhz_lowest as u64)),
        b" to ",
        t(&n(r.mhz_highest as u64)),
        b"), ",
        t(&n(r.mapped_kib as u64)),
        b" KiB mapped",
    ]);

    // 1. Fill.
    let colour = 0x0033_6699;
    let fill = cmd::fill(&surface(DST_AT), &WHOLE, colour).expect("a valid fill");
    let (mut gpu_t, mut cpu_t) = ([0u64; RUNS], [0u64; RUNS]);
    for i in 0..RUNS {
        g.dst.fill(0);
        match g.run(&fill, WAIT_MS) {
            Ok(us) => gpu_t[i] = us,
            Err(e) => {
                aricode_rt::print_error(b"gpu_probe: fill", e);
                return 11;
            }
        }
        if let Some(at) = g.dst.iter().position(|&p| p != colour) {
            say(&[
                b"fill wrong at pixel ",
                t(&n(at as u64)),
                b": ",
                &hex(g.dst[at] as u64),
            ]);
            return 11;
        }
        let t0 = g.now();
        g.dst.fill(core::hint::black_box(colour + i as u32));
        cpu_t[i] = g.now() - t0;
    }
    line(b"fill 3 MiB", spread(&mut gpu_t), spread(&mut cpu_t));

    // 2. Copy.
    for (i, p) in g.src.iter_mut().enumerate() {
        *p = (i as u32).wrapping_mul(2_654_435_761);
    }
    let copy = cmd::copy(&surface(DST_AT), 0, 0, &surface(SRC_AT), &WHOLE).expect("a valid copy");
    for i in 0..RUNS {
        g.dst.fill(0);
        match g.run(&copy, WAIT_MS) {
            Ok(us) => gpu_t[i] = us,
            Err(e) => {
                aricode_rt::print_error(b"gpu_probe: copy", e);
                return 12;
            }
        }
        if let Some(at) = (0..g.dst.len()).find(|&k| g.dst[k] != g.src[k]) {
            say(&[b"copy wrong at pixel ", t(&n(at as u64))]);
            return 12;
        }
        let t0 = g.now();
        g.dst.copy_from_slice(core::hint::black_box(&g.src[..]));
        cpu_t[i] = g.now() - t0;
    }
    line(b"copy 3 MiB", spread(&mut gpu_t), spread(&mut cpu_t));

    // 3. What a non-secure batch reaches (ADR 0083, "still to measure"): a
    // store to the global GTT, a flush that stores to the status page by
    // index, and a register load into the blitter's own page. Each must
    // leave what it aimed at as it was.
    let before = g.report().unwrap_or_default();
    let canary = before.status_page + 0x90 * 4;
    let probe = [
        // MI_STORE_DATA_IMM with "use the global GTT" (Linux
        // MI_STORE_DWORD_IMM_GEN4 | MI_USE_GGTT): 0, address, value.
        (0x20 << 23) | (1 << 22) | 2,
        0,
        canary,
        0xBAD0_0001,
        // MI_FLUSH_DW storing by index into the status page.
        cmd::FLUSH_DW | cmd::FLUSH_DW_STORE_INDEX | cmd::FLUSH_DW_STORE_DWORD,
        (0x90 * 4) | cmd::FLUSH_DW_GLOBAL_GTT,
        0xBAD0_0002,
        // MI_LOAD_REGISTER_IMM of the blitter's watchdog threshold.
        (0x22 << 23) | 1,
        0x2_217C,
        0x00AB_0000,
        cmd::NOOP,
    ];
    let outcome = g.run(&probe, WAIT_MS);
    let after = g.report().unwrap_or_default();
    say(&[
        b"non-secure batch ",
        match outcome {
            Ok(_) => b"finished".as_slice(),
            Err(Error::Hung) => b"hung the engine (reset)",
            Err(_) => b"failed",
        },
        b": canary ",
        &hex(before.canary as u64),
        b" -> ",
        &hex(after.canary as u64),
        b", watchdog ",
        &hex(before.watchdog as u64),
        b" -> ",
        &hex(after.watchdog as u64),
    ]);
    if after.canary != before.canary || after.watchdog != before.watchdog {
        say(&[b"REACHED: a non-secure batch wrote what it must not"]);
        return 13;
    }
    say(&[b"not reached: privileged writes from a non-secure batch did not land"]);

    // 4. A hang: a batch that jumps to itself.
    let resets = after.resets;
    let t0 = g.now();
    let hang = [cmd::BATCH_START | cmd::BATCH_PPGTT, BATCH_AT as u32];
    match g.run(&hang, 15_000) {
        Err(Error::Hung) => {}
        other => {
            say(&[
                b"a batch that never ends was not reset: ",
                match other {
                    Ok(_) => b"it finished".as_slice(),
                    Err(Error::Timeout) => b"the wait timed out",
                    Err(_) => b"another error",
                },
            ]);
            return 14;
        }
    }
    let r = g.report().unwrap_or_default();
    say(&[
        b"hang caught and reset after ",
        t(&n((g.now() - t0) / 1000)),
        b" ms; resets ",
        t(&n(resets)),
        b" -> ",
        t(&n(r.resets)),
    ]);
    g.dst.fill(0);
    match g.run(&fill, WAIT_MS) {
        Ok(_) if g.dst.iter().all(|&p| p == colour) => {
            say(&[b"after the reset the engine fills correctly"])
        }
        _ => {
            say(&[b"the engine did not work after its reset"]);
            return 15;
        }
    }
    say(&[b"done"]);
    0
}

aricode_rt::entry!(main);
