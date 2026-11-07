// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `gpu_probe`: stage A of ADR 0080 (ADR 0083). Fills and copies a surface
//! with the GPU's blitter and with the CPU, timed side by side and checked
//! pixel by pixel; checks that the commands a non-secure batch could use to
//! reach the blitter's registers are refused; hangs the engine on purpose
//! and checks it is reset and works again.
//!
//! Exit codes: 0 all as expected; 2 no GPU engine here, and its calls
//! refused cleanly; 10 setup failed; 11 a fill was wrong; 12 a copy was
//! wrong; 13 a batch that must be refused was not, or something it aimed at
//! changed; 14 a hang was not caught; 15 the engine did not work after its
//! reset; 16 a call on an absent GPU was not refused as documented.
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
/// Where each surface sits in the GPU's address space.
const SRC_AT: u64 = 0x10_0000;
const DST_AT: u64 = 0x50_0000;
const RUNS: usize = 8;
const WAIT_MS: u64 = 5_000;

/// A number as text.
struct Text {
    buf: [u8; 20],
    len: usize,
}

impl Text {
    fn s(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

fn n(v: u64) -> Text {
    let mut b = [0u8; 20];
    let digits = fmt_u64(v, &mut b);
    let mut t = Text {
        buf: [0; 20],
        len: digits.len(),
    };
    t.buf[..t.len].copy_from_slice(digits);
    t
}

fn hex(v: u64) -> Text {
    let mut b = [0u8; 16];
    let digits = fmt_hex(v, 8, &mut b);
    let mut t = Text {
        buf: [0; 20],
        len: 2 + digits.len(),
    };
    t.buf[..2].copy_from_slice(b"0x");
    t.buf[2..t.len].copy_from_slice(digits);
    t
}

fn say(parts: &[&[u8]]) {
    sys::print(b"gpu_probe: ");
    for p in parts {
        sys::print(p);
    }
    sys::print(b"\n");
}

fn error_name(e: Error) -> &'static [u8] {
    match e {
        Error::Denied => b"refused by the kernel's check",
        Error::Hung => b"hung, and the engine was reset",
        Error::Timeout => b"timed out",
        Error::Busy => b"busy",
        Error::Invalid => b"invalid",
        Error::NotFound => b"no GPU",
        _ => b"another error",
    }
}

struct Gpu {
    gpu: Handle,
    system: Handle,
    src: &'static mut [u32],
    dst: &'static mut [u32],
}

impl Gpu {
    fn report(&self) -> GpuReport {
        let mut b = [0u8; GpuReport::LEN];
        sys::system_info(self.system, sysinfo::GPU, &mut b)
            .ok()
            .and_then(|_| GpuReport::decode(&b))
            .unwrap_or_default()
    }

    fn now(&self) -> u64 {
        time::uptime_us(self.system).unwrap_or(0)
    }

    /// Runs `words` then `MI_BATCH_BUFFER_END`, padded to whole quadwords;
    /// its time in microseconds.
    fn run(&mut self, words: &[u32], timeout_ms: u64) -> Result<u64, Error> {
        let mut batch = [cmd::NOOP; 32];
        batch[..words.len()].copy_from_slice(words);
        batch[words.len()] = cmd::BATCH_END;
        let len = (words.len() + 2) & !1;
        let t0 = self.now();
        let number = sys::gpu_submit(self.gpu, &batch[..len])?;
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
        n(gpu.0).s(),
        b" us, median ",
        n(gpu.1).s(),
        b" us (",
        n(rate(gpu.0)).s(),
        b" MB/s); cpu best ",
        n(cpu.0).s(),
        b" us, median ",
        n(cpu.1).s(),
        b" us (",
        n(rate(cpu.0)).s(),
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
        let end = [cmd::BATCH_END, cmd::NOOP];
        let refused = sys::gpu_submit(gpu, &end) == Err(Error::NotFound)
            && sys::gpu_wait(gpu, 1, 0) == Err(Error::NotFound)
            && sys::gpu_unmap(gpu, SRC_AT) == Err(Error::NotFound);
        let mapped = sys::dma_alloc(hardware, 4096).map(|d| sys::gpu_map(gpu, d, SRC_AT));
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
            src: map(hardware, gpu, SRC_AT, BYTES)?,
            dst: map(hardware, gpu, DST_AT, BYTES)?,
        })
    })();
    let mut g = match setup {
        Ok(g) => g,
        Err(e) => {
            say(&[b"setup failed: ", error_name(e)]);
            return 10;
        }
    };
    let r = g.report();
    say(&[
        b"engine ready at ",
        n(r.mhz as u64).s(),
        b" MHz (",
        n(r.mhz_lowest as u64).s(),
        b" to ",
        n(r.mhz_highest as u64).s(),
        b"), ",
        n(r.mapped_kib as u64).s(),
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
                say(&[b"fill: ", error_name(e)]);
                return 11;
            }
        }
        if let Some(at) = g.dst.iter().position(|&p| p != colour) {
            say(&[
                b"fill wrong at pixel ",
                n(at as u64).s(),
                b": ",
                hex(g.dst[at] as u64).s(),
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
                say(&[b"copy: ", error_name(e)]);
                return 12;
            }
        }
        if let Some(at) = (0..g.dst.len()).find(|&k| g.dst[k] != g.src[k]) {
            say(&[b"copy wrong at pixel ", n(at as u64).s()]);
            return 12;
        }
        let t0 = g.now();
        g.dst.copy_from_slice(core::hint::black_box(&g.src[..]));
        cpu_t[i] = g.now() - t0;
    }
    line(b"copy 3 MiB", spread(&mut gpu_t), spread(&mut cpu_t));

    // 3. What reached the blitter's registers and the status page on the
    // laptop (ADR 0083), each in a batch of its own, and a blit past the
    // client's memory: all refused, and nothing they aimed at changed.
    let before = g.report();
    let canary = before.status_page + 0x90 * 4;
    let past = cmd::fill(
        &Surface {
            address: (DST_AT + BYTES - PITCH as u64) as u32,
            ..surface(DST_AT)
        },
        &Rect {
            x1: 0,
            y1: 0,
            x2: WIDTH,
            y2: 2,
        },
        0xBAD0_0003,
    )
    .expect("a well-formed fill");
    let dangerous: [(&[u8], &[u32]); 4] = [
        (
            b"register load into the blitter's page",
            &[(0x22 << 23) | 1, 0x2_217C, 0x00AB_0000],
        ),
        (
            b"store by index into the status page",
            &[
                cmd::FLUSH_DW | cmd::FLUSH_DW_STORE_INDEX | cmd::FLUSH_DW_STORE_DWORD,
                (0x90 * 4) | cmd::FLUSH_DW_GLOBAL_GTT,
                0xBAD0_0002,
            ],
        ),
        (
            b"store through the global GTT",
            &[(0x20 << 23) | (1 << 22) | 2, 0, canary, 0xBAD0_0001],
        ),
        (b"fill past the client's memory", &past),
    ];
    for (what, words) in dangerous {
        match g.run(words, WAIT_MS) {
            Err(Error::Denied) => say(&[what, b": refused by the kernel's check"]),
            other => {
                say(&[
                    what,
                    b": NOT refused: ",
                    match other {
                        Ok(_) => b"it ran".as_slice(),
                        Err(e) => error_name(e),
                    },
                ]);
                return 13;
            }
        }
    }
    let after = g.report();
    say(&[
        b"canary ",
        hex(before.canary as u64).s(),
        b" -> ",
        hex(after.canary as u64).s(),
        b", watchdog ",
        hex(before.watchdog as u64).s(),
        b" -> ",
        hex(after.watchdog as u64).s(),
    ]);
    if after.canary != before.canary || after.watchdog != before.watchdog {
        say(&[b"REACHED: something a refused batch aimed at changed"]);
        return 13;
    }

    // 4. A hang: a batch that jumps to its own start.
    let t0 = g.now();
    let hang = [cmd::BATCH_START | cmd::BATCH_PPGTT, 0];
    match g.run(&hang, 15_000) {
        Err(Error::Hung) => {}
        other => {
            say(&[
                b"a batch that never ends was not reset: ",
                match other {
                    Ok(_) => b"it finished".as_slice(),
                    Err(e) => error_name(e),
                },
            ]);
            return 14;
        }
    }
    let r = g.report();
    say(&[
        b"hang caught and reset after ",
        n((g.now() - t0) / 1000).s(),
        b" ms; resets ",
        n(after.resets).s(),
        b" -> ",
        n(r.resets).s(),
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
