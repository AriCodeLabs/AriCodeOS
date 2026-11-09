// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `gpu_probe`: stage A of ADR 0080 (ADR 0083). Fills and copies a surface
//! with the GPU's blitter and with the CPU, timed side by side and checked
//! pixel by pixel; checks that the commands a non-secure batch could use to
//! reach the blitter's registers, or jump to a batch, are refused. Then
//! stage B (ADR 0086): EU kernels assembled here run on the render engine,
//! igt's fill first, then one that writes each thread group's number, both
//! checked byte by byte; then one that never ends, which must be caught and
//! reset, after which both engines must work.
//!
//! Exit codes: 0 all as expected; 2 no GPU engine here, and its calls
//! refused cleanly; 10 setup failed; 11 a fill was wrong; 12 a copy was
//! wrong; 13 a batch that must be refused was not, or something it aimed at
//! changed; 14 an EU thread that never ends was not reset; 15 the engines
//! did not work after that reset; 16 a call on an absent GPU was not refused as documented; 17 the
//! GPU's clock could not be asked for, was given above what was asked, or
//! did not speed the work up; 18 a dispatch could not be set up or did not
//! finish; 19 a dispatch wrote the wrong bytes; 20 a dispatch the hardware
//! would not take was not refused; 21 (`run gpu_probe 2`, the second client
//! of the residue test) a register showed what the last client left.
#![no_std]
#![no_main]

use aricode_gen7_eu::{g, Asm, ExecSize::*, Imm, Inst, Message, Reg};
use aricode_intel_gt::cmd::{self, Rect, Surface};
use aricode_intel_gt::compute::{self, Descriptor};
use aricode_rt::sys::abi::gpu::{Dispatch, DISPATCH_NUMBER};
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
/// Stage B: the dispatch's state and EU code, and the surface it writes.
const STATE_AT: u64 = 0xA0_0000;
const OUT_AT: u64 = 0xB0_0000;
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

/// A DMA buffer, or with `hardware` `None` a memory object (ADR 0084),
/// mapped for the CPU and at `at` for the GPU.
fn map(
    hardware: Option<Handle>,
    gpu: Handle,
    at: u64,
    bytes: u64,
) -> Result<&'static mut [u32], Error> {
    let memory = match hardware {
        Some(hw) => sys::dma_alloc(hw, bytes)?,
        None => sys::mem_create(bytes)?,
    };
    let virt = sys::mmio_map(memory)?;
    sys::gpu_map(gpu, memory, at)?;
    // SAFETY: `mmio_map` mapped `bytes` of this process's own buffer at
    // `virt`, kept while the process lives; nothing else aliases it here.
    Ok(unsafe { core::slice::from_raw_parts_mut(virt as *mut u32, (bytes / 4) as usize) })
}

fn main(arg: u64) -> u64 {
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
            && sys::gpu_frequency(gpu, 650) == Err(Error::NotFound)
            && sys::gpu_dispatch(gpu, &Dispatch::default()) == Err(Error::NotFound)
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
            // The source a DMA buffer, the destination a memory object: the
            // GPU reaches both kinds.
            src: map(Some(hardware), gpu, SRC_AT, BYTES)?,
            dst: map(None, gpu, DST_AT, BYTES)?,
        })
    })();
    let mut g = match setup {
        Ok(g) => g,
        Err(e) => {
            say(&[b"setup failed: ", error_name(e)]);
            return 10;
        }
    };
    // `run gpu_probe 2`: the second client of the residue test (ADR 0086,
    // B6), after a whole run left a pattern in the EU registers.
    if arg == 2 {
        return match residue_after_change(&mut g, hardware) {
            Ok(()) => 0,
            Err(code) => code,
        };
    }
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

    // 2b. The same fill and copy at each clock asked for, with the CPU
    // package's temperature after: what each costs in heat (ADR 0083). The
    // CPU only reads between runs (each fill has a colour of its own), so its
    // dirty cache lines do not slow the GPU. The clock the GPU reports is a
    // sample of the moment, low while it idles: the test is that work goes
    // faster, not one reading.
    let mut fill_at = [0u64; 3];
    for (k, mhz) in [350u64, 650, 1050].into_iter().enumerate() {
        let set = match sys::gpu_frequency(g.gpu, mhz) {
            Ok(m) => m,
            Err(e) => {
                say(&[b"frequency: ", error_name(e)]);
                return 17;
            }
        };
        if set > mhz.max(350) {
            say(&[b"asked ", n(mhz).s(), b" MHz, given more: ", n(set).s()]);
            return 17;
        }
        sys::thread_sleep(50);
        let (mut fill_t, mut copy_t) = ([0u64; RUNS], [0u64; RUNS]);
        for i in 0..RUNS {
            let shade = 0x0010_2030 + (k * RUNS + i) as u32;
            let each = cmd::fill(&surface(DST_AT), &WHOLE, shade).expect("a valid fill");
            fill_t[i] = match g.run(&each, WAIT_MS) {
                Ok(us) if g.dst.iter().all(|&p| p == shade) => us,
                Ok(_) => {
                    say(&[b"fill wrong at ", n(set).s(), b" MHz"]);
                    return 11;
                }
                Err(e) => {
                    say(&[b"fill at ", n(set).s(), b" MHz: ", error_name(e)]);
                    return 11;
                }
            };
            copy_t[i] = match g.run(&copy, WAIT_MS) {
                Ok(us) if g.dst == g.src => us,
                Ok(_) => {
                    say(&[b"copy wrong at ", n(set).s(), b" MHz"]);
                    return 12;
                }
                Err(e) => {
                    say(&[b"copy at ", n(set).s(), b" MHz: ", error_name(e)]);
                    return 12;
                }
            };
        }
        let sampled = g.report().mhz as u64;
        let mut t = [0u8; 8];
        let degrees = match sys::system_info(g.system, sysinfo::TEMPERATURE, &mut t) {
            Ok(8) => u32::from_le_bytes([t[0], t[1], t[2], t[3]]) as u64,
            _ => 0,
        };
        let (fill_best, _) = spread(&mut fill_t);
        let (copy_best, _) = spread(&mut copy_t);
        fill_at[k] = fill_best;
        say(&[
            b"asked ",
            n(mhz).s(),
            b" MHz, set ",
            n(set).s(),
            b" (sampled ",
            n(sampled).s(),
            b"): fill ",
            n(fill_best).s(),
            b" us (",
            n(rate(fill_best)).s(),
            b" MB/s), copy ",
            n(copy_best).s(),
            b" us (",
            n(rate(copy_best)).s(),
            b" MB/s), ",
            n(degrees).s(),
            b" C",
        ]);
    }
    // The highest clock must do the fill in at most three quarters of the
    // lowest's time (a third of it is what the clocks' ratio allows).
    if fill_at[2] * 4 > fill_at[0] * 3 {
        say(&[b"the clock did not speed the work up"]);
        return 17;
    }
    let _ = sys::gpu_frequency(g.gpu, 350);

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
    let dangerous: [(&[u8], &[u32]); 5] = [
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
        // The ring runs checked commands, never a batch in memory EU threads
        // could rewrite (ADR 0086).
        (
            b"jump to its own start",
            &[cmd::BATCH_START | cmd::BATCH_PPGTT, 0],
        ),
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

    if let Err(code) = dispatches(&mut g, hardware) {
        return code;
    }
    say(&[b"done"]);
    0
}

/// Where each piece of a dispatch's state sits in its page (offsets from
/// the bases, which are all the page's address).
const CODE: usize = 0x000;
const CURBE: usize = 0x200;
const BINDING_TABLE: usize = 0x240;
const SURFACE_STATE: usize = 0x280;
const DESCRIPTOR: usize = 0x2C0;
/// The surface: 16 by 16 thread groups, each writing 16 bytes of a row.
const OUT_WIDTH: u32 = 256;
const OUT_HEIGHT: u32 = 16;
const GROUPS: u32 = OUT_WIDTH / 16;

/// Assembles a kernel that writes one 16-byte row block per thread group,
/// at x = group X * 16 and y = group Y, of the byte `value` leaves in g1.0
/// (igt's `gpgpu_fill.gxa`, from its second instruction).
fn row_block_kernel(code: &mut [[u32; 4]], value: impl FnOnce(&mut Asm)) -> Option<usize> {
    let write = Message::media_block_write(0, 2).ok()?.desc_as_d();
    let eot = Message::thread_spawner_eot().desc_as_d();
    let mut asm = Asm::new(code);
    value(&mut asm);
    asm.mul(E1, g(2).ud(), g(0).ud().sub(1).scalar(), Imm::ud(0x10))
        .mov(E1, g(2).ud().sub(1), g(0).ud().sub(6).scalar())
        .mov(E8, g(4).ud(), g(0).ud())
        .mov(E2, g(4).ud(), g(2).ud().region(2, 2, 1))
        .mov(E1, g(4).ud().sub(2), Imm::ud(0xf))
        .mov(E16, g(5).ud(), g(1).ud().scalar())
        .emit(Inst::send(E16, Reg::acc0().uw(), g(4).d().scalar(), write))
        .mov(E8, g(112).ud(), g(0).ud())
        .emit(Inst::send(E16, Reg::null().uw(), g(112).d().scalar(), eot));
    asm.finish().ok().map(|p| p.len())
}

/// igt's fill: the byte from the CURBE data (g1), replicated.
fn fill_kernel(code: &mut [[u32; 4]]) -> Option<usize> {
    row_block_kernel(code, |asm| {
        asm.mov(E4, g(1).ub(), g(1).ub().scalar());
    })
}

/// Each group writes its number, X + 16 * Y, replicated.
fn group_kernel(code: &mut [[u32; 4]]) -> Option<usize> {
    row_block_kernel(code, |asm| {
        asm.shl(E1, g(1).ud(), g(0).ud().sub(6).scalar(), Imm::ud(4))
            .add(E1, g(1).ud(), g(1).ud().scalar(), g(0).ud().sub(1).scalar())
            .mov(E4, g(1).ub(), g(1).ub().scalar());
    })
}

/// The dispatch every kernel here runs with: one SIMD16 thread a group,
/// GROUPS by OUT_HEIGHT groups, one CURBE register.
fn out_dispatch() -> Dispatch {
    Dispatch {
        general_base: 0,
        surface_base: STATE_AT as u32,
        dynamic_base: STATE_AT as u32,
        instruction_base: STATE_AT as u32,
        scratch: 0,
        max_threads: 1,
        urb_entries: 0,
        urb_entry_regs: 0,
        curbe_regs: 1,
        curbe_offset: CURBE as u32,
        curbe_len: 32,
        descriptor_offset: DESCRIPTOR as u32,
        descriptors: 1,
        descriptor: 0,
        simd: 16,
        group: [1, 1, 1],
        start: [0, 0, 0],
        groups: [GROUPS, OUT_HEIGHT, 1],
        right_mask: 0xFFFF,
        bottom_mask: 0xFFFF_FFFF,
    }
}

/// Writes the state of a dispatch of `code` into `state`, its CURBE data
/// `curbe`.
fn write_state(state: &mut [u32], code: &[[u32; 4]], curbe: u32) -> Option<()> {
    state.fill(0);
    for (i, inst) in code.iter().enumerate() {
        state[CODE / 4 + 4 * i..CODE / 4 + 4 * i + 4].copy_from_slice(inst);
    }
    state[CURBE / 4] = curbe;
    state[BINDING_TABLE / 4] = SURFACE_STATE as u32;
    let surface = compute::surface_2d(
        compute::FORMAT_R8_UNORM,
        OUT_AT as u32,
        OUT_WIDTH,
        OUT_HEIGHT,
        OUT_WIDTH,
    )?;
    state[SURFACE_STATE / 4..SURFACE_STATE / 4 + 8].copy_from_slice(&surface);
    let descriptor = Descriptor {
        kernel: CODE as u32,
        binding_table: BINDING_TABLE as u32,
        binding_entries: 1,
        curbe_regs: 1,
        slm_blocks: 0,
        barrier: false,
    }
    .encode()?;
    state[DESCRIPTOR / 4..DESCRIPTOR / 4 + 8].copy_from_slice(&descriptor);
    Some(())
}

/// Stage B (ADR 0086): EU kernels on the render engine.
fn dispatches(g: &mut Gpu, hardware: Handle) -> Result<(), u64> {
    let before = g.report();
    if before.render != 1 {
        say(&[
            b"no render ring here (state ",
            n(before.render as u64).s(),
            b")",
        ]);
        let refused = sys::gpu_dispatch(g.gpu, &out_dispatch()) == Err(Error::NotFound);
        if !refused {
            say(&[b"a dispatch without a render ring was not refused with NotFound"]);
            return Err(20);
        }
        return Ok(());
    }
    let state = map(Some(hardware), g.gpu, STATE_AT, 4096).map_err(|e| {
        say(&[b"compute setup failed: ", error_name(e)]);
        18u64
    })?;
    let out = map(None, g.gpu, OUT_AT, 4096).map_err(|e| {
        say(&[b"compute setup failed: ", error_name(e)]);
        18u64
    })?;
    let mut code = [[0u32; 4]; 32];
    let run = |g: &mut Gpu, d: &Dispatch| -> Result<u64, Error> {
        let t0 = g.now();
        let number = sys::gpu_dispatch(g.gpu, d)?;
        sys::gpu_wait(g.gpu, number, WAIT_MS)?;
        Ok(g.now() - t0)
    };

    // 5. igt's fill, every byte the CURBE's.
    let Some(len) = fill_kernel(&mut code) else {
        say(&[b"the fill kernel did not assemble"]);
        return Err(18);
    };
    if write_state(state, &code[..len], 0xA5).is_none() {
        say(&[b"the dispatch's state did not encode"]);
        return Err(18);
    }
    out.fill(0);
    let took = run(g, &out_dispatch()).map_err(|e| {
        say(&[b"the fill dispatch failed: ", error_name(e)]);
        18u64
    })?;
    if let Some(i) = out.iter().position(|&w| w != 0xA5A5_A5A5) {
        say(&[
            b"FILL WRONG at byte ",
            n(4 * i as u64).s(),
            b": ",
            hex(out[i] as u64).s(),
        ]);
        return Err(19);
    }
    say(&[
        b"EU fill: ",
        n((GROUPS * OUT_HEIGHT) as u64).s(),
        b" thread groups, every byte right, in ",
        n(took).s(),
        b" us",
    ]);

    // 6. Each group's number, so a group run twice, not at all, or in the
    // wrong place shows.
    let Some(len) = group_kernel(&mut code) else {
        say(&[b"the group kernel did not assemble"]);
        return Err(18);
    };
    write_state(state, &code[..len], 0).ok_or(18u64)?;
    out.fill(0);
    let took = run(g, &out_dispatch()).map_err(|e| {
        say(&[b"the group dispatch failed: ", error_name(e)]);
        18u64
    })?;
    let bytes = out.iter().flat_map(|w| w.to_le_bytes());
    for (i, b) in bytes.enumerate() {
        let (x, y) = (i as u32 % OUT_WIDTH, i as u32 / OUT_WIDTH);
        let want = (x / 16 + GROUPS * y) as u8;
        if b != want {
            say(&[
                b"GROUP WRONG at x ",
                n(x as u64).s(),
                b" y ",
                n(y as u64).s(),
                b": ",
                n(b as u64).s(),
                b", want ",
                n(want as u64).s(),
            ]);
            return Err(19);
        }
    }
    say(&[
        b"EU groups: each of ",
        n((GROUPS * OUT_HEIGHT) as u64).s(),
        b" wrote its own number, in ",
        n(took).s(),
        b" us",
    ]);

    // 7. Descriptors the hardware would not take are refused, and a number
    // never given is not waited for.
    let mut bad = [out_dispatch(); 4];
    bad[0].descriptor_offset += 8;
    bad[1].simd = 4;
    bad[2].max_threads = 0;
    bad[3].groups[0] = 0;
    for d in &bad {
        if sys::gpu_dispatch(g.gpu, d) != Err(Error::Invalid) {
            say(&[b"a dispatch the hardware would not take was not refused"]);
            return Err(20);
        }
    }
    // Numbers compare with wrap-around, as the engine's do: one far ahead
    // of the last given is never given; one far behind reads as done.
    let ahead = DISPATCH_NUMBER | 0x7000_0000;
    let other_engine = DISPATCH_NUMBER << 1 | 1;
    if sys::gpu_wait(g.gpu, ahead, 0) != Err(Error::Invalid)
        || sys::gpu_wait(g.gpu, other_engine, 0) != Err(Error::Invalid)
    {
        say(&[b"a dispatch number never given was waited for"]);
        return Err(20);
    }
    say(&[b"4 bad descriptors refused as invalid"]);

    // 8. A hang (ADR 0086, B5): an EU thread that loops for ever. The
    // heartbeat resets the render engine; then both engines work.
    let mut asm = Asm::new(&mut code);
    let top = asm.label();
    asm.jmpi(None, top).emit(Inst::send(
        E16,
        Reg::null().uw(),
        aricode_gen7_eu::g(112).d().scalar(),
        Message::thread_spawner_eot().desc_as_d(),
    ));
    let Ok(looping) = asm.finish().map(|p| p.len()) else {
        say(&[b"the looping kernel did not assemble"]);
        return Err(18);
    };
    write_state(state, &code[..looping], 0).ok_or(18u64)?;
    let mut one = out_dispatch();
    one.groups = [1, 1, 1];
    let resets = g.report().resets;
    let t0 = g.now();
    match run(g, &one) {
        Err(Error::Hung) => {}
        other => {
            say(&[
                b"an EU thread that never ends was not reset: ",
                match other {
                    Ok(_) => b"it finished".as_slice(),
                    Err(e) => error_name(e),
                },
            ]);
            return Err(14);
        }
    }
    say(&[
        b"EU hang caught and the render engine reset after ",
        n((g.now() - t0) / 1000).s(),
        b" ms; resets ",
        n(resets).s(),
        b" -> ",
        n(g.report().resets).s(),
    ]);
    let Some(len) = fill_kernel(&mut code) else {
        return Err(18);
    };
    write_state(state, &code[..len], 0x3C).ok_or(18u64)?;
    out.fill(0);
    let fine = run(g, &out_dispatch()).is_ok() && out.iter().all(|&w| w == 0x3C3C_3C3C);
    let colour = 0x00C0_FFEE;
    let blit = cmd::fill(&surface(DST_AT), &WHOLE, colour).map_err(|_| 18u64)?;
    g.dst.fill(0);
    let blitter = g.run(&blit, WAIT_MS).is_ok() && g.dst.iter().all(|&p| p == colour);
    if !fine || !blitter {
        say(&[
            b"after the render reset: EU fill ",
            if fine { b"right" } else { b"WRONG" },
            b", blitter fill ",
            if blitter { b"right" } else { b"WRONG" },
        ]);
        return Err(15);
    }
    say(&[b"after the render reset the EUs and the blitter fill correctly"]);

    // The EU threads reached nothing of the kernel's.
    let r = g.report();
    if r.canary != before.canary || r.watchdog != before.watchdog {
        say(&[b"REACHED: the status page or the blitter's registers changed"]);
        return Err(13);
    }

    // 9. Residue (ADR 0086, B6), within this client: a pattern left in
    // every register, then read back by threads that write nothing first.
    // What they see is what any program would; the kernel clears it only
    // when the client changes (`run gpu_probe 2` checks that).
    let mut res = Residue::new(g, hardware)?;
    res.write(g)?;
    let (ran, seen) = res.read(g)?;
    say(&[
        b"residue within a client: the pattern seen on ",
        n(seen as u64).s(),
        b" of ",
        n(ran as u64).s(),
        b" thread slots read",
    ]);
    // The same after the GPU sat idle longer than a change of client
    // takes, so a clean second client owes nothing to the GT sleeping.
    res.write(g)?;
    sys::thread_sleep(IDLE_MS);
    let (ran, seen) = res.read(g)?;
    say(&[
        b"residue after ",
        n(IDLE_MS).s(),
        b" ms idle: the pattern seen on ",
        n(seen as u64).s(),
        b" of ",
        n(ran as u64).s(),
        b" slots",
    ]);
    res.write(g)?;
    say(&[b"pattern left for the next client: run gpu_probe 2"]);
    Ok(())
}

/// The idle time of the residue control, longer than `run gpu_probe 2`
/// takes to start over SSH.
const IDLE_MS: u64 = 5_000;
/// What one thread writes into every register in the residue test.
const PATTERN: u32 = 0xA5C3_5A3C;
/// The residue test's memory: its state page, and a surface of 256 bytes
/// by 64 rows: a row of 8 slots of 32 bytes a thread for each of 32 EUs
/// (what a reader saw), then 32 rows of 8 dwords (which slots ran).
const RESIDUE_AT: u64 = 0xC0_0000;
const RESIDUE_OUT_AT: u64 = 0xC1_0000;
const RESIDUE_BYTES: u64 = 256 * 64;

struct Residue {
    state: &'static mut [u32],
    out: &'static mut [u32],
    threads: u16,
}

impl Residue {
    fn new(g: &mut Gpu, hardware: Handle) -> Result<Self, u64> {
        let threads = g.report().threads;
        let fail = |e: Error| {
            say(&[b"residue setup failed: ", error_name(e)]);
            18u64
        };
        Ok(Residue {
            state: map(Some(hardware), g.gpu, RESIDUE_AT, 4096).map_err(fail)?,
            out: map(None, g.gpu, RESIDUE_OUT_AT, RESIDUE_BYTES).map_err(fail)?,
            threads,
        })
    }

    /// Runs `code` on every thread slot, the groups the kernel's clear uses.
    fn run(&mut self, g: &mut Gpu, code: &[[u32; 4]]) -> Result<(), u64> {
        let (per_group, groups) = aricode_intel_gt::clear::groups(self.threads).ok_or(18u64)?;
        self.state.fill(0);
        for (i, inst) in code.iter().enumerate() {
            self.state[4 * i..4 * i + 4].copy_from_slice(inst);
        }
        let surface = compute::surface_2d(
            compute::FORMAT_R8_UNORM,
            RESIDUE_OUT_AT as u32,
            256,
            64,
            256,
        )
        .ok_or(18u64)?;
        self.state[0xC40 / 4..0xC40 / 4 + 8].copy_from_slice(&surface);
        self.state[0xC00 / 4] = 0xC40;
        let descriptor = Descriptor {
            kernel: 0,
            binding_table: 0xC00,
            binding_entries: 1,
            curbe_regs: 0,
            slm_blocks: 0,
            barrier: false,
        }
        .encode()
        .ok_or(18u64)?;
        self.state[0xD00 / 4..0xD00 / 4 + 8].copy_from_slice(&descriptor);
        let d = Dispatch {
            surface_base: RESIDUE_AT as u32,
            dynamic_base: RESIDUE_AT as u32,
            instruction_base: RESIDUE_AT as u32,
            max_threads: self.threads,
            descriptor_offset: 0xD00,
            descriptors: 1,
            simd: 16,
            group: [per_group as u8, 1, 1],
            groups: [groups as u32, 1, 1],
            right_mask: 0xFFFF,
            bottom_mask: 0xFFFF_FFFF,
            ..Dispatch::default()
        };
        let number = sys::gpu_dispatch(g.gpu, &d).map_err(|e| {
            say(&[b"a residue dispatch was refused: ", error_name(e)]);
            18u64
        })?;
        sys::gpu_wait(g.gpu, number, WAIT_MS).map_err(|e| {
            say(&[b"a residue dispatch failed: ", error_name(e)]);
            18u64
        })
    }

    /// Every register but g0 (the payload) and g127 (the header kept for
    /// the end) to the pattern.
    fn write(&mut self, g: &mut Gpu) -> Result<(), u64> {
        let mut code = [[0u32; 4]; 80];
        let mut asm = Asm::new(&mut code);
        asm.mov(E8, aricode_gen7_eu::g(127).ud(), aricode_gen7_eu::g(0).ud())
            .mov(E8, aricode_gen7_eu::g(1).ud(), Imm::ud(PATTERN));
        for r in (2..126).step_by(2) {
            asm.mov(E16, aricode_gen7_eu::g(r).ud(), Imm::ud(PATTERN));
        }
        asm.mov(E8, aricode_gen7_eu::g(126).ud(), Imm::ud(PATTERN))
            .send(
                E8,
                Reg::null().ud(),
                aricode_gen7_eu::g(127).ud(),
                Message::thread_spawner_eot(),
            );
        let len = asm.finish().map_err(|_| 18u64)?.len();
        self.run(g, &code[..len])
    }

    /// Threads that OR every register they did not write (g1 to g126) and
    /// write it at their slot, with a mark that they ran. The slots that
    /// ran, and those that saw the pattern.
    fn read(&mut self, g: &mut Gpu) -> Result<(u32, u32), u64> {
        use aricode_gen7_eu::g as r;
        let write = Message::media_block_write(0, 1).map_err(|_| 18u64)?;
        let mut code = [[0u32; 4]; 160];
        let mut asm = Asm::new(&mut code);
        asm.mov(E8, r(127).ud(), r(0).ud());
        for k in 2..127 {
            asm.or(E8, r(1).ud(), r(1).ud(), r(k).ud());
        }
        asm.and(E1, r(126).ud(), Reg::sr0().ud().scalar(), Imm::ud(7))
            .shl(E1, r(126).ud(), r(126).ud().scalar(), Imm::ud(2))
            .shr(E1, r(126).ud().sub(1), Reg::sr0().ud().scalar(), Imm::ud(8))
            .and(
                E1,
                r(126).ud().sub(1),
                r(126).ud().sub(1).scalar(),
                Imm::ud(0x1F),
            )
            // What it saw: 32 bytes at x = thread * 32, y = row.
            .mov(E8, r(2).ud(), r(127).ud())
            .shl(E1, r(2).ud(), r(126).ud().scalar(), Imm::ud(3))
            .mov(E1, r(2).ud().sub(1), r(126).ud().sub(1).scalar())
            .mov(E1, r(2).ud().sub(2), Imm::ud(31))
            .mov(E8, r(3).ud(), r(1).ud())
            .send(E8, Reg::null().ud(), r(2).ud(), write)
            // That it ran: 1 at x = thread * 4, y = 32 + row.
            .mov(E8, r(4).ud(), r(127).ud())
            .mov(E1, r(4).ud(), r(126).ud().scalar())
            .add(
                E1,
                r(4).ud().sub(1),
                r(126).ud().sub(1).scalar(),
                Imm::ud(32),
            )
            .mov(E1, r(4).ud().sub(2), Imm::ud(3))
            .mov(E8, r(5).ud(), Imm::ud(1))
            .send(E8, Reg::null().ud(), r(4).ud(), write)
            .send(
                E8,
                Reg::null().ud(),
                r(127).ud(),
                Message::thread_spawner_eot(),
            );
        let len = asm.finish().map_err(|_| 18u64)?.len();
        self.out.fill(0);
        self.run(g, &code[..len])?;
        let (mut ran, mut seen) = (0, 0);
        for row in 0..32usize {
            for t in 0..8usize {
                if self.out[(32 + row) * 64 + t] != 1 {
                    continue;
                }
                ran += 1;
                let at = row * 64 + t * 8;
                if self.out[at..at + 8].iter().all(|&w| w == PATTERN) {
                    seen += 1;
                }
                if self.out[at..at + 8].iter().any(|&w| w != 0 && w != PATTERN) {
                    say(&[
                        b"a slot saw neither zero nor the pattern: ",
                        hex(self.out[at] as u64).s(),
                    ]);
                }
            }
        }
        Ok((ran, seen))
    }
}

/// The second client of the residue test: the kernel cleared what the last
/// one left, so no register shows the pattern.
fn residue_after_change(g: &mut Gpu, hardware: Handle) -> Result<(), u64> {
    let r = g.report();
    if r.render != 1 {
        say(&[b"no render ring here"]);
        return Err(18);
    }
    let mut res = Residue::new(g, hardware)?;
    let (ran, seen) = res.read(g)?;
    let r = g.report();
    say(&[
        b"the kernel's last clear reached ",
        n(r.cleared_threads as u64).s(),
        b" of ",
        n(r.threads as u64).s(),
        b" threads in ",
        n(r.clear_us as u64).s(),
        b" us; the pattern seen on ",
        n(seen as u64).s(),
        b" of ",
        n(ran as u64).s(),
        b" slots read",
    ]);
    if seen != 0 || ran == 0 {
        say(&[b"RESIDUE: a new client saw the last one's registers"]);
        return Err(21);
    }
    Ok(())
}

aricode_rt::entry!(main);
