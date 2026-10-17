// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Scheduler: thread table + `aricode-sched` policy + context switching.
//!
//! Locking rule (Phase 32): the scheduler lock is taken to decide and stays
//! held across the context switch; it is released by the resuming thread
//! (`force_unlock`), which does not hold the guard. That way no other CPU
//! can pick the outgoing thread before its `rsp` is saved. The switch
//! happens with interrupts disabled; the resuming thread restores its own.
//!
//! Preemption (Phase 10): the timer handler calls `on_tick` after the EOI.
//! A context switch from the handler leaves the thread state on its own
//! stack (interrupt frame + trampoline registers); when it is picked again,
//! it returns through the handler and `iretq` restores its flags. Every
//! kernel lock disables interrupts, so no critical section can be preempted.
//!
//! SMP (Phase 32): a shared ready queue; each CPU has its current thread and
//! its idle thread (`cpu::Cpu`). Only the BSP keeps time and wakes sleepers;
//! when a thread becomes ready and another CPU is idle, that CPU gets an IPI
//! so it takes the thread without waiting for its next tick.

use super::context::switch_context;
use super::thread::Thread;
use crate::arch::x86_64::apic;
use crate::cpu;
use crate::sync::{IrqLock, KernelIrq};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aricode_sched::{RoundRobin, State, Tid};
use aricode_sync::IrqPolicy;
use core::sync::atomic::{AtomicU64, Ordering};
use log::{debug, info};

struct Scheduler {
    policy: RoundRobin,
    threads: BTreeMap<Tid, Box<Thread>>,
}

static SCHED: IrqLock<Option<Scheduler>> = IrqLock::new(None);
static SWITCHES: AtomicU64 = AtomicU64::new(0);
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);

/// CPU quantum: 2 ticks = 20 ms at 100 Hz.
pub const QUANTUM_TICKS: u32 = 2;

pub const MAIN_TID: Tid = Tid(0);
pub const IDLE_TID: Tid = Tid(1);

fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    let mut guard = SCHED.lock();
    f(guard.as_mut().expect("scheduler not initialized"))
}

/// Adopts the boot context as the `main` thread and creates the BSP's idle thread.
pub fn init() {
    let mut sched = Scheduler {
        policy: RoundRobin::new(MAIN_TID, IDLE_TID),
        threads: BTreeMap::new(),
    };
    let kernel_cr3 = crate::memory::vmm::with(|m| m.root_phys());
    sched.threads.insert(
        MAIN_TID,
        Box::new(Thread::adopt_current(MAIN_TID, "main", kernel_cr3)),
    );
    let idle = Thread::new(IDLE_TID, "idle", idle_entry, 0, None, kernel_cr3).expect("idle thread");
    sched.threads.insert(IDLE_TID, Box::new(idle));
    *SCHED.lock() = Some(sched);
    let bsp = cpu::slot(0);
    bsp.set_idle_tid(IDLE_TID.0);
    bsp.set_current(MAIN_TID.0, QUANTUM_TICKS);
    info!(
        "task: main thread adopted, idle created ({} KiB stacks with guard); preemptive round-robin scheduler, quantum {} ms",
        super::stack::STACK_SIZE / 1024,
        QUANTUM_TICKS as u64 * crate::time::MS_PER_TICK
    );
}

/// Creates the idle thread of CPU `idx` (Phase 32), which that AP adopts as
/// its running thread: returns the top of its stack to start it there.
pub fn create_idle_for_cpu(idx: usize) -> Result<u64, &'static str> {
    let kernel_cr3 = crate::memory::vmm::with(|m| m.root_phys());
    let tid = with(|s| s.policy.allocate_tid());
    let stack = super::stack::KernelStack::allocate(tid.0)?;
    let top = stack.top();
    let mut name = alloc::string::String::from("idle-");
    name.push_str(&alloc::format!("{idx}"));
    let thread = Thread::adopt_on_stack(tid, &name, stack, kernel_cr3);
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy
            .add_cpu(idx, tid)
            .map_err(|_| "scheduler: CPU out of order")
    })?;
    cpu::slot(idx).set_idle_tid(tid.0);
    Ok(top)
}

/// AP `idx` starts scheduling: its idle thread is the current one.
pub fn adopt_idle(idx: usize) {
    let c = cpu::slot(idx);
    let idle = Tid(c.idle_tid());
    let top = with(|s| {
        s.threads
            .get(&idle)
            .expect("AP idle thread")
            .kernel_stack_top()
    });
    c.set_current(idle.0, QUANTUM_TICKS);
    crate::arch::x86_64::gdt::set_kernel_stack(top);
}

/// Called from the timer handler (interrupts disabled, EOI already sent). On
/// the BSP it wakes sleepers; on every CPU it preempts if the quantum
/// expired. Does nothing before `init`.
pub fn on_tick(now: u64, bsp: bool, from_user: bool) {
    // A user thread that makes no syscalls (infinite loop) only passes
    // through here: if its process has a pending termination, it ends now,
    // just as on an exception (audit 6ee19fb, finding 4).
    if from_user && crate::user::process::current_kill_requested() {
        crate::user::process::kill_current(crate::user::process::KILLED_BY_SUPERVISOR);
    }
    let woke = if bsp {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else {
            return;
        };
        s.policy.wake_due(now)
    } else {
        0
    };
    if woke > 0 {
        kick_idle();
    }
    let expired = cpu::current().quantum_tick();
    if expired || woke > 0 {
        PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
        switch_with(|p, c| p.preempt(c));
    }
}

/// If another CPU is idle, takes it out of `hlt` so it checks the queue.
fn kick_idle() {
    if let Some(c) = cpu::idle_other_than(cpu::id()) {
        apic::send_ipi(c.lapic_id(), apic::RESCHED_VECTOR);
    }
}

/// Puts the current thread to sleep until tick `until` (or yields if it has passed).
pub fn sleep_until(until: u64) {
    if crate::time::ticks() >= until {
        yield_now();
        return;
    }
    switch_with(|p, c| p.sleep_current(c, until));
}

/// Sleeps at least `ms`. An early wake-up (a spurious `wake` from a queue
/// this thread waited on before) goes back to sleep until the deadline.
pub fn sleep_ms(ms: u64) {
    // One more tick: the current one has partly elapsed, and "at least `ms`"
    // is measured with the microsecond clock, not the tick (stage 5).
    let until = crate::time::ticks() + ms.div_ceil(crate::time::MS_PER_TICK) + 1;
    loop {
        sleep_until(until);
        // An early wake-up from `process_kill` does not go back to sleep: the
        // syscall terminates the process on exit (audit 6ee19fb, 4).
        if crate::time::ticks() >= until || crate::user::process::current_kill_requested() {
            return;
        }
    }
}

/// Blocks the current thread until someone calls `wake`.
pub fn block_current() {
    switch_with(|p, c| p.block_current(c));
}

/// Wakes a blocked or sleeping thread before its deadline (or leaves the
/// wake-up pending if it is still running on another CPU).
pub fn wake(tid: Tid) -> Result<(), aricode_sched::SchedError> {
    let r = with(|s| s.policy.wake(tid));
    if r.is_ok() {
        kick_idle();
    }
    r
}

/// Sets the CPU `tid` may run on (`None` = any).
pub fn set_affinity(tid: Tid, cpu: Option<usize>) -> Result<(), aricode_sched::SchedError> {
    with(|s| s.policy.set_affinity(tid, cpu))
}

#[derive(Debug, Clone, Copy)]
pub struct SchedStats {
    pub switches: u64,
    pub preemptions: u64,
}

pub fn stats() -> SchedStats {
    SchedStats {
        switches: SWITCHES.load(Ordering::Relaxed),
        preemptions: PREEMPTIONS.load(Ordering::Relaxed),
    }
}

extern "C" fn idle_entry(_: usize) {
    idle_loop()
}

/// Idle loop of a CPU: if something is ready for it, it takes it; otherwise
/// it sleeps. `sti; hlt` is atomic: an IPI or tick arriving just before
/// sleeping still wakes the CPU.
pub fn idle_loop() -> ! {
    loop {
        let irq = KernelIrq::save_and_disable();
        let work = with(|s| s.policy.has_ready_for(cpu::id() as usize));
        if work {
            KernelIrq::restore(irq);
            yield_now();
        } else {
            // SAFETY: rdtsc only reads the time stamp counter.
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            // SAFETY: enable interrupts and wait for the next one.
            unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack)) };
            // SAFETY: same as above.
            let t1 = unsafe { core::arch::x86_64::_rdtsc() };
            // The idle thread never migrates: `current()` is its own CPU.
            cpu::current().add_idle_cycles(t1.wrapping_sub(t0));
        }
    }
}

pub fn current_tid() -> Tid {
    Tid(cpu::current_tid())
}

/// Creates a kernel thread ready to run.
pub fn spawn(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
) -> Result<Tid, &'static str> {
    let cr3 = crate::memory::vmm::with(|m| m.root_phys());
    spawn_thread(name, entry, arg, None, cr3, None)
}

/// Creates a kernel thread that will only run on CPU `cpu`.
pub fn spawn_on(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    cpu: usize,
) -> Result<Tid, &'static str> {
    let cr3 = crate::memory::vmm::with(|m| m.root_phys());
    spawn_thread(name, entry, arg, None, cr3, Some(cpu))
}

/// Prepares (without queueing) the main thread of a user process: it starts
/// at `entry` (ring 0, with address space `cr3` active) and from there jumps
/// to ring 3. The caller registers the process and then calls
/// `start_thread`, so the thread never runs before its process exists.
pub fn prepare_for_process(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    pid: u32,
    cr3: u64,
) -> Result<Box<Thread>, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    Ok(Box::new(Thread::new(
        tid,
        name,
        entry,
        arg,
        Some(pid),
        cr3,
    )?))
}

/// Queues a thread prepared with `prepare_for_process`.
pub fn start_thread(thread: Box<Thread>) -> Tid {
    let tid = thread.tid;
    debug!("task: spawn {tid:?} '{}'", thread.name);
    with(|s| {
        s.threads.insert(tid, thread);
        s.policy.add(tid);
    });
    kick_idle();
    tid
}

fn spawn_thread(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    process: Option<u32>,
    cr3: u64,
    affinity: Option<usize>,
) -> Result<Tid, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    let thread = Thread::new(tid, name, entry, arg, process, cr3)?;
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy.add(tid);
        if let Some(c) = affinity {
            s.policy
                .set_affinity(tid, Some(c))
                .map_err(|_| "scheduler: unknown CPU")?;
        }
        Ok::<(), &'static str>(())
    })?;
    kick_idle();
    debug!("task: spawn {tid:?} '{name}'");
    Ok(tid)
}

/// Process of the current thread.
pub fn current_process() -> Option<u32> {
    let me = current_tid();
    with(|s| s.threads.get(&me).and_then(|t| t.process))
}

/// State of a thread, if it exists.
pub fn state(tid: Tid) -> Option<State> {
    with(|s| s.policy.state(tid))
}

/// Yields the CPU to the next ready thread.
pub fn yield_now() {
    switch_with(|p, c| p.yield_current(c));
}

/// Terminates the current thread. Never returns.
pub fn exit() -> ! {
    switch_with(|p, c| Some(p.exit_current(c)));
    unreachable!("a finished thread does not resume");
}

#[no_mangle]
extern "C" fn thread_exit_trampoline() -> ! {
    exit()
}

/// First code of a new thread (from `thread_start`, IF = 0): releases the
/// scheduler lock taken by the CPU that switched to it.
#[no_mangle]
extern "C" fn thread_first_run() {
    // SAFETY: context switch protocol (see the module header).
    unsafe { SCHED.force_unlock() };
    reap();
}

/// Applies `decide` to the policy (for the current CPU) and, if there is a
/// change, switches context with the scheduler lock held.
fn switch_with(decide: impl FnOnce(&mut RoundRobin, usize) -> Option<Tid>) {
    let irq = KernelIrq::save_and_disable();
    let c = cpu::current();
    let mut guard = SCHED.lock();
    let switch = {
        let s = guard.as_mut().expect("scheduler not initialized");
        let prev = s.policy.current(c.index());
        decide(&mut s.policy, c.index()).map(|next| {
            let old = &mut s.threads.get_mut(&prev).expect("current thread").saved_rsp as *mut u64;
            let nt = s.threads.get(&next).expect("next thread");
            (next, old, nt.saved_rsp, nt.kernel_stack_top(), nt.cr3)
        })
    };
    match switch {
        Some((next, old, new, kstack, cr3)) => {
            c.set_current(next.0, QUANTUM_TICKS);
            SWITCHES.fetch_add(1, Ordering::Relaxed);
            // Ring 3 to ring 0 entries of the next thread: its kernel stack.
            crate::arch::x86_64::gdt::set_kernel_stack(kstack);
            // Address space of the next thread (the kernel half is shared, so
            // switching is safe from any kernel stack).
            let current_cr3: u64;
            // SAFETY: read of CR3.
            unsafe {
                core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack))
            };
            if cr3 != current_cr3 {
                // SAFETY: `cr3` is a PML4 built by the VMM or by `process::spawn`.
                unsafe { core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack)) };
            }
            // The lock stays held during the switch: the resuming thread releases
            // it (below, or `thread_first_run` if it is its first time).
            core::mem::forget(guard);
            // SAFETY: `old` points to a field of a `Box<Thread>` that neither moves nor
            // is freed while the thread exists; `new` was saved by an earlier
            // `switch_context` or prepared by `initial_stack_frame`.
            unsafe { switch_context(old, new) };
            // SAFETY: whoever switched to us left the lock held.
            unsafe { SCHED.force_unlock() };
            // On resuming here, another thread may have finished: free its stacks.
            reap();
        }
        None => drop(guard),
    }
    KernelIrq::restore(irq);
}

/// Frees finished threads. Always runs on a thread other than the one being
/// freed, so dropping its stack is safe.
fn reap() {
    let zombies: Vec<Box<Thread>> = with(|s| {
        let dead: Vec<Tid> = s.policy.terminated().collect();
        dead.into_iter()
            .filter_map(|t| {
                s.policy.reap(t).ok()?;
                s.threads.remove(&t)
            })
            .collect()
    });
    for z in zombies {
        debug!("task: reap {:?} '{}'", z.tid, z.name);
        if let Some(pid) = z.process {
            crate::user::process::release_address_space(pid);
        }
        drop(z);
    }
    // Endpoints closed by the capabilities just released (ADR 0030), or by
    // any capability dropped outside a syscall: no lock is held here.
    crate::ipc::settle_closed();
}

/// `true` if `addr` falls in the guard page of some thread's stack. Used
/// from the exception handler: `try_lock` so as not to block if the fault
/// happened inside the scheduler itself.
pub fn in_thread_stack_guard(addr: u64) -> bool {
    let Some(guard) = SCHED.try_lock() else {
        return false;
    };
    guard.as_ref().is_some_and(|s| {
        s.threads
            .values()
            .any(|t| t.stack.as_ref().is_some_and(|st| st.in_guard(addr)))
    })
}

pub fn thread_count() -> usize {
    with(|s| s.policy.thread_count())
}

/// Cooperative self-test (`test=threads`): two threads strictly alternate
/// with `yield`, and a batch of threads finishes and frees its stacks.
pub fn threads_selftest() {
    use crate::memory::pmm;
    use core::sync::atomic::{AtomicBool, AtomicU64};

    static LOG: IrqLock<Vec<u8>> = IrqLock::new(Vec::new());
    static GO: AtomicBool = AtomicBool::new(false);
    extern "C" fn ping(arg: usize) {
        // Nobody records until both threads exist: otherwise a tick between
        // the two spawns leaves only one in the queue and breaks alternation.
        while !GO.load(Ordering::Acquire) {
            yield_now();
        }
        for _ in 0..3 {
            LOG.lock().push(arg as u8);
            yield_now();
        }
    }
    assert_eq!(current_tid(), MAIN_TID, "the self-test runs on main");
    // Strict alternation only makes sense on one CPU: the three threads are
    // pinned to CPU 0.
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let a = spawn_on("ping-a", ping, b'A' as usize, 0).unwrap();
    let b = spawn_on("ping-b", ping, b'B' as usize, 0).unwrap();
    GO.store(true, Ordering::Release);
    while state(a).is_some() || state(b).is_some() {
        yield_now();
    }
    let log = LOG.lock().clone();
    // Who records first depends on where in the turn the last tick landed;
    // what is required is strict alternation.
    let alternates = log.len() == 6
        && log.windows(2).all(|w| w[0] != w[1])
        && log.iter().filter(|&&c| c == b'A').count() == 3;
    assert!(
        alternates,
        "the two threads must alternate: {:?}",
        core::str::from_utf8(&log)
    );
    set_affinity(MAIN_TID, None).unwrap();

    static DONE: AtomicU64 = AtomicU64::new(0);
    extern "C" fn worker(_: usize) {
        for _ in 0..10 {
            yield_now();
        }
        DONE.fetch_add(1, Ordering::Relaxed);
    }
    let free_before = pmm::stats().free_frames;
    const N: usize = 40;
    let tids: Vec<Tid> = (0..N)
        .map(|i| spawn("worker", worker, i).unwrap())
        .collect();
    while tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    // The last zombies are reaped by the thread that resumes after their exit,
    // which may be on another CPU: yielding a couple of times gives them time.
    for _ in 0..8 {
        sleep_ms(10);
    }
    assert_eq!(DONE.load(Ordering::Relaxed), N as u64);
    let free_after = pmm::stats().free_frames;
    // Stacks are returned; only new page tables may remain.
    assert!(
        free_before - free_after <= 8,
        "frames not returned after reap: {}",
        free_before - free_after
    );
    // main, the console cursor thread and one idle thread per CPU.
    let expected = 2 + cpu::online_count();
    assert_eq!(
        thread_count(),
        expected,
        "only main, the cursor thread and one idle per CPU"
    );
    assert_eq!(current_tid(), MAIN_TID);
    info!(
        "threads selftest ok (alternation {}, {N} workers finished and reaped, {} frames in tables)",
        core::str::from_utf8(&log).unwrap_or("?"),
        free_before - free_after
    );
}

/// Preemptive self-test (`test=sched`): threads that never yield share the
/// CPU, scheduled sleep respects time, and block/wake work through a wait
/// queue.
pub fn selftest() {
    use super::wait::WaitQueue;
    use core::sync::atomic::{AtomicBool, AtomicU64};

    // 1. Fairness: three busy threads without yield for 300 ms, all on CPU 0
    // so they truly compete for it.
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTERS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    extern "C" fn busy(i: usize) {
        while !STOP.load(Ordering::Relaxed) {
            COUNTERS[i].fetch_add(1, Ordering::Relaxed);
            core::hint::spin_loop();
        }
    }
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let switches_before = stats().switches;
    let busy_tids: Vec<Tid> = (0..3)
        .map(|i| spawn_on("busy", busy, i, 0).unwrap())
        .collect();
    sleep_ms(300);
    STOP.store(true, Ordering::Relaxed);
    while busy_tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let counts: Vec<u64> = COUNTERS.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let (min, max) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
    assert!(min > 0, "a busy thread never got to run: {counts:?}");
    assert!(min * 2 >= max, "unfair share: {counts:?}");
    let switches = stats().switches - switches_before;
    assert!(
        switches >= 20,
        "too few context switches in 300 ms: {switches}"
    );
    set_affinity(MAIN_TID, None).unwrap();

    // 2. Scheduled sleep: a thread sleeps 50 ms and records when it wakes.
    static WOKE_AT: AtomicU64 = AtomicU64::new(0);
    extern "C" fn sleeper(_: usize) {
        sleep_ms(50);
        WOKE_AT.store(crate::time::ticks(), Ordering::Relaxed);
    }
    let t0 = crate::time::ticks();
    let st = spawn("sleeper", sleeper, 0).unwrap();
    while state(st).is_some() {
        yield_now();
    }
    let slept = WOKE_AT.load(Ordering::Relaxed) - t0;
    assert!((5..=7).contains(&slept), "sleep_ms(50) slept {slept} ticks");

    // 3. Block and wake through a wait queue: two consumers, `wake_one`
    // releases exactly one and `wake_all` the rest.
    static QUEUE: WaitQueue = WaitQueue::new();
    static READY_FLAG: AtomicU64 = AtomicU64::new(0);
    static CONSUMED: AtomicU64 = AtomicU64::new(0);
    extern "C" fn consumer(i: usize) {
        QUEUE.wait_until(|| READY_FLAG.load(Ordering::Relaxed) & (1 << i) != 0);
        CONSUMED.fetch_add(1, Ordering::Relaxed);
    }
    let c0 = spawn("consumer-0", consumer, 0).unwrap();
    let c1 = spawn("consumer-1", consumer, 1).unwrap();
    sleep_ms(30);
    assert_eq!(state(c0), Some(State::Blocked));
    assert_eq!(state(c1), Some(State::Blocked));
    assert_eq!(CONSUMED.load(Ordering::Relaxed), 0);
    READY_FLAG.store(0b01, Ordering::Relaxed);
    QUEUE.wake_one(); // wakes c0, which was the first to wait
    sleep_ms(30);
    assert_eq!(
        CONSUMED.load(Ordering::Relaxed),
        1,
        "wake_one must release only one"
    );
    assert_eq!(
        state(c1),
        Some(State::Blocked),
        "the second one is still waiting"
    );
    READY_FLAG.store(0b11, Ordering::Relaxed);
    QUEUE.wake_all();
    while state(c0).is_some() || state(c1).is_some() {
        yield_now();
    }
    assert_eq!(CONSUMED.load(Ordering::Relaxed), 2);

    let st = stats();
    info!(
        "sched selftest ok (busy {counts:?} in 300 ms, {switches} switches, {} preemptions, sleep 50 ms -> {slept} ticks, wait_one/wake_all)",
        st.preemptions
    );
}

/// SMP self-test (`test=smp`): every MADT CPU online, pinned threads running
/// simultaneously on different CPUs, unpinned threads that migrate, a TLB
/// shootdown observed from another CPU and a counter under a lock contended
/// by every CPU.
pub fn smp_selftest() {
    use crate::memory::{phys, pmm, vmm};
    use aricode_x86_paging::{GLOBAL, NO_EXECUTE, WRITABLE};
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

    let cpus = cpu::online_count();
    let listed = crate::acpi::with(|a| a.madt.lapic_ids.len()).clamp(1, cpu::MAX_CPUS);
    assert_eq!(cpus, listed, "not every MADT CPU is online");
    assert!(cpus <= cpu::MAX_CPUS);

    // 1. Affinity and parallelism: one busy thread per CPU for 150 ms.
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTS: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];
    static SEEN: [AtomicU32; cpu::MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; cpu::MAX_CPUS];
    extern "C" fn pinned(i: usize) {
        let me = cpu::id();
        SEEN[i].store(me, Ordering::Relaxed);
        while !STOP.load(Ordering::Relaxed) {
            COUNTS[i].fetch_add(1, Ordering::Relaxed);
            if cpu::id() != me {
                SEEN[i].store(u32::MAX - 1, Ordering::Relaxed);
            }
            core::hint::spin_loop();
        }
    }
    let t0 = crate::time::ticks();
    let tids: Vec<Tid> = (0..cpus)
        .map(|i| spawn_on("pinned", pinned, i, i).unwrap())
        .collect();
    sleep_ms(150);
    STOP.store(true, Ordering::Relaxed);
    while tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let elapsed = crate::time::ticks() - t0;
    let counts: Vec<u64> = COUNTS[..cpus]
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();
    for i in 0..cpus {
        assert_eq!(
            SEEN[i].load(Ordering::Relaxed),
            i as u32,
            "the thread pinned to CPU {i} ran on another"
        );
        assert!(counts[i] > 0, "the thread of CPU {i} did not run");
    }
    let (min, max) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
    assert!(
        min * 4 >= max,
        "the CPUs did not work simultaneously: {counts:?}"
    );
    assert!(
        elapsed < 40,
        "{cpus} busy threads in parallel took {elapsed} ticks"
    );

    // 2. Migration: unpinned threads yielding in a loop pass through every
    // CPU and at least one changes CPU. Waited for with a deadline
    // (incident 0002): on a loaded host a virtual CPU can be descheduled by
    // the host for a while, so asserting after a fixed amount of work
    // measured the host, not the kernel. The deadline is generous; only a
    // CPU that never runs a roamer within it fails the test, after the
    // report says which CPU it was and how much of the wait it spent idle.
    const MIGRATION_DEADLINE_MS: u64 = 10_000;
    const MIGRATION_WARN_MS: u64 = 1_000;
    const MIN_ROUNDS: u32 = 500;
    static MASKS: [AtomicU32; 2 * cpu::MAX_CPUS] = [const { AtomicU32::new(0) }; 2 * cpu::MAX_CPUS];
    static ROAM_STOP: AtomicBool = AtomicBool::new(false);
    extern "C" fn roamer(i: usize) {
        let mut rounds = 0u32;
        while rounds < MIN_ROUNDS || !ROAM_STOP.load(Ordering::Relaxed) {
            MASKS[i].fetch_or(1 << cpu::id(), Ordering::Relaxed);
            rounds = rounds.saturating_add(1);
            yield_now();
        }
    }
    let union = || {
        MASKS[..2 * cpus]
            .iter()
            .fold(0u32, |m, x| m | x.load(Ordering::Relaxed))
    };
    let migrated = || {
        MASKS[..2 * cpus]
            .iter()
            .filter(|m| m.load(Ordering::Relaxed).count_ones() >= 2)
            .count()
    };
    let every_cpu = (1u32 << cpus) - 1;
    let idle_before: Vec<u64> = cpu::online().map(|c| c.idle_cycles()).collect();
    let roam_start = crate::time::uptime_ms();
    let roamers: Vec<Tid> = (0..2 * cpus)
        .map(|i| spawn("roamer", roamer, i).unwrap())
        .collect();
    let roam_ms = loop {
        let waited = crate::time::uptime_ms() - roam_start;
        if union() == every_cpu && (cpus == 1 || migrated() > 0) {
            break waited;
        }
        if waited >= MIGRATION_DEADLINE_MS {
            report_cpus_without_roamers(union(), &idle_before, waited);
            break waited;
        }
        sleep_ms(10);
    };
    // Passing late is still worth seeing (audit robustness-1, L8): the
    // deadline must not hide a scheduler that got slow at migrating.
    if roam_ms > MIGRATION_WARN_MS {
        log::warn!(
            "smp selftest: every CPU reached only after {roam_ms} ms (more than {MIGRATION_WARN_MS} ms): host load or a scheduler regression, see incident 0002"
        );
    }
    ROAM_STOP.store(true, Ordering::Relaxed);
    while roamers.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let migrated = migrated();
    assert_eq!(
        union(),
        every_cpu,
        "unpinned threads must pass through every CPU within {MIGRATION_DEADLINE_MS} ms"
    );
    if cpus > 1 {
        assert!(
            migrated > 0,
            "no thread changed CPU in {roam_ms} ms: {MASKS:?}"
        );
    }

    // 3. TLB shootdown: another CPU has the translation in its TLB; the frame
    // is changed and after the shootdown it sees the new contents.
    const VIRT: u64 = 0xFFFF_9000_0010_0000;
    static READY: AtomicBool = AtomicBool::new(false);
    static GO: AtomicBool = AtomicBool::new(false);
    static SEEN_VALUE: AtomicU64 = AtomicU64::new(0);
    extern "C" fn reader(_: usize) {
        // SAFETY: the page is mapped by the self-test while it runs.
        let first = unsafe { (VIRT as *const u64).read_volatile() };
        assert_eq!(first, 0x11, "first read");
        READY.store(true, Ordering::Release);
        while !GO.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        // SAFETY: same as above.
        SEEN_VALUE.store(
            unsafe { (VIRT as *const u64).read_volatile() },
            Ordering::Relaxed,
        );
    }
    let served_before = crate::arch::x86_64::smp::flushes_served();
    let (f1, f2) = pmm::with(|p| (p.allocate().unwrap(), p.allocate().unwrap()));
    // SAFETY: freshly allocated frames, reachable through the physical map.
    unsafe {
        (phys::phys_to_virt(f1.addr()) as *mut u64).write_volatile(0x11);
        (phys::phys_to_virt(f2.addr()) as *mut u64).write_volatile(0x22);
    }
    vmm::with(|m| m.map_4k(VIRT, f1.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).unwrap();
    let reader_cpu = if cpus > 1 { 1 } else { 0 };
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let r = spawn_on("reader", reader, 0, reader_cpu).unwrap();
    while !READY.load(Ordering::Acquire) {
        yield_now();
    }
    vmm::with(|m| {
        m.unmap_4k(VIRT).unwrap();
        m.map_4k(VIRT, f2.addr(), WRITABLE | NO_EXECUTE | GLOBAL)
            .unwrap();
    });
    vmm::flush_page(VIRT);
    GO.store(true, Ordering::Release);
    while state(r).is_some() {
        yield_now();
    }
    assert_eq!(
        SEEN_VALUE.load(Ordering::Relaxed),
        0x22,
        "CPU {reader_cpu} still saw the old translation"
    );
    let served = crate::arch::x86_64::smp::flushes_served() - served_before;
    assert_eq!(served, cpus - 1, "invalidation IPIs serviced");
    vmm::with(|m| m.unmap_4k(VIRT)).unwrap();
    vmm::flush_page(VIRT);
    pmm::with(|p| {
        p.free(f1).unwrap();
        p.free(f2).unwrap();
    });
    set_affinity(MAIN_TID, None).unwrap();

    // 4. Contended locks: a counter under an IrqLock incremented from every
    // CPU at once.
    static COUNTER: IrqLock<u64> = IrqLock::new(0);
    const PER_THREAD: u64 = 20_000;
    extern "C" fn incrementer(_: usize) {
        for _ in 0..PER_THREAD {
            *COUNTER.lock() += 1;
        }
    }
    *COUNTER.lock() = 0;
    let incs: Vec<Tid> = (0..cpus)
        .map(|i| spawn_on("inc", incrementer, i, i).unwrap())
        .collect();
    while incs.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let total = *COUNTER.lock();
    assert_eq!(
        total,
        PER_THREAD * cpus as u64,
        "increments lost under the lock"
    );

    info!(
        "smp selftest ok ({cpus} CPUs: busy {counts:?} in {elapsed} ticks, {migrated} of {} threads migrated, every CPU reached in {roam_ms} ms, shootdown seen by CPU {reader_cpu} ({served} IPI), counter {total})",
        2 * cpus
    );
}

/// Incident 0002: the migration deadline expired. Logs, for each online CPU
/// no roamer ran on, how much of the wait it spent idle (idle the whole
/// time points at the scheduler never handing it unpinned threads; little
/// idle time at the CPU being busy or descheduled by the host) and whether
/// it is in its idle thread now.
fn report_cpus_without_roamers(union: u32, idle_before: &[u64], waited_ms: u64) {
    log::error!(
        "smp selftest: after {waited_ms} ms unpinned threads ran only on CPU mask {union:#b}"
    );
    for (c, before) in cpu::online().zip(idle_before) {
        if union & (1 << c.index()) != 0 {
            continue;
        }
        let idle = c.idle_cycles().saturating_sub(*before);
        match crate::time::tsc_cycles_to_ms(idle) {
            Some(ms) => log::error!(
                "smp selftest: CPU {} never ran an unpinned thread: idle {ms} of {waited_ms} ms, in its idle thread now: {}",
                c.index(),
                c.is_idle()
            ),
            None => log::error!(
                "smp selftest: CPU {} never ran an unpinned thread: idle {idle} TSC cycles (uncalibrated), in its idle thread now: {}",
                c.index(),
                c.is_idle()
            ),
        }
    }
}
