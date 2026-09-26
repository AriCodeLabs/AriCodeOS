//! Pilas de hilo: región virtual propia, frames del PMM y guard page sin
//! mapear por debajo de cada pila.

use crate::memory::{pmm, vmm};
use aricode_pmm::Frame;
use aricode_x86_paging::{GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE};

/// Región de pilas de hilo (entrada 448 del PML4).
pub const STACKS_BASE: u64 = 0xFFFF_E000_0000_0000;
pub const STACK_SIZE: u64 = 32 * 1024;
const SLOT_SIZE: u64 = STACK_SIZE + PAGE_SIZE; // guard + pila

pub struct KernelStack {
    slot: u32,
}

impl KernelStack {
    /// Mapea la pila del slot `slot` (uno por hilo, nunca reutilizado dentro
    /// de la misma sesión de arranque: el slot es el tid).
    pub fn allocate(slot: u32) -> Result<Self, &'static str> {
        let s = Self { slot };
        let mut done = 0;
        for virt in (s.bottom()..s.top()).step_by(PAGE_SIZE as usize) {
            let Some(frame) = pmm::with(|p| p.allocate()) else {
                s.unmap(done);
                return Err("stack: sin frames");
            };
            if vmm::with(|m| m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).is_err()
            {
                pmm::with(|p| p.free(frame)).expect("frame recien asignado");
                s.unmap(done);
                return Err("stack: map");
            }
            done += PAGE_SIZE;
        }
        Ok(s)
    }

    pub fn guard(&self) -> u64 {
        STACKS_BASE + self.slot as u64 * SLOT_SIZE
    }
    pub fn bottom(&self) -> u64 {
        self.guard() + PAGE_SIZE
    }
    pub fn top(&self) -> u64 {
        self.bottom() + STACK_SIZE
    }
    pub fn in_guard(&self, addr: u64) -> bool {
        (self.guard()..self.bottom()).contains(&addr)
    }

    fn unmap(&self, len: u64) {
        for virt in (self.bottom()..self.bottom() + len).step_by(PAGE_SIZE as usize) {
            let phys = vmm::with(|m| m.unmap_4k(virt)).expect("pagina de pila mapeada");
            // SAFETY: invalidación de una entrada que acabamos de retirar.
            unsafe {
                core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags))
            };
            pmm::with(|p| p.free(Frame::from_addr(phys))).expect("frame de pila");
        }
    }
}

impl Drop for KernelStack {
    /// Devuelve las páginas al PMM. Nunca debe soltarse la pila del hilo en
    /// ejecución: el planificador solo libera hilos ya terminados desde otro.
    fn drop(&mut self) {
        self.unmap(STACK_SIZE);
    }
}
