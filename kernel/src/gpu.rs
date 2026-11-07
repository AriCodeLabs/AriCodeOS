// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The GPU's engine on Intel Ivy Bridge (ADR 0083): the kernel owns the
//! global GTT, the one client's PPGTT, the blitter's ring and every register;
//! a program with the `GPU` object maps its DMA buffers and submits batches
//! that run non-secure in its own address space. The decisions live in
//! `aricode-intel-gt`; this is the glue.
//!
//! The engine is never used under a spin lock: resets and waits on it take
//! up to tens of milliseconds, so a call takes the engine out of `GPU`, works
//! with interrupts on, and puts it back. A process that ends while it is the
//! client only leaves a mark: its mappings, which keep its buffers alive, go
//! at the next call, from a context that may wait for the engine.

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
use aricode_intel_gt::ring::{self, Ring};
use aricode_intel_gt::{self as gt, ppgtt, regs, Refusal, Timeouts};
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
/// A dword of the status page nothing writes; see `GpuReport::canary`.
const CANARY_INDEX: usize = 0x90;
/// The blitter's watchdog threshold (PRM Vol 1 Part 4, 2.1.9.2): unused,
/// read back as a probe of what non-secure batches can write.
const WATCHDOG: u32 = 0x2_217C;
/// With work pending and no batch finishing for this long, the engine is
/// hung: Linux's heartbeat is
/// 2.5 s. `gpu.hang_ms` changes it.
const HANG_MS: u64 = 2_000;
/// Waits spin this long before they sleep, so short batches are timed well.
const SPIN_US: u64 = 2_000;

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
    _dma: ObjectRef,
}

struct Engine {
    fw: Forcewake,
    layout: Layout,
    ring: Ring,
    limits: Limits,
    timeouts: Timeouts,
    hang_us: u64,
    ring_mem: ObjectRef,
    status: ObjectRef,
    /// The batch copies, and the number of the request that last used each.
    copies: ObjectRef,
    copy_seq: [u32; COPIES],
    _scratch: ObjectRef,
    _scratch_table: ObjectRef,
    scratch_pte: u32,
    tables: Vec<Option<ObjectRef>>,
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
    /// A reset failed: the engine takes no more work.
    dead: bool,
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

/// The page at `phys` as dwords, through the physical map.
fn dwords(phys: u64, pages: u32) -> &'static mut [u32] {
    // SAFETY: frames the engine allocated and keeps for its whole life,
    // reachable through the physical map; only the engine's holder touches
    // them, and the engine is out of `GPU` for one call at a time.
    unsafe {
        core::slice::from_raw_parts_mut(
            crate::memory::phys::phys_to_virt(phys) as *mut u32,
            (pages as u64 * FRAME_SIZE / 4) as usize,
        )
    }
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
/// why (ADR 0083). After `framebuffer::use_panel_size`: the screen's size
/// decides which GTT entries stay the firmware's.
pub fn init(off: bool, mhz: Option<u32>, hang_ms: Option<u64>) {
    let Some(d) = crate::pci::intel_display() else {
        return;
    };
    let status = if !gt::SUPPORTED.contains(&d.device) {
        Status::Refused(Refusal::NotThisFamily)
    } else if off {
        info!("gpu: left as the firmware set it (gpu=off)");
        Status::Off
    } else {
        let t0 = crate::time::uptime_us();
        match bring_up(&d, mhz, hang_ms.unwrap_or(HANG_MS) * 1000) {
            Ok(e) => {
                info!(
                    "gpu: blitter ready in {} us; GTT entries 0..{} kept for the screen, \
                     ring at {:#x}, PPGTT directory at entry {}, {} MHz",
                    crate::time::uptime_us() - t0,
                    e.layout.kept,
                    e.layout.ring * FRAME_SIZE as u32,
                    e.layout.directory,
                    e.limits.lowest as u32 * power::STEP_MHZ,
                );
                GPU.lock().engine = Some(e);
                Status::Ready
            }
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
    hang_us: u64,
) -> Result<Box<Engine>, Refusal> {
    let (bar0, _) = d.bars.ok_or(Refusal::BadBars)?;
    let mut regs = framebuffer::intel_bar0(bar0.0).ok_or(Refusal::BadBars)?;
    let found = gt::inspect(&regs, d.device, bar0.1, d.mggc0)?;
    let (clock, timeouts) = (Clock, Timeouts::default());
    let fw = Forcewake::init(&mut regs, &clock, timeouts)?;
    gt::check_blitter(&fw.hold(&mut regs, &clock)?)?;
    let stolen = (d.bdsm & 0xFFF0_0000) as u64;
    let screen = framebuffer::screen_bytes(&framebuffer::current());
    let kept = gtt::firmware_run(&regs, found.gtt_entries, stolen, screen)?;
    let layout = Layout::new(kept, found.gtt_entries, RING_PAGES).ok_or(Refusal::GttSize)?;
    let alloc = |pages: u32| {
        crate::hw::dma_alloc(pages as u64 * FRAME_SIZE, None).map_err(|_| Refusal::NoMemory)
    };
    let (scratch, status, ring_mem, scratch_table) =
        (alloc(1)?, alloc(1)?, alloc(RING_PAGES)?, alloc(1)?);
    let (copies, first_table) = (alloc(COPIES as u32)?, alloc(1)?);
    let scratch_pte = gtt::encode(phys(&scratch), Cache::Uncached).ok_or(Refusal::GttSize)?;
    let table = dwords(phys(&scratch_table), 1);
    table.fill(scratch_pte);
    flush_lines(table.as_ptr(), table.len() * 4);
    // Every entry the firmware did not use points at the scratch page; then
    // the status page and the ring, cached in the LLC as Linux maps them.
    gtt::clear(&mut regs, kept, layout.directory, scratch_pte);
    let llc = |p: u64| gtt::encode(p, Cache::Llc).ok_or(Refusal::GttSize);
    gtt::set(&mut regs, layout.status, llc(phys(&status))?);
    for i in 0..RING_PAGES {
        gtt::set(
            &mut regs,
            layout.ring + i,
            llc(phys(&ring_mem) + i as u64 * FRAME_SIZE)?,
        );
    }
    let pde = ppgtt::directory_entry(phys(&scratch_table)).ok_or(Refusal::GttSize)?;
    for i in 1..layout.directory_entries {
        ppgtt::set_directory(&mut regs, &layout, i, pde);
    }
    // The first table holds the batch copies, in every client's PPGTT.
    let t = dwords(phys(&first_table), 1);
    t.fill(scratch_pte);
    for i in 0..COPIES {
        let (_, entry) = ppgtt::split(COPY_AT + i as u32 * FRAME_SIZE as u32);
        t[entry as usize] = llc(phys(&copies) + i as u64 * FRAME_SIZE)?;
    }
    flush_lines(t.as_ptr(), t.len() * 4);
    let first = ppgtt::directory_entry(phys(&first_table)).ok_or(Refusal::GttSize)?;
    ppgtt::set_directory(&mut regs, &layout, 0, first);
    ppgtt::flush_directory(&mut regs);
    let mut ring = Ring::new(RING_PAGES);
    {
        let mut awake = fw.hold(&mut regs, &clock)?;
        ring::prepare(&mut awake);
        ring::start(&mut awake, &clock, &timeouts, &layout, &mut ring)?;
        let step = mhz.map_or(found.limits.lowest, |m| {
            (m / power::STEP_MHZ).min(255) as u8
        });
        power::set_frequency(&mut awake, &found.limits, step);
    }
    let mut tables = Vec::new();
    tables.resize_with(layout.directory_entries as usize, || None);
    tables[0] = Some(first_table);
    Ok(Box::new(Engine {
        fw,
        layout,
        ring,
        limits: found.limits,
        timeouts,
        hang_us,
        ring_mem,
        status,
        copies,
        copy_seq: [0; COPIES],
        _scratch: scratch,
        _scratch_table: scratch_table,
        scratch_pte,
        tables,
        maps: Vec::new(),
        next: 1,
        submitted: 0,
        requests: 0,
        resets: 0,
        lost: (0, 0),
        progress: (0, crate::time::uptime_us()),
        dead: false,
    }))
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

/// The engine has consumed every command given: the hardware says so, not
/// the status page, which a batch might write (ADR 0083).
fn idle(e: &Engine, regs: &mut Bar0) -> bool {
    match e.fw.hold(regs, &Clock) {
        Ok(awake) => awake.read(regs::bcs::HEAD) & regs::bcs::HEAD_ADDR == e.ring.tail(),
        Err(_) => false,
    }
}

/// Resets the blitter and starts its ring again; every batch submitted is
/// lost. A reset that fails leaves the engine dead.
fn reset(e: &mut Engine, regs: &mut Bar0) {
    let stored = dwords(phys(&e.status), 1)[ring::SEQNO_INDEX as usize];
    let ok = e.fw.hold(regs, &Clock).is_ok_and(|mut awake| {
        ring::reset(&mut awake, &Clock, &e.timeouts).is_ok()
            && ring::start(&mut awake, &Clock, &e.timeouts, &e.layout, &mut e.ring).is_ok()
    });
    dwords(phys(&e.status), 1)[ring::SEQNO_INDEX as usize] = e.submitted;
    e.lost = (stored, e.submitted);
    e.resets += 1;
    e.progress = (e.submitted, crate::time::uptime_us());
    if ok {
        warn!(
            "gpu: blitter reset; batches {} to {} lost",
            stored.wrapping_add(1),
            e.submitted
        );
    } else {
        e.dead = true;
        warn!("gpu: blitter reset failed; the engine takes no more work");
    }
}

/// Lets go of the client: stops the engine if it still works for it, points
/// its pages back at the scratch page, and returns its buffers.
fn release_client(e: &mut Engine) -> Vec<Mapping> {
    let Some(mut regs) = framebuffer::mapped_bar0() else {
        return Vec::new();
    };
    if !idle(e, &mut regs) {
        reset(e, &mut regs);
    }
    let maps = core::mem::take(&mut e.maps);
    for m in &maps {
        set_pages(e, m.address, m.pages, None);
    }
    maps
}

/// Points the PPGTT's pages at `phys` onwards, or at the scratch page.
fn set_pages(e: &mut Engine, address: u32, pages: u32, phys: Option<u64>) {
    for i in 0..pages {
        let (dir, entry) = ppgtt::split(address + i * FRAME_SIZE as u32);
        let Some(table) = &e.tables[dir as usize] else {
            continue;
        };
        let value = phys
            .and_then(|p| gtt::encode(p + i as u64 * FRAME_SIZE, Cache::Llc))
            .unwrap_or(e.scratch_pte);
        let t = dwords(self::phys(table), 1);
        t[entry as usize] = value;
        flush_lines(&t[entry as usize], 4);
    }
}

/// `gpu_map`: `dma` at `address` in the client's PPGTT.
pub fn map(dma: ObjectRef, address: u64) -> Result<(), Error> {
    let (base, len) = match &dma.kind {
        ObjectKind::Dma(d) => (d.region.phys, d.region.len),
        _ => return Err(Error::Invalid),
    };
    use aricode_abi::gpu::{FIRST, PAGE, SIZE};
    if !address.is_multiple_of(PAGE)
        || address < FIRST
        || address.saturating_add(len) > SIZE
        || len == 0
    {
        return Err(Error::Invalid);
    }
    let (address, pages) = (address as u32, len.div_ceil(PAGE) as u32);
    let (mut e, _) = take()?;
    if e.dead {
        return Err(Error::Hung);
    }
    claim()?;
    let end = address + pages * PAGE as u32;
    if e.maps
        .iter()
        .any(|m| address < m.address + m.pages * PAGE as u32 && m.address < end)
    {
        return Err(Error::Invalid);
    }
    // Every table first, so a failure leaves nothing half mapped.
    let mut new_dirs = false;
    for dir in ppgtt::split(address).0..=ppgtt::split(end - 1).0 {
        if e.tables[dir as usize].is_none() {
            let table = crate::hw::dma_alloc(FRAME_SIZE, None).map_err(|_| Error::Limit)?;
            let t = dwords(phys(&table), 1);
            t.fill(e.scratch_pte);
            flush_lines(t.as_ptr(), t.len() * 4);
            e.tables[dir as usize] = Some(table);
            new_dirs = true;
        }
    }
    let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
    if new_dirs {
        for dir in ppgtt::split(address).0..=ppgtt::split(end - 1).0 {
            let table = e.tables[dir as usize]
                .as_ref()
                .map(phys)
                .expect("allocated above");
            let pde = ppgtt::directory_entry(table).ok_or(Error::Limit)?;
            ppgtt::set_directory(&mut regs, &e.layout, dir, pde);
        }
        ppgtt::flush_directory(&mut regs);
    }
    set_pages(&mut e, address, pages, Some(base));
    // The next request invalidates the GPU's TLB before its batch.
    e.maps.push(Mapping {
        address,
        pages,
        _dma: dma,
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
    let mut regs = framebuffer::mapped_bar0().ok_or(Error::NotFound)?;
    if !e.dead && !idle(&e, &mut regs) {
        return Err(Error::Busy);
    }
    let m = e.maps.remove(i);
    set_pages(&mut e, m.address, m.pages, None);
    drop(e);
    drop(m);
    Ok(())
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
    let stored = dwords(phys(&e.status), 1)[ring::SEQNO_INDEX as usize];
    if e.copy_seq[slot] != 0 && !ring::passed(stored, e.copy_seq[slot]) {
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
    let mut awake = e.fw.hold(&mut regs, &Clock).map_err(|_| Error::Busy)?;
    let head = awake.read(regs::bcs::HEAD);
    let idle_before = head & regs::bcs::HEAD_ADDR == e.ring.tail();
    let seqno = e.next;
    let copy = &mut dwords(phys(&e.copies), COPIES as u32)[slot * 1024..(slot + 1) * 1024];
    copy[..used].copy_from_slice(&words[..used]);
    let tail = e
        .ring
        .request(dwords(phys(&e.ring_mem), RING_PAGES), head, at, seqno)
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
    ring::submit(&mut awake, tail);
    if idle_before {
        e.progress = (stored, crate::time::uptime_us());
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
            let stored = dwords(phys(&e.status), 1)[ring::SEQNO_INDEX as usize];
            if ring::passed(stored, seqno) {
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
            if stored != e.progress.0 {
                e.progress = (stored, now);
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

/// A process ended: if it was the client, its mappings go at the next call.
/// Called where nothing may wait (the scheduler's reaping).
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

/// `system_info(GPU)`.
pub fn report() -> GpuReport {
    let (state, refusal) = match GPU.lock().status {
        Status::Absent => (GPU_ABSENT, 0),
        Status::Off => (GPU_OFF, 0),
        Status::Refused(r) => (GPU_REFUSED, r as u8),
        Status::Ready => (GPU_READY, 0),
    };
    let mut r = GpuReport {
        state,
        refusal,
        ..GpuReport::default()
    };
    if state != GPU_READY {
        return r;
    }
    let Ok((e, client)) = take() else {
        return r;
    };
    let mhz = |step: u8| (step as u32 * power::STEP_MHZ) as u16;
    r.client = client.unwrap_or(0);
    r.mhz_lowest = mhz(e.limits.lowest);
    r.mhz_highest = mhz(e.limits.highest);
    r.requests = e.requests;
    r.resets = e.resets;
    r.canary = dwords(phys(&e.status), 1)[CANARY_INDEX];
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
