//! Processes: their own address space (kernel half shared), a main thread
//! that enters ring 3 and an exit status.

use crate::arch::x86_64::usermode;
use crate::memory::{phys, pmm, vmm};
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
    /// Next free address for MMIO/DMA mappings.
    mmio_next: u64,
    /// Virtual ranges pointing at memory not owned by the process (MMIO, DMA):
    /// unmapped without freeing when the address space is destroyed.
    foreign: Vec<(u64, u64)>,
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

/// Extra capabilities and the `_start` argument.
pub fn spawn_with_handles_and_arg(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
    arg: u64,
    dma_quota: u64,
) -> Result<Pid, &'static str> {
    spawn_full(name, elf_bytes, extra, arg, dma_quota)
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
    spawn_full(name, elf_bytes, extra, 0, 0)
}

fn spawn_full(
    name: &str,
    elf_bytes: &[u8],
    extra: Vec<(crate::object::ObjectRef, aricode_handles::Rights)>,
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
            mmio_next: USER_MMIO_BASE,
            foreign: Vec::new(),
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
pub fn release_address_space(pid: u32) {
    let taken = PROCESSES.lock().get_mut(&Pid(pid)).and_then(|p| {
        p.mapper
            .take()
            .map(|m| (m, core::mem::take(&mut p.foreign)))
    });
    if let Some((mut m, foreign)) = taken {
        // Foreign memory (MMIO, DMA): unmapped without freeing; its owners
        // are the device or the DMA object.
        for (virt, len) in foreign {
            for page in (virt..virt + len).step_by(PAGE_SIZE as usize) {
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
    }
    crate::iommu::release(pid);
    // Fully reaped: now it can be removed from the table.
    let waiters = PROCESSES.lock().get_mut(&Pid(pid)).map(|p| {
        p.reaped = true;
        p.exit_wait.clone()
    });
    if let Some(w) = waiters {
        w.wake_all();
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
/// (uncached) or a DMA buffer (cached). Returns the virtual address.
pub fn map_foreign(phys: u64, len: u64, cacheable: bool) -> Result<u64, aricode_abi::Error> {
    use aricode_x86_paging::{CACHE_DISABLE, WRITE_THROUGH};
    let pid = current_pid().ok_or(aricode_abi::Error::Fault)?;
    let mut procs = PROCESSES.lock();
    let p = procs.get_mut(&pid).ok_or(aricode_abi::Error::Fault)?;
    let m = p.mapper.as_mut().ok_or(aricode_abi::Error::Fault)?;
    let offset = phys & (PAGE_SIZE - 1);
    let start = phys - offset;
    let span = (offset + len).next_multiple_of(PAGE_SIZE);
    let virt = p.mmio_next;
    // The MMIO/DMA area is above the user stack (8 MiB) and grows upwards
    // to the end of user space.
    if virt
        .checked_add(span)
        .is_none_or(|end| end > USER_SPACE_END)
    {
        return Err(aricode_abi::Error::Limit);
    }
    let mut flags = USER | WRITABLE | NO_EXECUTE;
    if !cacheable {
        flags |= CACHE_DISABLE | WRITE_THROUGH;
    }
    m.map_range_4k(virt, start, span, flags)
        .map_err(|_| aricode_abi::Error::Limit)?;
    p.mmio_next += span;
    p.foreign.push((virt, span));
    Ok(virt + offset)
}
