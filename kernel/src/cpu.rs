// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Per-CPU state (Phase 32): own tables (GDT, TSS, #DF stack), running
//! thread, quantum and the `syscall` scratch slot.
//!
//! `IA32_GS_BASE` points to the CPU's `Cpu` while it runs in ring 0 and is
//! swapped with `swapgs` on every transition to/from ring 3, so assembly
//! code reaches its fields with `gs:[offset]` and Rust code with a single
//! GS-relative access, which is atomic with respect to migration: a thread
//! reading "my current thread" always reads itself, on whatever CPU. For
//! any other field, interrupts must be disabled before calling `current()`.

use crate::arch::x86_64::gdt::Tables;
use crate::arch::x86_64::msr;
use crate::sync::IrqLock;
use aricode_sched::idle::IdleClock;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const MAX_CPUS: usize = 16;

/// State cell touched only by its owning CPU, with interrupts disabled (or
/// during its own startup).
pub struct PerCpuCell<T>(UnsafeCell<T>);

// SAFETY: access is restricted by contract to the owning CPU, which cannot
// be interrupted while using it (see `get`).
unsafe impl<T> Sync for PerCpuCell<T> {}

impl<T> PerCpuCell<T> {
    pub const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// # Safety
    ///
    /// Only from the owning CPU and with no interrupt able to re-enter.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self) -> &mut T {
        &mut *self.0.get()
    }
}

#[repr(C)]
pub struct Cpu {
    /// Address of this very `Cpu` (`gs:[0]`).
    self_ptr: AtomicU64,
    /// Kernel stack of the current thread for `syscall` entry.
    kernel_rsp: AtomicU64,
    /// `syscall_entry` scratch slot for the user `rsp`.
    user_rsp: AtomicU64,
    current: AtomicU32,
    quantum_left: AtomicU32,
    index: u32,
    lapic_id: AtomicU32,
    online: AtomicBool,
    idle_running: AtomicBool,
    idle_tid: AtomicU32,
    tables: PerCpuCell<Tables>,
    /// TSC cycles with the idle thread running (`sysinfo::IDLE`, incident
    /// 0020): written by this CPU in `set_current`, read by any CPU.
    idle: IrqLock<IdleClock>,
}

pub const OFF_SELF: usize = 0;
pub const OFF_KERNEL_RSP: usize = 8;
pub const OFF_USER_RSP: usize = 16;
pub const OFF_CURRENT: usize = 24;
pub const OFF_INDEX: usize = 32;

const _: () = {
    assert!(core::mem::offset_of!(Cpu, self_ptr) == OFF_SELF);
    assert!(core::mem::offset_of!(Cpu, kernel_rsp) == OFF_KERNEL_RSP);
    assert!(core::mem::offset_of!(Cpu, user_rsp) == OFF_USER_RSP);
    assert!(core::mem::offset_of!(Cpu, current) == OFF_CURRENT);
    assert!(core::mem::offset_of!(Cpu, index) == OFF_INDEX);
};

impl Cpu {
    const fn new(index: u32) -> Self {
        Self {
            self_ptr: AtomicU64::new(0),
            kernel_rsp: AtomicU64::new(0),
            user_rsp: AtomicU64::new(0),
            current: AtomicU32::new(0),
            quantum_left: AtomicU32::new(0),
            index,
            lapic_id: AtomicU32::new(0),
            online: AtomicBool::new(false),
            idle_running: AtomicBool::new(false),
            idle_tid: AtomicU32::new(u32::MAX),
            tables: PerCpuCell::new(Tables::new()),
            idle: IrqLock::new(IdleClock::new()),
        }
    }

    /// TSC cycles this CPU's idle thread has been its running thread, the
    /// current idle stretch included (`IdleClock`), up to now.
    pub fn idle_cycles(&self) -> u64 {
        let (clock, now) = self.idle_clock_now();
        clock.total(now)
    }
    /// This CPU's idle clock (closed stretches and the open one) and the
    /// TSC, read together under its lock: no switch of this CPU is
    /// accounted between the two, and every stretch in the clock started
    /// and ended before the TSC value.
    pub fn idle_clock_now(&self) -> (IdleClock, u64) {
        let clock = self.idle.lock();
        (*clock, crate::time::rdtsc())
    }

    pub fn index(&self) -> usize {
        self.index as usize
    }
    pub fn lapic_id(&self) -> u32 {
        self.lapic_id.load(Ordering::Relaxed)
    }
    pub fn set_lapic_id(&self, id: u32) {
        self.lapic_id.store(id, Ordering::Relaxed);
    }
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }
    /// The CPU now services interrupts and schedules.
    pub fn set_online(&self) {
        self.online.store(true, Ordering::Release);
    }
    pub fn idle_tid(&self) -> u32 {
        self.idle_tid.load(Ordering::Relaxed)
    }
    pub fn set_idle_tid(&self, tid: u32) {
        self.idle_tid.store(tid, Ordering::Relaxed);
    }
    pub fn is_idle(&self) -> bool {
        self.idle_running.load(Ordering::Relaxed)
    }
    /// Thread that starts running on this CPU (only from it, with IF = 0).
    /// Idle time is accounted here, at the switch (incident 0020): leaving
    /// the idle thread closes the idle stretch, entering it opens one.
    pub fn set_current(&self, tid: u32, quantum: u32) {
        let from_idle = self.idle_running.load(Ordering::Relaxed);
        let to_idle = tid == self.idle_tid();
        if from_idle || to_idle {
            // The TSC is read under the lock, so a reader holding it sees
            // either the clock before this switch or after it, with its
            // timestamp earlier than anything the reader reads next.
            let mut clock = self.idle.lock();
            clock.switch(from_idle, to_idle, crate::time::rdtsc());
        }
        self.current.store(tid, Ordering::Relaxed);
        self.quantum_left.store(quantum, Ordering::Relaxed);
        self.idle_running
            .store(tid == self.idle_tid(), Ordering::Relaxed);
    }
    /// Takes one tick off the quantum; `true` if it expired, and then the
    /// quantum starts again at `quantum` even if no switch follows
    /// (`aricode_sched::quantum_tick`, incident 0023). Only from this CPU
    /// with IF = 0 (the timer handler), so load and store do not race.
    pub fn quantum_tick(&self, quantum: u32) -> bool {
        let left = self.quantum_left.load(Ordering::Relaxed);
        let (left, expired) = aricode_sched::quantum_tick(left, quantum);
        self.quantum_left.store(left, Ordering::Relaxed);
        expired
    }
    /// Kernel stack of the current thread for `syscall` (owning CPU only).
    pub fn set_kernel_rsp(&self, top: u64) {
        self.kernel_rsp.store(top, Ordering::Relaxed);
    }

    /// This CPU's tables.
    ///
    /// # Safety
    ///
    /// Only from this very CPU with interrupts disabled.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn tables(&self) -> &mut Tables {
        self.tables.get()
    }
}

static CPUS: [Cpu; MAX_CPUS] = {
    let mut cpus = [const { Cpu::new(0) }; MAX_CPUS];
    let mut i = 0;
    while i < MAX_CPUS {
        cpus[i].index = i as u32;
        i += 1;
    }
    cpus
};

pub fn slot(index: usize) -> &'static Cpu {
    &CPUS[index]
}

/// Points the calling CPU's `IA32_GS_BASE` at `cpu`. After loading the GDT
/// (loading a selector into GS zeroes its base) and before any GS-relative
/// access.
pub fn activate(cpu: &'static Cpu) {
    const IA32_GS_BASE: u32 = 0xC000_0101;
    const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
    let addr = cpu as *const Cpu as u64;
    cpu.self_ptr.store(addr, Ordering::Relaxed);
    // SAFETY: segment bases; the `Cpu` is static and never moves. The user
    // value (KERNEL_GS_BASE until the first swapgs) is zero.
    unsafe {
        msr::write(IA32_GS_BASE, addr);
        msr::write(IA32_KERNEL_GS_BASE, 0);
    }
}

fn gs_read_u64(offset: usize) -> u64 {
    let v: u64;
    // SAFETY: GS-relative read within this CPU's `Cpu`.
    unsafe {
        asm!("mov {}, gs:[{}]", out(reg) v, in(reg) offset, options(nostack, readonly, preserves_flags))
    };
    v
}

fn gs_read_u32(offset: usize) -> u32 {
    let v: u32;
    // SAFETY: same as above.
    unsafe {
        asm!("mov {:e}, gs:[{}]", out(reg) v, in(reg) offset, options(nostack, readonly, preserves_flags))
    };
    v
}

/// `Cpu` of the executing CPU. Only stable while interrupts are disabled
/// (otherwise the thread may migrate after reading it).
pub fn current() -> &'static Cpu {
    // SAFETY: gs:[0] holds the address of a static `Cpu` (`activate`).
    unsafe { &*(gs_read_u64(OFF_SELF) as *const Cpu) }
}

/// Index of the CPU executing right now.
pub fn id() -> u32 {
    gs_read_u32(OFF_INDEX)
}

/// Thread running on this CPU: the caller itself, on any CPU.
pub fn current_tid() -> u32 {
    gs_read_u32(OFF_CURRENT)
}

pub fn is_bsp() -> bool {
    id() == 0
}

pub fn online() -> impl Iterator<Item = &'static Cpu> {
    CPUS.iter().filter(|c| c.is_online())
}

pub fn online_count() -> usize {
    online().count()
}

/// Every online CPU's idle clock, in CPU order, appended to `out`, and the
/// TSC read once with all those clocks locked (`sysinfo::IDLE`, incident
/// 0020). Every switch accounts under its own CPU's lock with the TSC read
/// inside it, so no CPU can close or open a stretch between the clocks and
/// the TSC value: the result is one instant. Locks are taken in CPU order
/// and released in reverse (properly nested, so each restores the
/// interrupt state it saw); a switch takes only its own lock, so this
/// cannot deadlock. `out` should have room for `MAX_CPUS` clocks so
/// nothing is allocated with the locks held.
pub fn idle_clocks_at_one_instant(out: &mut alloc::vec::Vec<IdleClock>) -> u64 {
    fn lock_from(
        cpus: &mut dyn Iterator<Item = &'static Cpu>,
        out: &mut alloc::vec::Vec<IdleClock>,
    ) -> u64 {
        match cpus.next() {
            Some(c) => {
                let clock = c.idle.lock();
                out.push(*clock);
                lock_from(cpus, out)
            }
            None => crate::time::rdtsc(),
        }
    }
    lock_from(&mut online(), out)
}
