//! Bloque de control de un hilo del kernel.

use super::context::{initial_stack_frame, ThreadEntry};
use super::stack::KernelStack;
use alloc::string::String;
use aricode_sched::Tid;

pub struct Thread {
    pub tid: Tid,
    pub name: String,
    /// `None` para el contexto de arranque (usa la pila del kernel).
    pub stack: Option<KernelStack>,
    /// `rsp` guardado por `switch_context` cuando el hilo no está ejecutando.
    pub saved_rsp: u64,
    /// Proceso de usuario al que pertenece (`None` = hilo del kernel).
    pub process: Option<u32>,
    /// PML4 a cargar en CR3 al conmutar a este hilo.
    pub cr3: u64,
}

impl Thread {
    /// Hilo que ya está ejecutando sobre una pila ajena (el de arranque).
    pub fn adopt_current(tid: Tid, name: &str, cr3: u64) -> Self {
        Self {
            tid,
            name: String::from(name),
            stack: None,
            saved_rsp: 0,
            process: None,
            cr3,
        }
    }

    /// Hilo nuevo con pila propia listo para su primer cambio de contexto.
    pub fn new(
        tid: Tid,
        name: &str,
        entry: ThreadEntry,
        arg: usize,
        process: Option<u32>,
        cr3: u64,
    ) -> Result<Self, &'static str> {
        let stack = KernelStack::allocate(tid.0)?;
        // SAFETY: `stack.top()` es el fin de una pila recién mapeada RW.
        let saved_rsp = unsafe { initial_stack_frame(stack.top(), entry, arg) };
        Ok(Self {
            tid,
            name: String::from(name),
            stack: Some(stack),
            saved_rsp,
            process,
            cr3,
        })
    }

    /// Cima de la pila de kernel de este hilo (TSS.rsp0 y entrada `syscall`).
    pub fn kernel_stack_top(&self) -> u64 {
        match &self.stack {
            Some(s) => s.top(),
            None => crate::memory::vmm::KERNEL_STACK_TOP,
        }
    }
}
