// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Processes: their own address space (kernel half shared), a main thread
//! that enters ring 3 and an exit status.

use crate::arch::x86_64::usermode;
use crate::memory::{phys, pmm, vmm};
use crate::object::Cap;
use crate::sync::IrqLock;
use crate::task::scheduler;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use aricode_abi::USER_SPACE_END;
use aricode_pmm::Frame;
use aricode_x86_paging::{Mapper, OffsetMap, NO_EXECUTE, PAGE_SIZE, USER, WRITABLE};
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(pub u32);

/// Minimum base of user code (ELFs are linked at 0x40_0000) and stack.
pub const USER_CODE_BASE: u64 = 0x40_0000;

/// `true` if `addr` falls in the guard page below the user stack.
pub fn in_user_stack_guard(addr: u64) -> bool {
    let bottom = USER_STACK_TOP - USER_STACK_SIZE;
    (bottom - PAGE_SIZE..bottom).contains(&addr)
}
pub const USER_STACK_TOP: u64 = 0x80_0000;
pub const USER_STACK_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(u64),
    /// Killed by the kernel: exception in ring 3.
    Killed(&'static str),
}

pub struct Process {
    pub name: String,
    pub handles: crate::object::Handles,
    /// Argument `_start` receives in `rdi`.
    pub arg: u64,
    /// Wakes whoever waits for the exit.
    pub exit_wait: alloc::sync::Arc<crate::task::wait::WaitQueue>,
    /// Addresses for MMIO/DMA mappings (`0x1000_0000` up to the end of user
    /// space); ranges given back by `mmio_unmap` are reused, so repeated
    /// map/unmap cycles keep using the same page tables (ADR 0038).
    mmio_ranges: aricode_vranges::VirtRanges,
    /// Virtual ranges pointing at memory not owned by the process (MMIO,
    /// DMA): unmapped without freeing when the address space is destroyed
    /// or on `mmio_unmap`.
    foreign: Vec<Foreign>,
    /// Bytes of DMA buffers mapped right now (each mapping keeps its buffer
    /// alive, ADR 0038); bounded by `dma_quota` when there is one.
    dma_mapped: u64,
    /// `None` once the address space has been freed.
    mapper: Option<Mapper<vmm::PmmFrames, OffsetMap>>,
    pub entry: u64,
    pub status: Option<ExitStatus>,
    /// Limit of DMA memory (bytes) the process can allocate in total;
    /// 0 = no limit. Set by its creator (`spawn_limits`).
    pub dma_quota: u64,
    pub dma_used: u64,
    /// Console output waiting for a complete line (Phase 32): with several
    /// CPUs, two processes printing a line in several pieces would
    /// interleave; the kernel emits each line whole.
    console_line: Vec<u8>,
    /// The scheduler has reaped its thread and freed its address space and
    /// DMA domain: only then can `wait` remove the process (audit 3eb4965,
    /// finding 1: with several CPUs the thread state is not enough).
    reaped: bool,
    /// Main thread, to wake it if it has to be killed.
    main_tid: aricode_sched::Tid,
    /// Pending `process_kill`: the thread terminates on returning to ring 0 or
    /// on waking from a wait (audit 6ee19fb, finding 4).
    kill_requested: bool,
}

/// A foreign mapping. A DMA buffer's mapping holds a reference to the
/// buffer, so its frames cannot go back to the PMM while any process still
/// maps them, whoever closed which handle (incident 0004, ADR 0038). Device
/// memory (MMIO) needs none: its frames never belong to the PMM.
struct Foreign {
    virt: u64,
    phys: u64,
    span: u64,
    dma: Option<crate::object::ObjectRef>,
}

/// Reason given when a process is killed at another's request.
pub const KILLED_BY_SUPERVISOR: &str = "stopped by the supervisor";

/// Requests termination of `pid` from another process: flags the request and
/// wakes its thread so it sees it. `false` if the process does not exist or
/// has already finished.
pub fn request_kill(pid: Pid) -> bool {
    let tid = {
        let mut procs = PROCESSES.lock();
        let Some(p) = procs.get_mut(&pid) else {
            return false;
        };
        if p.status.is_some() {
            return true;
        }
        p.kill_requested = true;
        p.main_tid
    };
    let _ = scheduler::wake(tid);
    true
}

/// The current thread's process has a pending termination.
pub fn current_kill_requested() -> bool {
    let Some(pid) = current_pid() else {
        return false;
    };
    PROCESSES
        .lock()
        .get(&pid)
        .is_some_and(|p| p.kill_requested && p.status.is_none())
}

/// Size from which an incomplete line is emitted as is.
const CONSOLE_LINE_MAX: usize = 1024;

static PROCESSES: IrqLock<BTreeMap<Pid, Process>> = IrqLock::new(BTreeMap::new());
static NEXT_PID: IrqLock<u32> = IrqLock::new(1);

/// Creates a process from a static user ELF64.
pub fn spawn(name: &str, elf_bytes: &[u8]) -> Result<Pid, &'static str> {
    spawn_with_handles(name, elf_bytes, Vec::new())
}

/// Extra capabilities (objects the kernel grants) and the `_start` argument.
pub fn spawn_with_handles_and_arg(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    spawn_with_caps(name, elf_bytes, grants(extra), arg, dma_quota)
}

/// Like `spawn_with_handles_and_arg` with capabilities that already exist
/// (taken from the parent's table): they move into the child as they are.
pub fn spawn_with_caps(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(Cap, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    spawn_full(name, elf_bytes, extra, arg, dma_quota)
}

/// Capabilities for objects the kernel grants to a new process.
fn grants(
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
) -> Vec<(Cap, aricode_handles::Rights)> {
    extra
        .into_iter()
        .map(|(obj, rights)| (Cap::new(obj, rights), rights))
        .collect()
}

/// Base of user MMIO/DMA mappings.
const USER_MMIO_BASE: u64 = 0x1000_0000;

/// Like `spawn`, with additional capabilities inserted after the initial
/// three (the first one gets handle `0x0001_0004`, and so on).
pub fn spawn_with_handles(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
) -> Result<Pid, &'static str> {
    spawn_full(name, elf_bytes, grants(extra), 0, 0)
}

fn spawn_full(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(Cap, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    let elf = aricode_elf::Elf::parse(elf_bytes).map_err(|_| "process: invalid ELF")?;
    let (lo, hi) = elf.virt_range();
    if lo < USER_CODE_BASE || hi > USER_STACK_TOP - USER_STACK_SIZE - PAGE_SIZE {
        return Err("process: the ELF does not fit in the user region");
    }
    let pid = {
        let mut n = NEXT_PID.lock();
        let p = Pid(*n);
        *n += 1;
        p
    };
    let mut m =
        Mapper::new(vmm::PmmFrames, OffsetMap(phys::offset())).map_err(|_| "process: pml4")?;
    let kernel_root = vmm::with(|k| k.root_phys());
    // SAFETY: the kernel PML4 is valid and reachable through the physical map.
    unsafe { m.share_kernel_half(kernel_root) };

    for seg in &elf.segments {
        let mut flags = USER;
        if seg.writable() {
            flags |= WRITABLE;
        }
        if !seg.executable() {
            flags |= NO_EXECUTE;
        }
        let (start, end) = seg.page_range();
        let bytes = elf.segment_bytes(seg);
        for page in (start..end).step_by(PAGE_SIZE as usize) {
            let frame = pmm::with(|p| p.allocate()).ok_or("process: out of frames")?;
            let dst = phys::phys_to_virt(frame.addr()) as *mut u8;
            // SAFETY: freshly allocated frame, reachable through the physical map.
            unsafe { core::ptr::write_bytes(dst, 0, PAGE_SIZE as usize) };
            // Part of the file contents that falls in this page.
            let file_start = seg.vaddr.max(page);
            let file_end = (seg.vaddr + seg.filesz).min(page + PAGE_SIZE);
            if file_start < file_end {
                let src =
                    &bytes[(file_start - seg.vaddr) as usize..(file_end - seg.vaddr) as usize];
                // SAFETY: destination within the frame; `src` within the segment.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src.as_ptr(),
                        dst.add((file_start - page) as usize),
                        src.len(),
                    )
                };
            }
            m.map_4k(page, frame.addr(), flags)
                .map_err(|_| "process: map segment")?;
        }
    }
    // Stack: RW, NX, with the lowest page left unmapped as a guard.
    for virt in (USER_STACK_TOP - USER_STACK_SIZE..USER_STACK_TOP).step_by(PAGE_SIZE as usize) {
        let frame = pmm::with(|p| p.allocate()).ok_or("process: out of frames")?;
        // SAFETY: freshly allocated frame.
        unsafe {
            core::ptr::write_bytes(
                phys::phys_to_virt(frame.addr()) as *mut u8,
                0,
                PAGE_SIZE as usize,
            )
        };
        m.map_4k(virt, frame.addr(), USER | WRITABLE | NO_EXECUTE)
            .map_err(|_| "process: map stack")?;
    }

    // The thread is prepared now and only queued once the process is in the
    // table: its first code looks up the entry point and the jump to ring 3.
    let thread = scheduler::prepare_for_process(
        name,
        user_thread_entry,
        pid.0 as usize,
        pid.0,
        m.root_phys(),
    )?;
    let tid = thread.tid;
    let mut handles = crate::object::initial_table(pid, tid);
    for (obj, rights) in extra {
        handles
            .insert(obj, rights)
            .map_err(|_| "process: handle table full")?;
    }
    PROCESSES.lock().insert(
        pid,
        Process {
            name: String::from(name),
            handles,
            arg,
            exit_wait: alloc::sync::Arc::new(crate::task::wait::WaitQueue::new()),
            mmio_ranges: aricode_vranges::VirtRanges::new(USER_MMIO_BASE, USER_SPACE_END),
            foreign: Vec::new(),
            dma_mapped: 0,
            mapper: Some(m),
            entry: elf.entry,
            status: None,
            dma_quota,
            dma_used: 0,
            console_line: Vec::new(),
            reaped: false,
            main_tid: tid,
            kill_requested: false,
        },
    );
    scheduler::start_thread(thread);
    crate::events::record(
        aricode_abi::sysinfo::EventKind::ProcessCreated,
        pid.0,
        0,
        name,
    );
    info!(
        "user: process {} '{name}' created ({} segments, {} KiB, entry {:#x}, thread {tid:?})",
        pid.0,
        elf.segments.len(),
        (hi - lo) / 1024,
        elf.entry
    );
    Ok(pid)
}

/// First code of the main thread, with the process address space already
/// active: sets the thread's kernel stack and jumps to ring 3.
extern "C" fn user_thread_entry(pid: usize) {
    let pid = Pid(pid as u32);
    let (entry, arg) = {
        let procs = PROCESSES.lock();
        let p = procs.get(&pid).expect("process");
        (p.entry, p.arg)
    };
    // SAFETY: code and stack mapped USER in `spawn`; the kernel stack was set
    // by the scheduler when switching to this thread.
    unsafe { usermode::enter(entry, USER_STACK_TOP, arg) }
}

/// Process of the current thread, if it is a user thread.
pub fn current_pid() -> Option<Pid> {
    scheduler::current_process().map(Pid)
}

/// Appends `text` to the current process's pending line and emits to the
/// console everything that already forms complete lines (or everything, if
/// the pending line exceeds `CONSOLE_LINE_MAX`).
pub fn console_write_current(text: &str) {
    let Some(pid) = current_pid() else {
        crate::print!("{text}");
        return;
    };
    let ready: Vec<u8> = {
        let mut procs = PROCESSES.lock();
        let Some(p) = procs.get_mut(&pid) else {
            drop(procs);
            crate::print!("{text}");
            return;
        };
        p.console_line.extend_from_slice(text.as_bytes());
        match p.console_line.iter().rposition(|&b| b == b'\n') {
            Some(i) => {
                let rest = p.console_line.split_off(i + 1);
                core::mem::replace(&mut p.console_line, rest)
            }
            None if p.console_line.len() >= CONSOLE_LINE_MAX => {
                core::mem::take(&mut p.console_line)
            }
            None => Vec::new(),
        }
    };
    if !ready.is_empty() {
        crate::print!("{}", String::from_utf8_lossy(&ready));
    }
}

/// Emits the pending incomplete line of `pid` (prompt before reading the
/// console, process exit).
pub fn console_flush(pid: Pid) {
    let pending = PROCESSES
        .lock()
        .get_mut(&pid)
        .map(|p| core::mem::take(&mut p.console_line))
        .unwrap_or_default();
    if !pending.is_empty() {
        crate::print!("{}", String::from_utf8_lossy(&pending));
    }
}

fn finish(pid: Pid, status: ExitStatus) -> ! {
    console_flush(pid);
    let (name, waiters) = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).expect("process in the table");
        p.status = Some(status);
        (p.name.clone(), p.exit_wait.clone())
    };
    waiters.wake_all();
    match status {
        ExitStatus::Exited(code) => {
            crate::events::record(
                aricode_abi::sysinfo::EventKind::ProcessExited,
                pid.0,
                code,
                &name,
            );
            info!("user: process {} '{name}' exited with code {code}", pid.0)
        }
        ExitStatus::Killed(why) => {
            crate::events::record(
                aricode_abi::sysinfo::EventKind::ProcessKilled,
                pid.0,
                0,
                why,
            );
            warn!("user: process {} '{name}' killed: {why}", pid.0)
        }
    }
    scheduler::exit()
}

/// `process_exit` from a syscall.
pub fn exit_current(code: u64) -> ! {
    let pid = current_pid().expect("exit from a user thread");
    finish(pid, ExitStatus::Exited(code))
}

/// Exception in ring 3: kills the process, never the kernel.
pub fn kill_current(why: &'static str) -> ! {
    let pid = current_pid().expect("user fault on a user thread");
    finish(pid, ExitStatus::Killed(why))
}

/// Frees the address space and DMA domain of a finished process and marks
/// it reaped, waking whoever waits for it. Called by the scheduler when
/// reaping its thread, always from another thread and with another CR3.
///
/// Order (incident 0004): the devices this process drove stop bus
/// mastering first; only then do its mappings, its DMA domain and its
/// capabilities go, freeing memory a device could otherwise still write;
/// a device left unclaimed is reset last.
pub fn release_address_space(pid: u32) {
    // The capabilities are taken out now, before anything is freed, so the
    // devices they claim can be stopped first; they are dropped below.
    let handles = PROCESSES
        .lock()
        .get_mut(&Pid(pid))
        .map(|p| core::mem::replace(&mut p.handles, aricode_handles::HandleTable::new(0)));
    let stopped = handles
        .as_ref()
        .map(|h| crate::pci::stop_claims_of(pid, h))
        .unwrap_or_default();
    let taken = PROCESSES.lock().get_mut(&Pid(pid)).and_then(|p| {
        p.dma_mapped = 0;
        p.mapper
            .take()
            .map(|m| (m, core::mem::take(&mut p.foreign)))
    });
    if stopped.quarantine {
        quarantine_dma(pid, handles.as_ref(), taken.as_ref().map(|t| &t.1[..]));
    }
    if let Some((mut m, foreign)) = taken {
        // Foreign memory (MMIO, DMA): unmapped without freeing; its owners
        // are the device or the DMA object. A DMA buffer whose last
        // reference was this mapping is freed when the entry is dropped,
        // once the page tables no longer point at it.
        for f in &foreign {
            for page in (f.virt..f.virt + f.span).step_by(PAGE_SIZE as usize) {
                let _ = m.unmap_4k(page);
            }
        }
        let mut frames = Vec::new();
        m.free_user_half(|f| frames.push(f));
        frames.push(m.into_root_frame());
        let n = frames.len();
        pmm::with(|p| {
            for f in frames {
                p.free(Frame::from_addr(f)).expect("process frame");
            }
        });
        log::debug!("user: address space of process {pid} freed ({n} frames)");
        for f in foreign {
            report_last_reference(pid, &f);
        }
    }
    crate::iommu::release(pid);
    // The capabilities go now (not when a parent gets around to `wait`), so
    // the endpoints this process served or was to answer on close at once
    // (ADR 0030). They are dropped with no lock held and their endpoints
    // settled before the process counts as reaped: whoever `wait`s for it
    // finds its capabilities already released (audit robustness-1, M2).
    if let Some(handles) = handles {
        release_capabilities(pid, handles);
    }
    crate::pci::reset_released(pid, &stopped.functions);
    let waiters = PROCESSES.lock().get_mut(&Pid(pid)).map(|p| {
        p.reaped = true;
        p.exit_wait.clone()
    });
    if let Some(w) = waiters {
        w.wake_all();
    }
}

/// A device of dying process `pid` did not stop mastering the bus: every
/// DMA buffer the process holds or maps is kept for good instead of going
/// back to the PMM (incident 0004), and reported.
fn quarantine_dma(pid: u32, handles: Option<&crate::object::Handles>, foreign: Option<&[Foreign]>) {
    use crate::object::ObjectKind;
    let mut held: Vec<crate::object::ObjectRef> = Vec::new();
    let caps = handles
        .into_iter()
        .flat_map(|h| h.iter().map(|(c, _)| c.object()));
    let mapped = foreign.into_iter().flatten().filter_map(|f| f.dma.as_ref());
    for obj in caps.chain(mapped) {
        if matches!(obj.kind, ObjectKind::Dma(_))
            && !held.iter().any(|o| alloc::sync::Arc::ptr_eq(o, obj))
        {
            held.push(obj.clone());
        }
    }
    let count = held.len();
    let bytes: u64 = held.into_iter().map(crate::hw::quarantine).sum();
    warn!(
        "user: process {pid}: {count} DMA buffer(s), {} KiB, quarantined (never reused): a device it drove still masters the bus",
        bytes / 1024
    );
}

/// A foreign mapping that is about to be dropped and was the last
/// reference to its DMA buffer: the buffer outlived every handle to it
/// because this process kept it mapped (ADR 0038). Logged once per
/// mapping, so it is bounded by the mappings a process can have.
fn report_last_reference(pid: u32, f: &Foreign) {
    if let Some(obj) = &f.dma {
        if alloc::sync::Arc::strong_count(obj) == 1 {
            info!(
                "user: process {pid} dropped the last mapping of a DMA buffer it had no handle to ({} KiB at {:#x}): its frames return to the PMM now",
                f.span / 1024,
                f.phys
            );
        }
    }
}

/// Drops a finished process's capabilities and settles the endpoints they
/// close. Runs in `reap()`, with interrupts disabled like the address
/// space release before it (audit robustness-1, M3). Bounded: at most
/// `HANDLE_LIMIT` (256) capabilities, each O(1) except a DMA buffer (its
/// frames back to the PMM) and an endpoint that closes (its queue: at most
/// `IPC_QUEUE_CAPACITY` messages of `IPC_MAX_HANDLES` capabilities, which
/// may close further endpoints in turn). Measured, and reported when it
/// exceeds `SLOW_RELEASE_US`, so a slow case shows up in the log and in
/// `diag` instead of as silent interrupt latency.
fn release_capabilities(pid: u32, handles: crate::object::Handles) {
    const SLOW_RELEASE_US: u64 = 1_000;
    let count = handles.len();
    let t0 = crate::time::uptime_us();
    drop(handles);
    crate::ipc::settle_closed();
    let took = crate::time::uptime_us().saturating_sub(t0);
    if took > SLOW_RELEASE_US {
        warn!(
            "user: releasing {count} capabilities of process {pid} took {took} us with interrupts off"
        );
        crate::events::record(
            aricode_abi::sysinfo::EventKind::Fault,
            pid,
            took,
            "slow capability release",
        );
    } else {
        log::debug!("user: {count} capabilities of process {pid} released in {took} us");
    }
}

/// Waits for the process to finish and be reaped (address space and domain
/// freed), and returns its status, removing it from the table. `None` if the
/// deadline expires or another `wait` removed it first.
pub fn wait_until(pid: Pid, deadline: Option<u64>) -> Option<ExitStatus> {
    let waiters = PROCESSES.lock().get(&pid)?.exit_wait.clone();
    loop {
        {
            let mut procs = PROCESSES.lock();
            let p = procs.get(&pid)?;
            if p.reaped {
                let status = p.status;
                procs.remove(&pid);
                return status;
            }
        }
        let timed_out = waiters.wait_until_deadline(
            || PROCESSES.lock().get(&pid).is_none_or(|p| p.reaped),
            deadline,
        );
        if timed_out {
            return None;
        }
    }
}

/// Waits without a deadline.
pub fn wait(pid: Pid) -> ExitStatus {
    wait_until(pid, None).expect("no deadline means no timeout")
}

/// Self-test (`test=process_reap`, audit 3eb4965, finding 1): creating and
/// waiting for processes in batches, on every CPU, loses no frames: `wait`
/// does not remove a process until its address space is back in the PMM.
pub fn reap_selftest() {
    use crate::memory::{heap, pmm};
    use crate::user::programs;

    let code = programs::by_name("hello").expect("hello in the initrd");
    // Warm-up: let the heap do whatever growing it will before measuring.
    for _ in 0..4 {
        let pid = spawn("hello", code).expect("spawn");
        assert_eq!(wait(pid), ExitStatus::Exited(42));
    }
    let free_before = pmm::stats().free_frames;
    let heap_before = heap::mapped_bytes();
    const ROUNDS: usize = 30;
    const BATCH: usize = 6;
    for _ in 0..ROUNDS {
        let pids: Vec<Pid> = (0..BATCH)
            .map(|_| spawn("hello", code).expect("spawn"))
            .collect();
        for pid in pids {
            assert_eq!(wait(pid), ExitStatus::Exited(42));
            assert!(!exists(pid), "wait must remove process {pid:?}");
        }
    }
    assert!(
        PROCESSES.lock().is_empty(),
        "no process must remain in the table"
    );
    let heap_grown = (heap::mapped_bytes() - heap_before) / PAGE_SIZE;
    let free_after = pmm::stats().free_frames;
    let lost = free_before as i64 - free_after as i64 - heap_grown as i64;
    assert!(
        lost <= 8,
        "frames lost after {} processes: {lost} (heap +{heap_grown} pages)",
        ROUNDS * BATCH
    );
    info!(
        "process reap selftest ok ({} processes created and waited for on {} CPU(s), {lost} frames difference, heap +{heap_grown} pages)",
        ROUNDS * BATCH,
        crate::cpu::online_count()
    );
}

/// Physical pages a process maps through its foreign (MMIO/DMA) entries,
/// read back from its page tables (`test=dma_lifetime`).
fn foreign_pages(pid: Pid) -> Vec<u64> {
    let procs = PROCESSES.lock();
    let Some(p) = procs.get(&pid) else {
        return Vec::new();
    };
    let Some(m) = p.mapper.as_ref() else {
        return Vec::new();
    };
    let mut pages = Vec::new();
    for f in &p.foreign {
        for page in (f.virt..f.virt + f.span).step_by(PAGE_SIZE as usize) {
            if let Some((phys, _)) = m.translate(page) {
                pages.push(phys & !(PAGE_SIZE - 1));
            }
        }
    }
    pages
}

/// Words `dma_holder` and the self-test exchange through the buffer (must
/// match `user/testprogs/src/bin/dma_holder.rs`): the first once it has
/// mapped the buffer and closed its handle, the second to ask it to unmap.
const DMA_HOLDER_MAGIC: u64 = 0xD3A0_11FE_71E0_0004;
const DMA_HOLDER_UNMAP: u64 = 0x0A3A_9000_0000_0038;

/// Self-test (`test=dma_lifetime`, incident 0004, ADR 0038): a server keeps
/// a CPU mapping of a client's DMA buffer after closing its handle (as the
/// compositor does with window buffers). When the client drops the buffer
/// too, no frame the server still maps may be free in the PMM; the frames
/// return once the mapping goes, whether the server is reaped or unmaps
/// the buffer itself. With a quota, the DMA a process keeps mapped is
/// bounded by it.
pub fn dma_lifetime_selftest() {
    const LEN: u64 = 16 * 1024;
    let frames_free = |phys: u64| {
        pmm::with(|p| {
            (0..LEN / PAGE_SIZE).all(|i| p.is_free(Frame::from_addr(phys + i * PAGE_SIZE)))
        })
    };

    // 1. The mapping outlives every handle; reaping the server frees it.
    let (pid, phys) = dma_holder_with_mapping(LEN, 0);
    if let Err(free) = check_foreign_frames_allocated(pid, phys) {
        panic!(
            "dma lifetime: process {} still maps {free} DMA frame(s) the PMM considers free (incident 0004)",
            pid.0
        );
    }
    assert!(request_kill(pid), "dma_holder must still exist");
    assert_eq!(wait(pid), ExitStatus::Killed(KILLED_BY_SUPERVISOR));
    assert!(
        frames_free(phys),
        "dma lifetime: the frames must return to the PMM once the server is reaped"
    );

    // 2. The server unmaps the buffer itself (`mmio_unmap`): the frames
    //    return at once, with the server still alive.
    let (pid, phys) = dma_holder_with_mapping(LEN, 1);
    if let Err(free) = check_foreign_frames_allocated(pid, phys) {
        panic!(
            "dma lifetime: process {} still maps {free} DMA frame(s) the PMM considers free (incident 0004)",
            pid.0
        );
    }
    // SAFETY: the frames are alive (the server maps them) and reachable
    // through the physical map.
    unsafe {
        (phys::phys_to_virt(phys) as *mut u64)
            .add(1)
            .write_volatile(DMA_HOLDER_UNMAP)
    };
    let mut waited_ms = 0;
    while !frames_free(phys) {
        assert!(
            waited_ms < 10_000 && !exited(pid),
            "dma lifetime: mmio_unmap must free the buffer ({:?})",
            PROCESSES.lock().get(&pid).and_then(|p| p.status)
        );
        scheduler::sleep_ms(10);
        waited_ms += 10;
    }
    assert!(
        foreign_pages(pid).is_empty(),
        "dma lifetime: the unmapped range must be gone from the page tables"
    );
    assert_eq!(
        PROCESSES.lock().get(&pid).map(|p| p.mmio_ranges.top()),
        Some(USER_MMIO_BASE),
        "dma lifetime: the unmapped addresses are given back for reuse"
    );
    assert!(
        request_kill(pid),
        "dma_holder must still exist after unmapping"
    );
    assert_eq!(wait(pid), ExitStatus::Killed(KILLED_BY_SUPERVISOR));

    // 3. A process with a 4 KiB quota cannot keep a 16 KiB buffer mapped.
    let dma = crate::hw::dma_alloc(LEN).expect("DMA buffer");
    let pid = spawn_with_handles_and_arg(
        "dma_holder",
        crate::user::programs::by_name("dma_holder").expect("dma_holder in the initrd"),
        alloc::vec![(
            dma,
            aricode_handles::Rights::READ.union(aricode_handles::Rights::WRITE)
        )],
        0,
        4096,
    )
    .expect("dma_holder");
    assert_eq!(
        wait(pid),
        ExitStatus::Exited(1),
        "dma lifetime: mmio_map beyond the quota must be refused"
    );
    info!(
        "dma lifetime selftest ok ({} frames kept by a mapping after the last handle, freed on reap and on mmio_unmap; mapping beyond the quota refused)",
        LEN / PAGE_SIZE
    );
}

/// Starts `dma_holder` with a new buffer of `len` bytes and argument
/// `arg`, waits until it has mapped the buffer and closed its handle, and
/// drops the kernel's reference (the client's last handle). Returns the
/// server and the buffer's physical address.
fn dma_holder_with_mapping(len: u64, arg: u64) -> (Pid, u64) {
    use crate::object::ObjectKind;
    use aricode_handles::Rights;

    let dma = crate::hw::dma_alloc(len).expect("DMA buffer");
    let phys = match &dma.kind {
        ObjectKind::Dma(d) => d.region.phys,
        _ => unreachable!(),
    };
    let pid = spawn_with_handles_and_arg(
        "dma_holder",
        crate::user::programs::by_name("dma_holder").expect("dma_holder in the initrd"),
        alloc::vec![(dma.clone(), Rights::READ.union(Rights::WRITE))],
        arg,
        0,
    )
    .expect("dma_holder");
    let word = phys::phys_to_virt(phys) as *const u64;
    let mut waited_ms = 0;
    // SAFETY: the buffer is alive (the kernel holds a reference) and
    // reachable through the physical map.
    while unsafe { word.read_volatile() } != DMA_HOLDER_MAGIC {
        assert!(
            waited_ms < 10_000 && !exited(pid),
            "dma lifetime: dma_holder did not map the buffer ({:?})",
            PROCESSES.lock().get(&pid).and_then(|p| p.status)
        );
        scheduler::sleep_ms(10);
        waited_ms += 10;
    }
    drop(dma);
    (pid, phys)
}

/// The debug check of incident 0004: every frame `pid` maps through its
/// foreign entries must still be allocated in the PMM, and the buffer at
/// `phys` must be among them. `Err` holds how many are free.
fn check_foreign_frames_allocated(pid: Pid, phys: u64) -> Result<(), usize> {
    let pages = foreign_pages(pid);
    assert!(
        pages.contains(&phys),
        "dma lifetime: the server must still map the buffer"
    );
    let free = pmm::with(|p| {
        pages
            .iter()
            .filter(|&&a| p.is_free(Frame::from_addr(a)))
            .count()
    });
    if free == 0 {
        Ok(())
    } else {
        Err(free)
    }
}

fn exited(pid: Pid) -> bool {
    PROCESSES
        .lock()
        .get(&pid)
        .is_none_or(|p| p.status.is_some())
}

/// Charges `len` bytes of DMA to the current process's quota; `false` if it
/// would exceed it (the quota is cumulative: allocations are not refunded).
pub fn charge_dma(len: u64) -> bool {
    let Some(pid) = current_pid() else {
        return true;
    };
    let mut procs = PROCESSES.lock();
    let Some(p) = procs.get_mut(&pid) else {
        return true;
    };
    if p.dma_quota != 0 && p.dma_used.saturating_add(len) > p.dma_quota {
        return false;
    }
    p.dma_used += len;
    true
}

/// Snapshot of the process table for `system_info`.
pub fn snapshot() -> Vec<aricode_abi::sysinfo::ProcessEntry> {
    use aricode_abi::sysinfo::{ProcessEntry, ProcessState};
    PROCESSES
        .lock()
        .iter()
        .map(|(pid, p)| {
            let (state, code) = match p.status {
                None => (ProcessState::Running, 0),
                Some(ExitStatus::Exited(c)) => {
                    (ProcessState::Exited, c.min(u16::MAX as u64) as u16)
                }
                Some(ExitStatus::Killed(_)) => (ProcessState::Killed, 0),
            };
            ProcessEntry::new(pid.0, state, code, &p.name)
        })
        .collect()
}

pub fn exists(pid: Pid) -> bool {
    PROCESSES.lock().contains_key(&pid)
}

/// Checks that `[ptr, ptr+len)` is in user space and mapped USER (and
/// WRITABLE if `write`) in the current process.
fn check_user_range(ptr: u64, len: u64, write: bool) -> Result<(), aricode_abi::Error> {
    if !aricode_abi::user_range_ok(ptr, len) {
        return Err(aricode_abi::Error::Fault);
    }
    if len == 0 {
        return Ok(());
    }
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let procs = PROCESSES.lock();
    let m = procs
        .get(&pid)
        .and_then(|p| p.mapper.as_ref())
        .ok_or(aricode_abi::Error::Fault)?;
    let needed = USER | if write { WRITABLE } else { 0 };
    let mut page = ptr & !(PAGE_SIZE - 1);
    while page < ptr + len {
        match m.translate(page) {
            Some((_, flags)) if flags & needed == needed => {}
            _ => return Err(aricode_abi::Error::Fault),
        }
        page += PAGE_SIZE;
    }
    debug_assert!(ptr + len <= USER_SPACE_END);
    Ok(())
}

/// Copies `[ptr, ptr+len)` from the current process's address space,
/// checking that each page is mapped USER. The process space is the active one.
pub fn copy_from_user(ptr: u64, len: u64) -> Result<Vec<u8>, aricode_abi::Error> {
    check_user_range(ptr, len, false)?;
    // SAFETY: range validated page by page in the active address space.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    Ok(bytes.to_vec())
}

/// Writes `bytes` to `[ptr, ptr+len)` of the current process, checking that
/// each page is mapped USER and WRITABLE.
pub fn copy_to_user(ptr: u64, bytes: &[u8]) -> Result<(), aricode_abi::Error> {
    check_user_range(ptr, bytes.len() as u64, true)?;
    // SAFETY: range validated as writable in the active address space.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    Ok(())
}

/// Access to the current process's handle table.
pub fn with_current_handles<R>(f: impl FnOnce(&mut crate::object::Handles) -> R) -> Option<R> {
    let pid = current_pid()?;
    let mut procs = PROCESSES.lock();
    procs.get_mut(&pid).map(|p| f(&mut p.handles))
}

/// Maps `[phys, phys+len)` into the current process as device memory
/// (uncached) or a DMA buffer (cached). Returns the virtual address and
/// whether this call created the mapping (`false`: it already existed). A
/// DMA buffer's mapping keeps `dma` (the buffer) alive until it is
/// unmapped (ADR 0038).
pub fn map_foreign(
    phys: u64,
    len: u64,
    cacheable: bool,
    dma: Option<crate::object::ObjectRef>,
) -> Result<(u64, bool), aricode_abi::Error> {
    use aricode_x86_paging::{CACHE_DISABLE, WRITE_THROUGH};
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let mut procs = PROCESSES.lock();
    let p = procs.get_mut(&pid).ok_or(aricode_abi::Error::Fault)?;
    let offset = phys & (PAGE_SIZE - 1);
    let start = phys - offset;
    let span = (offset + len).next_multiple_of(PAGE_SIZE);
    // Mapping the same region again (a server mapping a client's buffer on
    // every request) reuses the existing mapping instead of leaking one. A
    // mapped DMA buffer cannot be freed, so the same physical range is
    // still the same buffer.
    if let Some(f) = p.foreign.iter().find(|f| f.phys == start && f.span == span) {
        return Ok((f.virt + offset, false));
    }
    // The buffers a process keeps mapped stay allocated on its account:
    // with a quota, no more than the quota (ADR 0038).
    if dma.is_some() && p.dma_quota != 0 && p.dma_mapped.saturating_add(span) > p.dma_quota {
        drop(procs);
        crate::events::record(
            aricode_abi::sysinfo::EventKind::DmaQuota,
            pid.0,
            span,
            "mmio_map",
        );
        return Err(aricode_abi::Error::Limit);
    }
    // The MMIO/DMA area is above the user stack (8 MiB) and reaches the
    // end of user space.
    let virt = p.mmio_ranges.alloc(span).ok_or(aricode_abi::Error::Limit)?;
    let mut flags = USER | WRITABLE | NO_EXECUTE;
    if !cacheable {
        flags |= CACHE_DISABLE | WRITE_THROUGH;
    }
    let Some(m) = p.mapper.as_mut() else {
        p.mmio_ranges.free(virt, span);
        return Err(aricode_abi::Error::Fault);
    };
    // All or nothing: a failure (no frame for a page table) leaves no page
    // of the range mapped.
    if m.map_range_4k(virt, start, span, flags).is_err() {
        p.mmio_ranges.free(virt, span);
        return Err(aricode_abi::Error::Limit);
    }
    if dma.is_some() {
        p.dma_mapped += span;
    }
    p.foreign.push(Foreign {
        virt,
        phys: start,
        span,
        dma,
    });
    Ok((virt + offset, true))
}

/// `mmio_unmap`: removes the current process's foreign mapping that
/// `virt` (an address `mmio_map` returned) starts, invalidates it and only
/// then gives the addresses back and drops its reference to the buffer,
/// which may free it.
pub fn unmap_foreign(virt: u64) -> Result<(), aricode_abi::Error> {
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let f = {
        let mut procs = PROCESSES.lock();
        let p = procs.get_mut(&pid).ok_or(aricode_abi::Error::Fault)?;
        let start = virt & !(PAGE_SIZE - 1);
        let i = p
            .foreign
            .iter()
            .position(|f| f.virt == start)
            .ok_or(aricode_abi::Error::Invalid)?;
        let m = p.mapper.as_mut().ok_or(aricode_abi::Error::Fault)?;
        let f = p.foreign.swap_remove(i);
        for page in (f.virt..f.virt + f.span).step_by(PAGE_SIZE as usize) {
            let _ = m.unmap_4k(page);
        }
        if f.dma.is_some() {
            p.dma_mapped -= f.span;
        }
        f
    };
    vmm::flush_own_user_range(f.virt, f.span);
    // Only now can the addresses be handed out again.
    if let Some(p) = PROCESSES.lock().get_mut(&pid) {
        p.mmio_ranges.free(f.virt, f.span);
    }
    report_last_reference(pid.0, &f);
    Ok(())
}
