//! Estado por CPU (Fase 32): tablas propias (GDT, TSS, pila de #DF), hilo en
//! ejecución, quantum y scratch de `syscall`.
//!
//! `IA32_GS_BASE` apunta al `Cpu` de la CPU mientras ejecuta en ring 0 y se
//! intercambia con `swapgs` en cada transición con ring 3, así que el código
//! de ensamblador llega a sus campos con `gs:[desplazamiento]` y el de Rust
//! con un único acceso relativo a GS, que es atómico frente a una migración:
//! un hilo que lee "mi hilo actual" siempre se lee a sí mismo, en la CPU que
//! sea. Para cualquier otro campo hay que deshabilitar interrupciones antes
//! de tomar `current()`.

use crate::arch::x86_64::gdt::Tables;
use crate::arch::x86_64::msr;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const MAX_CPUS: usize = 16;

/// Celda de estado que solo toca la CPU dueña, con interrupciones
/// deshabilitadas (o durante su propio arranque).
pub struct PerCpuCell<T>(UnsafeCell<T>);

// SAFETY: el acceso queda restringido por contrato a la CPU dueña, que no
// puede ser interrumpida mientras lo usa (ver `get`).
unsafe impl<T> Sync for PerCpuCell<T> {}

impl<T> PerCpuCell<T> {
    pub const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// # Safety
    ///
    /// Solo desde la CPU dueña y sin que una interrupción pueda reentrar.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get(&self) -> &mut T {
        &mut *self.0.get()
    }
}

#[repr(C)]
pub struct Cpu {
    /// Dirección de este mismo `Cpu` (`gs:[0]`).
    self_ptr: AtomicU64,
    /// Pila de kernel del hilo actual para la entrada por `syscall`.
    kernel_rsp: AtomicU64,
    /// Scratch de `syscall_entry` para el `rsp` de usuario.
    user_rsp: AtomicU64,
    current: AtomicU32,
    quantum_left: AtomicU32,
    index: u32,
    lapic_id: AtomicU32,
    online: AtomicBool,
    idle_running: AtomicBool,
    idle_tid: AtomicU32,
    tables: PerCpuCell<Tables>,
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
        }
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
    /// La CPU ya atiende interrupciones y planifica.
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
    /// Hilo que pasa a ejecutar en esta CPU (solo desde ella, con IF = 0).
    pub fn set_current(&self, tid: u32, quantum: u32) {
        self.current.store(tid, Ordering::Relaxed);
        self.quantum_left.store(quantum, Ordering::Relaxed);
        self.idle_running
            .store(tid == self.idle_tid(), Ordering::Relaxed);
    }
    /// Resta un tick al quantum; `true` si expiró.
    pub fn quantum_tick(&self) -> bool {
        self.quantum_left.fetch_sub(1, Ordering::Relaxed) <= 1
    }
    /// Pila de kernel del hilo actual para `syscall` (solo la CPU dueña).
    pub fn set_kernel_rsp(&self, top: u64) {
        self.kernel_rsp.store(top, Ordering::Relaxed);
    }

    /// Tablas de esta CPU.
    ///
    /// # Safety
    ///
    /// Solo desde la propia CPU con interrupciones deshabilitadas.
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

/// Apunta `IA32_GS_BASE` de la CPU que llama a `cpu`. Después de cargar la
/// GDT (cargar un selector en GS pone su base a cero) y antes de cualquier
/// acceso relativo a GS.
pub fn activate(cpu: &'static Cpu) {
    const IA32_GS_BASE: u32 = 0xC000_0101;
    const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;
    let addr = cpu as *const Cpu as u64;
    cpu.self_ptr.store(addr, Ordering::Relaxed);
    // SAFETY: bases de segmento; el `Cpu` es estático y nunca se mueve. El
    // valor de usuario (KERNEL_GS_BASE hasta el primer swapgs) es cero.
    unsafe {
        msr::write(IA32_GS_BASE, addr);
        msr::write(IA32_KERNEL_GS_BASE, 0);
    }
}

fn gs_read_u64(offset: usize) -> u64 {
    let v: u64;
    // SAFETY: lectura relativa a GS dentro del `Cpu` de esta CPU.
    unsafe {
        asm!("mov {}, gs:[{}]", out(reg) v, in(reg) offset, options(nostack, readonly, preserves_flags))
    };
    v
}

fn gs_read_u32(offset: usize) -> u32 {
    let v: u32;
    // SAFETY: ídem.
    unsafe {
        asm!("mov {:e}, gs:[{}]", out(reg) v, in(reg) offset, options(nostack, readonly, preserves_flags))
    };
    v
}

/// `Cpu` de la CPU que ejecuta. Solo es estable mientras las interrupciones
/// estén deshabilitadas (si no, el hilo puede migrar tras leerlo).
pub fn current() -> &'static Cpu {
    // SAFETY: gs:[0] guarda la dirección de un `Cpu` estático (`activate`).
    unsafe { &*(gs_read_u64(OFF_SELF) as *const Cpu) }
}

/// Índice de la CPU que ejecuta en este instante.
pub fn id() -> u32 {
    gs_read_u32(OFF_INDEX)
}

/// Hilo en ejecución en esta CPU: el propio llamador, en cualquier CPU.
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

/// Alguna CPU distinta de `me` que esté en su hilo idle (para despertarla).
pub fn idle_other_than(me: u32) -> Option<&'static Cpu> {
    online().find(|c| c.index != me && c.is_idle())
}
