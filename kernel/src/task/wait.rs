//! Cola de espera: bloquea al hilo actual hasta que se cumpla una condición
//! y alguien llame a `wake_all`/`wake_one`.
//!
//! La comprobación de la condición y el encolado ocurren bajo el lock de la
//! cola con interrupciones deshabilitadas, así que un `wake` entre "comprobar"
//! y "bloquear" no se pierde: el bloqueo se decide con el hilo ya en la cola.

use super::scheduler;
use crate::sync::IrqLock;
use alloc::collections::VecDeque;
use aricode_sched::Tid;

pub struct WaitQueue {
    waiters: IrqLock<VecDeque<Tid>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: IrqLock::new(VecDeque::new()),
        }
    }

    /// Bloquea hasta que `cond()` sea cierta. `cond` se evalúa bajo el lock.
    pub fn wait_until(&self, mut cond: impl FnMut() -> bool) {
        loop {
            {
                let mut w = self.waiters.lock();
                if cond() {
                    return;
                }
                w.push_back(scheduler::current_tid());
            }
            // El lock se ha soltado pero las interrupciones siguen deshabilitadas
            // dentro de `block_current` hasta el cambio: un `wake` concurrente
            // solo puede llegar por interrupción y encontrará el hilo bloqueado
            // o aún corriendo (en cuyo caso `unblock` falla y el bucle reintenta).
            scheduler::block_current();
        }
    }

    /// Despierta a todos los que esperan.
    pub fn wake_all(&self) {
        let waiters: VecDeque<Tid> = core::mem::take(&mut *self.waiters.lock());
        for tid in waiters {
            // Si el hilo aún no llegó a bloquearse, reintentará por su cuenta.
            let _ = scheduler::unblock(tid);
        }
    }

    /// Despierta a uno (el más antiguo).
    pub fn wake_one(&self) {
        let tid = self.waiters.lock().pop_front();
        if let Some(t) = tid {
            let _ = scheduler::unblock(t);
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}
