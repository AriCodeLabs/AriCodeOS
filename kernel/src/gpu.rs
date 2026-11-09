// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The GPU's engine on Intel Ivy Bridge (ADR 0083): the kernel owns the
//! global GTT, the one client's PPGTT, the blitter's ring and every register;
//! a program with the `GPU` object maps its DMA buffers and submits batches,
//! which the kernel copies, checks and runs non-secure in the program's own
//! address space. The decisions live in `aricode-intel-gt`; this is the glue.
//!
//! The engine is never used under a spin lock: resets and waits on it take
//! up to tens of milliseconds, so a call takes the engine out of `GPU`, works
//! with interrupts on, and puts it back. A process that ends while it is the
//! client only leaves a mark, since it ends where nothing may wait; the
//! `gpu` kernel thread, or the next call, lets go of its mappings.
//!
//! Memory the GPU might still reach is never freed: a mapping keeps its
//! buffer alive, and when a reset fails, or taking the engine fails halfway,
//! the pages it could reach are kept for good (`hw::quarantine`).

use crate::framebuffer::{self, Bar0};
use crate::object::{ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use crate::user::process;
use alloc::boxed::Box;
use alloc::vec::Vec;
use aricode_abi::sysinfo::{GpuReport, GPU_ABSENT, GPU_OFF, GPU_READY, GPU_REFUSED};
use aricode_abi::Error;
use aricode_intel_gt::check::{self, Range};
use aricode_intel_gt::gtt::{self, Cache, Layout};
use aricode_intel_gt::power::{self, Forcewake, Limits};
use aricode_intel_gt::regs::{bcs, masked_off, masked_on};
use aricode_intel_gt::ring::{self, Ring};
use aricode_intel_gt::{self as gt, ppgtt, regs, render, Refusal, Timeouts};
use aricode_pmm::FRAME_SIZE;
use log::{info, warn};

/// Pages of the ring (Linux's 16 KiB).
const RING_PAGES: u32 = 4;
/// The kernel's copies of client batches: this many pages, each holding one
/// batch of up to a page, mapped in the client's PPGTT from `COPY_AT`, below
/// `gpu::FIRST`, where no mapping of the client's can reach them.
const COPIES: usize = 4;
const COPY_AT: u32 = 0x8000;
const _: () = assert!(COPY_AT as u64 + COPIES as u64 * 4096 <= aricode_abi::gpu::FIRST);
/// Mappings a client may hold (audit M4): bounds the kernel's memory and the
/// check's work per batch.
const MAX_MAPS: usize = 256;
/// The render status page's dword its flushes may store to (Linux's
/// `I915_GEM_HWS_SCRATCH`).
const RENDER_SCRATCH: u32 = 0x80;
/// A dword of the status page nothing writes; see `GpuReport::canary`.
const CANARY_INDEX: usize = 0x90;
/// The blitter's watchdog threshold (PRM Vol 1 Part 4, 2.1.9.2): unused,
/// read back as a probe of what non-secure batches can write.
const WATCHDOG: u32 = 0x2_217C;
/// With work pending and no batch finishing for this long, the engine is
/// hung (Linux's heartbeat is 2.5 s). `gpu.hang_ms` changes it, within
/// `HANG_MS_RANGE` (audit L4).
const HANG_MS: u64 = 2_000;
const HANG_MS_RANGE: core::ops::RangeInclusive<u64> = 100..=60_000;
/// Waits spin this long before they sleep, so short batches are timed well.
const SPIN_US: u64 = 2_000;
/// How often the `gpu` thread looks for a client that ended and at the
/// temperature.
const REAP_MS: u64 = 100;
/// At this CPU package temperature the GPU drops to its lowest clock
/// (`gpu.hot_c`): the laptop reached 90 C in 25 s at 1050 MHz under Linux
/// (ADR 0080, 4b). It may rise again `COOL_MARGIN` degrees below.
const HOT_C: u32 = 85;
const COOL_MARGIN: u32 = 5;
static HOT_AT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(HOT_C);
/// The GPU is held at its lowest clock for heat.
static HOT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The brake's state last applied to the GPU: the thread retries until it
/// matches `HOT` (audit M1).
static BRAKE_APPLIED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl gt::Regs for Bar0 {
    fn read(&self, offset: u32) -> u32 {
        Bar0::read(self, offset)
    }

    fn write(&mut self, offset: u32, value: u32) {
        Bar0::write(self, offset, value)
    }
}

struct Clock;

impl gt::Clock for Clock {
    fn now_us(&self) -> u64 {
        crate::time::uptime_us()
    }
}

struct Mapping {
    address: u32,
    pages: u32,
    /// Keeps the buffer's frames allocated while the GPU may reach them.
    dma: ObjectRef,
}

/// The engine's own pages. All of them are reachable by the GPU.
struct Pages {
    scratch: ObjectRef,
    status: ObjectRef,
    ring: ObjectRef,
    copies: ObjectRef,
    /// The PPGTT's 512 tables, contiguous: every directory entry is written
    /// once, at boot, and never changes (audit M3).
    tables: ObjectRef,
    /// The render engine's status page and ring (ADR 0086).
    render_status: ObjectRef,
    render_ring: ObjectRef,
}

impl Pages {
    fn all(self) -> [ObjectRef; 7] {
        [
            self.scratch,
            self.status,
            self.ring,
            self.copies,
            self.tables,
            self.render_status,
            self.render_ring,
        ]
    }
}

struct Engine {
    fw: Forcewake,
    layout: Layout,
    ring: Ring,
    limits: Limits,
    timeouts: Timeouts,
    hang_us: u64,
    pages: Pages,
    scratch_pte: u32,
    /// The number of the request that last used each copy page.
    copy_seq: [u32; COPIES],
    maps: Vec<Mapping>,
    /// The next batch's number, and the last one submitted.
    next: u32,
    submitted: u32,
    requests: u64,
    resets: u64,
    /// Numbers lost to the last reset: those after `.0` up to `.1`.
    lost: (u32, u32),
    /// The number of the last batch finished, and when it changed or the
    /// engine got work while idle. The status page is the kernel's alone:
    /// no checked batch can write it.
    progress: (u32, u64),
    /// A reset failed: the engine takes no more work, and what it could
    /// reach is never freed.
    dead: bool,
    /// The clock asked for at boot (`gpu.mhz`), the client's, the most any
    /// may have (`gpu.max_mhz`), and the one set now, in 50 MHz steps.
    boot_step: u8,
    asked: u8,
    cap: u8,
    step: u8,
    /// The render engine's ring, its next request's number, and whether it
    /// started and passed its boot check (ADR 0086).
    render_ring: Ring,
    render_next: u32,
    render_ok: bool,
    /// What the boot check of the render ring reported (`GpuReport`).
    render_state: u8,
    render_us: u16,
}

enum Status {
    Absent,
    Off,
    Refused(Refusal),
    Ready,
}

struct Shared {
    status: Status,
    /// `None` while a call has it out.
    engine: Option<Box<Engine>>,
    client: Option<u32>,
    /// The client ended; its mappings go at the next call.
    client_gone: bool,
}

static GPU: IrqLock<Shared> = IrqLock::new(Shared {
    status: Status::Absent,
    engine: None,
    client: None,
    client_gone: false,
});

fn phys(o: &ObjectRef) -> u64 {
    match &o.kind {
        ObjectKind::Dma(d) => d.region.phys,
        _ => unreachable!("the engine's pages are DMA buffers"),
    }
}

/// The pages at `phys` as dwords, through the physical map.
fn dwords(phys: u64, pages: u32) -> &'static mut [u32] {
    // SAFETY: frames the engine allocated and keeps for its whole life,
    // reachable through the physical map; only the engine's holder writes
    // them, and the engine is out of `GPU` for one call at a time.
    unsafe {
        core::slice::from_raw_parts_mut(
            crate::memory::phys::phys_to_virt(phys) as *mut u32,
            (pages as u64 * FRAME_SIZE / 4) as usize,
        )
    }
}

/// A dword of the status page, which the GPU writes: read as such (audit L6).
fn status_dword(e: &Engine, index: usize) -> u32 {
    let page = dwords(phys(&e.pages.status), 1);
    // SAFETY: inside the status page; a volatile read of memory a device
    // writes.
    unsafe { core::ptr::read_volatile(&page[index]) }
}

/// The number of the last batch finished.
fn stored(e: &Engine) -> u32 {
    status_dword(e, ring::SEQNO_INDEX as usize)
}

/// Writes the CPU's cached lines of `[addr, addr + len)` back to memory: page
/// tables are not snooped (PRM Vol 1 Part 2, 2.2.2).
fn flush_lines(addr: *const u32, len: usize) {
    let start = addr as u64 & !63;
    let end = addr as u64 + len as u64;
    let mut line = start;
    while line < end {
        // SAFETY: `clflush` of mapped kernel memory has no other effect.
        unsafe {
            core::arch::asm!("clflush [{}]", in(reg) line, options(nostack, preserves_flags))
        };
        line += 64;
    }
    // SAFETY: a fence touches no memory.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

/// Takes the GPU's engine at boot, if it is a supported Intel GPU in the
/// state the probe saw; otherwise leaves it as the firmware set it and says
/// why (ADR 0083). After `framebuffer::use_panel_size`: where the screen ends
/// decides which GTT entries stay the firmware's.
pub fn init(
    off: bool,
    mhz: Option<u32>,
    hang_ms: Option<u64>,
    max_mhz: Option<u32>,
    hot_c: Option<u32>,
) {
    // Below the thermal watch's power-off point, or the brake would never
    // act first (audit L5).
    let power_off = crate::arch::x86_64::thermal::off_at();
    let ceiling = if power_off > 55 { power_off - 5 } else { 100 };
    let hot = hot_c.unwrap_or(HOT_C).clamp(50, ceiling.min(100));
    HOT_AT.store(hot, core::sync::atomic::Ordering::Relaxed);
    let Some(d) = crate::pci::intel_display() else {
        return;
    };
    let hang_ms = hang_ms
        .unwrap_or(HANG_MS)
        .clamp(*HANG_MS_RANGE.start(), *HANG_MS_RANGE.end());
    let status = if !gt::SUPPORTED.contains(&d.device) {
        Status::Refused(Refusal::NotThisFamily)
    } else if off {
        info!("gpu: left as the firmware set it (gpu=off)");
        Status::Off
    } else {
        let t0 = crate::time::uptime_us();
        match bring_up(&d, mhz, max_mhz, hang_ms * 1000) {
            Ok(e) => {
                let mut e = e;
                if let Some(mut regs) = framebuffer::mapped_bar0() {
                    match render_bring_up(&mut e, &mut regs) {
                        Ok((first, again)) => {
                            e.render_ok = true;
                            e.render_state = 1;
                            e.render_us = first.min(u16::MAX as u64) as u16;
                            info!(
                                "gpu: render ring ready: an empty request in {first} us; \
                                 reset and started again, another in {again} us"
                            );
                        }
                        Err(why) => {
                            e.render_state = 2;
                            if let Ok(mut awake) = e.fw.hold(&mut regs, &Clock) {
                                awake.write(regs::RENDER.ctl(), 0);
                                awake.write(regs::RENDER.mi_mode(), masked_on(bcs::MODE_STOP));
                            }
                            warn!("gpu: render ring left off: {why}; the blitter goes on");
                        }
                    }
                }
                // Without the thread or a sensor there is no brake: no
                // clock above the boot's unless gpu.max_mhz says so (M3).
                let thread = crate::task::scheduler::spawn("gpu", reaper, 0);
                let sensor = crate::arch::x86_64::thermal::read().is_some();
                if (thread.is_err() || !sensor) && max_mhz.is_none() {
                    e.cap = e.boot_step;
                    warn!(
                        "gpu: no heat brake (thread: {}, sensor: {sensor}): clock capped at {} MHz",
                        thread.is_ok(),
                        e.cap as u32 * power::STEP_MHZ
                    );
                }
                info!(
                    "gpu: blitter ready in {} us; GTT entries 0..{} kept for the screen, \
                     ring at {:#x}, PPGTT directory at entry {}; {} MHz, at most {}, \
                     the lowest from {hot} C",
                    crate::time::uptime_us() - t0,
                    e.layout.kept,
                    e.layout.ring * FRAME_SIZE as u32,
                    e.layout.directory,
                    e.step as u32 * power::STEP_MHZ,
                    e.cap as u32 * power::STEP_MHZ,
                );
                GPU.lock().engine = Some(e);
                Status::Ready
            }
            Err(Refusal::Partial) => Status::Refused(Refusal::Partial),
            Err(why) => {
                warn!("gpu: left as the firmware set it: {why:?}");
                Status::Refused(why)
            }
        }
    };
    GPU.lock().status = status;
}

fn bring_up(
    d: &crate::pci::IntelDisplay,
    mhz: Option<u32>,
    max_mhz: Option<u32>,
    hang_us: u64,
) -> Result<Box<Engine>, Refusal> {
    // Reading, and the firmware's forcewake bits dropped: nothing else yet.
    let (bar0, aperture) = d.bars.ok_or(Refusal::BadBars)?;
    let mut regs = framebuffer::intel_bar0(bar0.0).ok_or(Refusal::BadBars)?;
    let found = gt::inspect(&regs, d.device, bar0.1, d.mggc0)?;
    let (clock, timeouts) = (Clock, Timeouts::default());
    let fw = Forcewake::init(&mut regs, &clock, timeouts)?;
    gt::check_blitter(&fw.hold(&mut regs, &clock)?)?;
    let stolen = (d.bdsm & 0xFFF0_0000) as u64;
    let fb = framebuffer::current();
    let screen_end = fb
        .address
        .checked_sub(aperture.0)
        .ok_or(Refusal::BadBars)?
        .saturating_add(framebuffer::screen_bytes(&fb));
    let kept = gtt::firmware_run(&regs, found.gtt_entries, stolen, screen_end)?;
    let aperture_entries = u32::try_from(aperture.1 / FRAME_SIZE).map_err(|_| Refusal::BadBars)?;
    let layout = Layout::new(kept, found.gtt_entries, RING_PAGES, aperture_entries)
        .ok_or(Refusal::GttSize)?;
    let alloc = |pages: u32| {
        crate::hw::dma_alloc(pages as u64 * FRAME_SIZE, None).map_err(|_| Refusal::NoMemory)
    };
    let pages = Pages {
        scratch: alloc(1)?,
        status: alloc(1)?,
        ring: alloc(RING_PAGES)?,
        copies: alloc(COPIES as u32)?,
        tables: alloc(layout.directory_entries)?,
        render_status: alloc(1)?,
        render_ring: alloc(RING_PAGES)?,
    };
    let scratch_pte = gtt::encode(phys(&pages.scratch), Cache::Uncached).ok_or(Refusal::GttSize)?;
    // From the first write to the tables on, a failure stops the engine and
    // keeps its pages for good (audit M2).
    let mut ring = Ring::new(RING_PAGES);
    let to_step = |m: u32| (m / power::STEP_MHZ).min(255) as u8;
    let cap = found
        .limits
        .clamp(max_mhz.map_or(found.limits.highest, to_step));
    let step = found
        .limits
        .clamp(mhz.map_or(found.limits.lowest, to_step))
        .min(cap);
    let programmed = (|| -> Result<(), Refusal> {
        let llc = |p: u64| gtt::encode(p, Cache::Llc).ok_or(Refusal::GttSize);
        let tables = dwords(phys(&pages.tables), layout.directory_entries);
        tables.fill(scratch_pte);
        for i in 0..COPIES {
            let (dir, entry) = ppgtt::split(COPY_AT + i as u32 * FRAME_SIZE as u32);
            tables[(dir * ppgtt::TABLE_ENTRIES + entry) as usize] =
                llc(phys(&pages.copies) + i as u64 * FRAME_SIZE)?;
        }
        flush_lines(tables.as_ptr(), tables.len() * 4);
        // Every entry the firmware did not use points at the scratch page;
        // then the status page and the ring, cached in the LLC as Linux maps
        // them; then the directory, each entry naming its table.
        gtt::clear(&mut regs, kept, layout.directory, scratch_pte);
        gtt::set(&mut regs, layout.status, llc(phys(&pages.status))?);
        gtt::set(
            &mut regs,
            layout.render_status,
            llc(phys(&pages.render_status))?,
        );
        for i in 0..RING_PAGES {
            gtt::set(
                &mut regs,
                layout.render_ring + i,
                llc(phys(&pages.render_ring) + i as u64 * FRAME_SIZE)?,
            );
        }
        for i in 0..RING_PAGES {
            gtt::set(
                &mut regs,
                layout.ring + i,
                llc(phys(&pages.ring) + i as u64 * FRAME_SIZE)?,
            );
        }
        for i in 0..layout.directory_entries {
            let table = phys(&pages.tables) + i as u64 * FRAME_SIZE;
            let pde = ppgtt::directory_entry(table).ok_or(Refusal::GttSize)?;
            ppgtt::set_directory(&mut regs, &layout, i, pde);
        }
        ppgtt::flush_directory(&mut regs);
        let mut awake = fw.hold(&mut regs, &clock)?;
        // Linux's Ivy Bridge settings, which the firmware does not apply
        // (ADR 0086), before any engine starts.
        render::apply(&mut awake, &render::GT);
        render::apply(&mut awake, &render::CLOCK_GATING);
        render::apply(&mut awake, &render::RENDER);
        ring::prepare(&mut awake);
        ring::start(
            &mut awake,
            &clock,
            &timeouts,
            &regs::BLITTER,
            &layout,
            &mut ring,
        )?;
        power::quiet_rps_interrupts(&mut awake);
        power::set_frequency(&mut awake, &found.limits, step);
        Ok(())
    })();
    if let Err(why) = programmed {
        warn!("gpu: taking the engine failed halfway ({why:?}): stopped, its pages kept for good");
        if let Ok(mut awake) = fw.hold(&mut regs, &clock) {
            for engine in [regs::BLITTER, regs::RENDER] {
                awake.write(engine.ctl(), 0);
                awake.write(engine.mode(), masked_off(bcs::PPGTT_ENABLE));
                awake.write(engine.mi_mode(), masked_on(bcs::MODE_STOP));
            }
        }
        for p in pages.all() {
            crate::hw::quarantine(p);
        }
        return Err(Refusal::Partial);
    }
    Ok(Box::new(Engine {
        fw,
        layout,
        ring,
        limits: found.limits,
        timeouts,
        hang_us,
        pages,
        scratch_pte,
        copy_seq: [0; COPIES],
        maps: Vec::new(),
        next: 1,
        submitted: 0,
        requests: 0,
        resets: 0,
        lost: (0, 0),
        progress: (0, crate::time::uptime_us()),
        dead: false,
        boot_step: step,
        asked: step,
        cap,
        step,
        render_ring: Ring::new(RING_PAGES),
        render_next: 1,
        render_ok: false,
        render_state: 0,
        render_us: 0,
    }))
}

/// Writes `body` into the render ring as one request and submits it; its
/// number, or `None` if the ring has no room or the GT does not wake.
fn render_request(e: &mut Engine, regs: &mut Bar0, body: &[u32]) -> Option<u32> {
    let page = e.layout.render_status * FRAME_SIZE as u32;
    let seqno = e.render_next;
    let mut words = [0u32; 256];
    let n = render::request(
        page + RENDER_SCRATCH * 4,
        page + ring::SEQNO_INDEX * 4,
        seqno,
        body,
        &mut words,
    )?;
    let mut awake = e.fw.hold(regs, &Clock).ok()?;
    let head = awake.read(regs::RENDER.head());
    let tail = e.render_ring.emit(
        dwords(phys(&e.pages.render_ring), RING_PAGES),
        head,
        &words[..n],
    )?;
    // SAFETY: a fence touches no memory.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
    ring::submit(&mut awake, &regs::RENDER, tail);
    e.render_next = match seqno.wrapping_add(1) {
        0 => 1,
        n => n,
    };
    Some(seqno)
}

/// Waits up to `limit_us` for render request `seqno`; the time it took.
fn render_wait(e: &Engine, seqno: u32, limit_us: u64) -> Option<u64> {
    let page = dwords(phys(&e.pages.render_status), 1);
    let start = crate::time::uptime_us();
    loop {
        // SAFETY: inside the status page; the GPU writes it.
        let stored = unsafe { core::ptr::read_volatile(&page[ring::SEQNO_INDEX as usize]) };
        let now = crate::time::uptime_us();
        if ring::passed(stored, seqno) {
            return Some(now - start);
        }
        if now - start > limit_us {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// Starts the render engine's ring and checks it at boot (ADR 0086, B2): an
/// empty request, a reset of the render domain with its settings applied
/// again, and another. On any failure the ring is stopped and the blitter
/// goes on alone.
fn render_bring_up(e: &mut Engine, regs: &mut Bar0) -> Result<(u64, u64), &'static str> {
    const LIMIT_US: u64 = 50_000;
    let (fw, timeouts, layout) = (e.fw, e.timeouts, e.layout);
    {
        let mut awake = fw.hold(regs, &Clock).map_err(|_| "forcewake")?;
        ring::start(
            &mut awake,
            &Clock,
            &timeouts,
            &regs::RENDER,
            &layout,
            &mut e.render_ring,
        )
        .map_err(|_| "the ring did not start")?;
    }
    let first = render_request(e, regs, &[]).ok_or("no room in the ring")?;
    let took = render_wait(e, first, LIMIT_US).ok_or("an empty request did not finish")?;
    {
        let mut awake = fw.hold(regs, &Clock).map_err(|_| "forcewake")?;
        ring::reset(&mut awake, &Clock, &timeouts, &regs::RENDER)
            .map_err(|_| "the render domain did not reset")?;
        render::apply(&mut awake, &render::RENDER);
        ring::start(
            &mut awake,
            &Clock,
            &timeouts,
            &regs::RENDER,
            &layout,
            &mut e.render_ring,
        )
        .map_err(|_| "the ring did not start again")?;
    }
    let second = render_request(e, regs, &[]).ok_or("no room after the reset")?;
    let again = render_wait(e, second, LIMIT_US)
        .ok_or("an empty request did not finish after the reset")?;
    Ok((took, again))
}

/// The engine, out of `GPU` for one call.
struct Taken(Option<Box<Engine>>);

impl core::ops::Deref for Taken {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        self.0.as_ref().expect("taken")
    }
}

impl core::ops::DerefMut for Taken {
    fn deref_mut(&mut self) -> &mut Engine {
        self.0.as_mut().expect("taken")
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        GPU.lock().engine = self.0.take();
    }
}

/// Takes the engine, waiting while another call has it; first lets go of a
/// client that ended. `NotFound` without an engine.
fn take() -> Result<(Taken, Option<u32>), Error> {
    loop {
        let got = {
            let mut g = GPU.lock();
            if !matches!(g.status, Status::Ready) {
                return Err(Error::NotFound);
            }
            g.engine.take().map(|e| {
                let gone = core::mem::take(&mut g.client_gone);
                if gone {
                    g.client = None;
                }
                (e, gone)
            })
        };
        match got {
            Some((e, gone)) => {
                let mut taken = Taken(Some(e));
                if gone {
                    let dropped = release_client(&mut taken);
                    // The buffers go outside any lock.
                    drop(dropped);
                }
                let client = GPU.lock().client;
                return Ok((taken, client));
            }
            None => crate::task::scheduler::yield_now(),
        }
    }
}

/// Lets go of a client that ended within `REAP_MS`, so its buffers and a
/// batch it left looping do not wait for another program's call (audit L3).
extern "C" fn reaper(_arg: usize) {
    use core::sync::atomic::Ordering;
    loop {
        crate::task::scheduler::sleep_ms(REAP_MS);
        if GPU.lock().client_gone {
            drop(take());
        }
        // The heat brake: the CPU's package carries the GPU too.
        let Some((degrees, _)) = crate::arch::x86_64::thermal::read() else {
            continue;
        };
        let hot_at = HOT_AT.load(Ordering::Relaxed);
        let was = HOT.load(Ordering::Relaxed);
        let now = power::too_hot(was, degrees, hot_at, COOL_MARGIN);
        HOT.store(now, Ordering::Relaxed);
        // Until the GPU has taken the brake's state, every tick tries again.
        if now != BRAKE_APPLIED.load(Ordering::Relaxed) {
            if let (Ok((mut e, _)), Some(mut regs)) = (take(), framebuffer::mapped_bar0()) {
                if apply_frequency(&mut e, &mut regs) {
                    BRAKE_APPLIED.store(now, Ordering::Relaxed);
                    warn!(
                        "gpu: {degrees} C: clock {} MHz",
                        e.step as u32 * power::STEP_MHZ
                    );
                }
            }
        }
    }
}

/// Sets the clock the client asked for, within the cap, or the lowest
/// while the machine is hot; `false` if the GPU could not be told.
fn apply_frequency(e: &mut Engine, regs: &mut Bar0) -> bool {
    let want = if HOT.load(core::sync::atomic::Ordering::Relaxed) {
        e.limits.lowest
    } else {
        e.asked.min(e.cap)
    };
    if want == e.step {
        return true;
    }
    match e.fw.hold(regs, &Clock) {
        Ok(mut awake) => {
            e.step = power::set_frequency(&mut awake, &e.limits, want);
            true
        }
        Err(_) => false,
    }
}

/// `gpu_frequency`: the client asks for `mhz`; the clock granted, in MHz,
/// within the GPU's limits and `gpu.max_mhz`, and the lowest while the
/// machine is hot.
pub fn frequency(mhz: u64) -> Result<u64, Error> {
    let (mut e, _) = take()?;
    if e.dead {
        return Err(Error::Hung);
    }
    claim()?;
    let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
    e.asked = e
        .limits
        .clamp((mhz / power::STEP_MHZ as u64).min(255) as u8);
    if !apply_frequency(&mut e, &mut regs) {
        return Err(Error::Busy);
    }
    Ok(e.step as u64 * power::STEP_MHZ as u64)
}

/// Makes the calling process the client if there is none; `Busy` if
/// another one is.
fn claim() -> Result<(), Error> {
    let pid = process::current_pid().ok_or(Error::Invalid)?.0;
    let mut g = GPU.lock();
    match g.client {
        None => {
            g.client = Some(pid);
            Ok(())
        }
        Some(c) if c == pid && !g.client_gone => Ok(()),
        Some(_) => Err(Error::Busy),
    }
}

fn is_client() -> bool {
    let pid = process::current_pid().map(|p| p.0);
    let g = GPU.lock();
    pid.is_some() && g.client == pid && !g.client_gone
}

/// The engine has done everything it was given: it has consumed the ring to
/// its tail, and the last batch's number has been stored after the flush that
/// follows it (audit L2).
fn idle(e: &Engine, regs: &mut Bar0) -> bool {
    let consumed = match e.fw.hold(regs, &Clock) {
        Ok(awake) => awake.read(bcs::HEAD) & bcs::HEAD_ADDR == e.ring.tail(),
        Err(_) => false,
    };
    consumed && ring::passed(stored(e), e.submitted)
}

/// Resets the blitter and starts its ring again; every batch submitted is
/// lost. A reset that fails leaves the engine dead.
fn reset(e: &mut Engine, regs: &mut Bar0) {
    let before = stored(e);
    let ok = e.fw.hold(regs, &Clock).is_ok_and(|mut awake| {
        ring::reset(&mut awake, &Clock, &e.timeouts, &regs::BLITTER).is_ok()
            && ring::start(
                &mut awake,
                &Clock,
                &e.timeouts,
                &regs::BLITTER,
                &e.layout,
                &mut e.ring,
            )
            .is_ok()
    });
    dwords(phys(&e.pages.status), 1)[ring::SEQNO_INDEX as usize] = e.submitted;
    e.lost = (before, e.submitted);
    e.resets += 1;
    e.progress = (e.submitted, crate::time::uptime_us());
    if ok {
        warn!(
            "gpu: blitter reset; batches {} to {} lost",
            before.wrapping_add(1),
            e.submitted
        );
    } else {
        e.dead = true;
        warn!("gpu: blitter reset failed; the engine takes no more work");
    }
}

/// Lets go of the client: stops the engine if it still works for it, points
/// its pages back at the scratch page, and returns its buffers. If the engine
/// cannot be stopped, the buffers are kept for good instead (audit H1).
fn release_client(e: &mut Engine) -> Vec<Mapping> {
    let Some(mut regs) = framebuffer::mapped_bar0() else {
        return Vec::new();
    };
    if !e.dead && !idle(e, &mut regs) {
        reset(e, &mut regs);
    }
    let maps = core::mem::take(&mut e.maps);
    for m in &maps {
        set_pages(e, m.address, m.pages, None);
    }
    // The next client starts at the boot's clock.
    e.asked = e.boot_step;
    let _ = apply_frequency(e, &mut regs);
    if e.dead {
        for m in maps {
            let bytes = crate::hw::quarantine(m.dma);
            warn!("gpu: {bytes} bytes the stuck engine may still reach kept for good");
        }
        return Vec::new();
    }
    maps
}

/// Points the PPGTT's pages from `address` at `frame(i)`, or at the
/// scratch page; `false` if a frame could not be had, the pages done so far
/// pointed back at the scratch page.
fn set_pages(
    e: &mut Engine,
    address: u32,
    pages: u32,
    frame: Option<&dyn Fn(u32) -> Option<u64>>,
) -> bool {
    let tables = dwords(phys(&e.pages.tables), e.layout.directory_entries);
    let at = |i: u32| {
        let (dir, entry) = ppgtt::split(address + i * FRAME_SIZE as u32);
        (dir * ppgtt::TABLE_ENTRIES + entry) as usize
    };
    for i in 0..pages {
        let value = match frame {
            None => e.scratch_pte,
            Some(f) => match f(i).and_then(|p| gtt::encode(p, Cache::Llc)) {
                Some(v) => v,
                None => {
                    for j in 0..i {
                        tables[at(j)] = e.scratch_pte;
                        flush_lines(&tables[at(j)], 4);
                    }
                    return false;
                }
            },
        };
        tables[at(i)] = value;
        flush_lines(&tables[at(i)], 4);
    }
    true
}

/// `gpu_map`: `dma`, a DMA buffer or a memory object (ADR 0084), at
/// `address` in the client's PPGTT. A memory object's pages are taken as
/// they are mapped, and stay while the mapping holds the object.
pub fn map(dma: ObjectRef, address: u64) -> Result<(), Error> {
    use aricode_abi::gpu::{FIRST, PAGE, SIZE};
    let len = match &dma.kind {
        ObjectKind::Dma(d) => d.region.len.next_multiple_of(PAGE),
        ObjectKind::Mem(m) => m.len(),
        _ => return Err(Error::Invalid),
    };
    // Checked before any page is taken (audit of ADR 0084, H2).
    if !address.is_multiple_of(PAGE)
        || address < FIRST
        || address.saturating_add(len) > SIZE
        || len == 0
    {
        return Err(Error::Invalid);
    }
    let (address, pages) = (address as u32, (len / PAGE) as u32);
    let (mut e, _) = take()?;
    if e.dead {
        return Err(Error::Hung);
    }
    claim()?;
    if e.maps.len() >= MAX_MAPS || e.maps.try_reserve(1).is_err() {
        return Err(Error::Limit);
    }
    let end = address + pages * PAGE as u32;
    if e.maps
        .iter()
        .any(|m| address < m.address + m.pages * PAGE as u32 && m.address < end)
    {
        return Err(Error::Invalid);
    }
    let frame = |i: u32| match &dma.kind {
        ObjectKind::Dma(d) => Some(d.region.phys + i as u64 * PAGE),
        ObjectKind::Mem(m) => m.page(i as usize),
        _ => None,
    };
    if !set_pages(&mut e, address, pages, Some(&frame)) {
        return Err(Error::Limit);
    }
    // Every table exists from boot; the next request invalidates the GPU's
    // TLB before its batch.
    e.maps.push(Mapping {
        address,
        pages,
        dma,
    });
    Ok(())
}

/// `gpu_unmap`.
pub fn unmap(address: u64) -> Result<(), Error> {
    let (mut e, _) = take()?;
    if !is_client() {
        return Err(Error::Busy);
    }
    let i = e
        .maps
        .iter()
        .position(|m| m.address as u64 == address)
        .ok_or(Error::Invalid)?;
    // A stuck engine may still write the buffer: it stays (audit H1).
    if e.dead {
        return Err(Error::Hung);
    }
    let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
    if !idle(&e, &mut regs) {
        return Err(Error::Busy);
    }
    let m = e.maps.remove(i);
    set_pages(&mut e, m.address, m.pages, None);
    drop(e);
    drop(m);
    Ok(())
}

/// Whether the blitter still runs in the PPGTT the kernel set (audit L2): a
/// power state's restore or anything else unmeasured must not leave it
/// running batches through the global GTT.
fn ppgtt_in_place(e: &Engine, regs: &mut Bar0) -> bool {
    e.fw.hold(regs, &Clock).is_ok_and(|awake| {
        awake.read(bcs::MODE) & bcs::PPGTT_ENABLE != 0
            && awake.read(bcs::PP_DIR_BASE) == e.layout.pp_dir_base()
    })
}

/// `gpu_submit`: the batch at `ptr`, `len` bytes of the caller's memory, is
/// copied into one of the kernel's pages, checked there, and that copy runs
/// (ADR 0083: a non-secure batch reaches the blitter's registers).
pub fn submit(ptr: u64, len: u64) -> Result<u64, Error> {
    if !(8..=FRAME_SIZE).contains(&len) || !len.is_multiple_of(8) {
        return Err(Error::Invalid);
    }
    let bytes = process::copy_from_user(ptr, len)?;
    let mut words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let (mut e, _) = take()?;
    if e.dead {
        return Err(Error::Hung);
    }
    claim()?;
    let e = &mut *e;
    let slot = e.next as usize % COPIES;
    let finished = stored(e);
    if e.copy_seq[slot] != 0 && !ring::passed(finished, e.copy_seq[slot]) {
        return Err(Error::Busy);
    }
    let at = COPY_AT + slot as u32 * FRAME_SIZE as u32;
    let maps: Vec<Range> = e
        .maps
        .iter()
        .map(|m| Range {
            start: m.address as u64,
            end: m.address as u64 + m.pages as u64 * FRAME_SIZE,
        })
        .collect();
    let used = match check::check(&mut words, at, &maps) {
        Ok(n) => n,
        Err(fault) => {
            process::record_event(
                aricode_abi::sysinfo::EventKind::Fault,
                fault.at() as u64,
                "gpu batch refused",
            );
            return Err(Error::Denied);
        }
    };
    let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
    if !ppgtt_in_place(e, &mut regs) {
        warn!("gpu: the blitter's PPGTT was not as set; resetting it");
        reset(e, &mut regs);
        if e.dead || !ppgtt_in_place(e, &mut regs) {
            e.dead = true;
            return Err(Error::Hung);
        }
    }
    let mut awake = e.fw.hold(&mut regs, &Clock).map_err(|_| Error::Busy)?;
    let head = awake.read(bcs::HEAD);
    let idle_before = head & bcs::HEAD_ADDR == e.ring.tail();
    let seqno = e.next;
    let copy = &mut dwords(phys(&e.pages.copies), COPIES as u32)[slot * 1024..(slot + 1) * 1024];
    copy[..used].copy_from_slice(&words[..used]);
    let tail = e
        .ring
        .request(dwords(phys(&e.pages.ring), RING_PAGES), head, at, seqno)
        .ok_or(Error::Busy)?;
    e.next = match seqno.wrapping_add(1) {
        0 => 1,
        n => n,
    };
    e.copy_seq[slot] = seqno;
    // The copy's and the ring's writes reach memory before the engine is
    // told of them.
    // SAFETY: a fence touches no memory.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
    ring::submit(&mut awake, &regs::BLITTER, tail);
    if idle_before {
        e.progress = (finished, crate::time::uptime_us());
    }
    e.submitted = seqno;
    e.requests += 1;
    Ok(seqno as u64)
}

/// `gpu_wait`.
pub fn wait(seqno: u64, timeout_ms: u64) -> Result<u64, Error> {
    let seqno = u32::try_from(seqno).map_err(|_| Error::Invalid)?;
    let start = crate::time::uptime_us();
    let deadline = start.saturating_add(timeout_ms.saturating_mul(1000));
    loop {
        if process::current_kill_reason().is_some() {
            return Err(Error::Killed);
        }
        {
            let (mut e, _) = take()?;
            if !is_client() {
                return Err(Error::Busy);
            }
            if seqno == 0 || !ring::passed(e.submitted, seqno) {
                return Err(Error::Invalid);
            }
            if !ring::passed(e.lost.0, seqno) && ring::passed(e.lost.1, seqno) {
                return Err(Error::Hung);
            }
            let finished = stored(&e);
            if ring::passed(finished, seqno) {
                return Ok(0);
            }
            if e.dead {
                return Err(Error::Hung);
            }
            // A batch of the checked commands ends in well under a second:
            // none finishing for `hang_us` with work pending is a hang
            // (Linux's heartbeat, not its old head sampling, which a batch
            // looping on itself keeps moving).
            let now = crate::time::uptime_us();
            if finished != e.progress.0 {
                e.progress = (finished, now);
            } else if now.saturating_sub(e.progress.1) > e.hang_us {
                let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
                reset(&mut e, &mut regs);
                return Err(Error::Hung);
            }
        }
        let now = crate::time::uptime_us();
        if now >= deadline {
            return Err(Error::Timeout);
        }
        if now - start < SPIN_US {
            crate::task::scheduler::yield_now();
        } else {
            crate::task::scheduler::sleep_ms(1);
        }
    }
}

/// A process ended: if it was the client, the `gpu` thread lets go of its
/// mappings. Called where nothing may wait (the scheduler's reaping).
pub fn process_gone(pid: u32) {
    let mut g = GPU.lock();
    if g.client == Some(pid) {
        g.client_gone = true;
    }
}

/// Whether the kernel drives the device at `bus:device.function`, which then
/// may not be claimed (ADR 0083).
pub fn holds(bus: u8, device: u8, function: u8) -> bool {
    (bus, device, function) == (0, 2, 0) && matches!(GPU.lock().status, Status::Ready)
}

/// `system_info(GPU)`. It never waits for the engine, nor lets go of a
/// client (audit L7): while a call has the engine out, it says only what
/// needs no engine.
pub fn report() -> GpuReport {
    let (status, client, engine) = {
        let mut g = GPU.lock();
        let status = match g.status {
            Status::Absent => (GPU_ABSENT, 0),
            Status::Off => (GPU_OFF, 0),
            Status::Refused(r) => (GPU_REFUSED, r as u8),
            Status::Ready => (GPU_READY, 0),
        };
        (
            status,
            g.client.unwrap_or(0),
            g.engine.take().map(|e| Taken(Some(e))),
        )
    };
    let mut r = GpuReport {
        state: status.0,
        refusal: status.1,
        client,
        ..GpuReport::default()
    };
    let Some(e) = engine else {
        return r;
    };
    let mhz = |step: u8| (step as u32 * power::STEP_MHZ) as u16;
    r.mhz_lowest = mhz(e.limits.lowest);
    r.render = e.render_state;
    r.render_us = e.render_us;
    r.mhz_highest = mhz(e.limits.highest);
    r.requests = e.requests;
    r.resets = e.resets;
    r.canary = status_dword(&e, CANARY_INDEX);
    r.status_page = e.layout.status * FRAME_SIZE as u32;
    r.mapped_kib = e.maps.iter().map(|m| m.pages * 4).sum();
    if let Some(mut regs) = framebuffer::mapped_bar0() {
        if let Ok(awake) = e.fw.hold(&mut regs, &Clock) {
            r.mhz = mhz(power::frequency(&awake));
            r.watchdog = awake.read(WATCHDOG);
        }
    }
    r
}
