//! Cola de espera: bloquea al hilo actual hasta que se cumpla una condición
//! y alguien llame a `wake_all`/`wake_one`, con plazo opcional.
//!
//! La comprobación de la condición, el encolado y el cambio de contexto
//! ocurren con las interrupciones deshabilitadas, así que un `wake` desde un
//! handler nunca cae entre "comprobar" y "dormir": encuentra al hilo ya
//! dormido o bloqueado y lo despierta.

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
        use crate::sync::KernelIrq;
        use aricode_sync::IrqPolicy;
        let me = scheduler::current_tid();
        loop {
            // Interrupciones deshabilitadas desde la comprobación hasta el
            // cambio de contexto: así un `wake` desde un handler de esta CPU
            // solo puede llegar cuando el hilo ya está dormido o bloqueado.
            // Un `wake` desde otra CPU en esa ventana queda pendiente en la
            // política y convierte el bloqueo en cesión (Fase 32).
            let irq = KernelIrq::save_and_disable();
            {
                let mut w = self.waiters.lock();
                // Si se vuelve a evaluar tras un despertar anticipado o un
                // plazo, la anotación anterior ya no sirve: evita despertares
                // espurios a quien luego espere en otra cola.
                w.retain(|t| *t != me);
                if cond() {
                    KernelIrq::restore(irq);
                    return false;
                }
                if deadline.is_some_and(|d| crate::time::ticks() >= d) {
                    KernelIrq::restore(irq);
                    return true;
                }
                w.push_back(me);
            }
            match deadline {
                Some(d) => scheduler::sleep_until(d),
                None => scheduler::block_current(),
            }
            KernelIrq::restore(irq);
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
