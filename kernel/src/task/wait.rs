//! Cola de espera: bloquea al hilo actual hasta que se cumpla una condición
//! y alguien llame a `wake_all`/`wake_one`, con plazo opcional.
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
    pub fn wait_until(&self, cond: impl FnMut() -> bool) {
        let timed_out = self.wait_until_deadline(cond, None);
        debug_assert!(!timed_out);
    }

    /// Como `wait_until`, pero con plazo en ticks: devuelve `true` si el plazo
    /// venció sin que la condición se cumpliera. Con `Some(deadline)` el hilo
    /// duerme hasta ese tick y un `wake` anticipado lo despierta antes.
    pub fn wait_until_deadline(
        &self,
        mut cond: impl FnMut() -> bool,
        deadline: Option<u64>,
    ) -> bool {
        loop {
            {
                let mut w = self.waiters.lock();
                if cond() {
                    return false;
                }
                if deadline.is_some_and(|d| crate::time::ticks() >= d) {
                    return true;
                }
                w.push_back(scheduler::current_tid());
            }
            // El lock se ha soltado pero las interrupciones siguen deshabilitadas
            // dentro del cambio de contexto: un `wake` concurrente solo puede
            // llegar por interrupción y encontrará el hilo bloqueado/dormido, o
            // aún corriendo (entonces `wake` falla y el bucle reevalúa).
            match deadline {
                Some(d) => scheduler::sleep_until(d),
                None => scheduler::block_current(),
            }
        }
    }

    /// Despierta a todos los que esperan.
    pub fn wake_all(&self) {
        let waiters: VecDeque<Tid> = core::mem::take(&mut *self.waiters.lock());
        for tid in waiters {
            // Si el hilo aún no llegó a bloquearse, reintentará por su cuenta.
            let _ = scheduler::wake(tid);
        }
    }

    /// Despierta a uno (el más antiguo).
    pub fn wake_one(&self) {
        let tid = self.waiters.lock().pop_front();
        if let Some(t) = tid {
            let _ = scheduler::wake(t);
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}
