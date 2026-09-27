//! Política de planificación (Fases 9, 10 y 32): round robin sobre
//! identificadores de hilo, con estados, sueño por tick, bloqueo, salida,
//! varias CPUs y afinidad.
//!
//! No sabe nada de pilas ni registros: el kernel le pregunta "¿quién sigue
//! en esta CPU?" y hace el cambio de contexto. Así la política se prueba en
//! host de forma determinista.
//!
//! Con varias CPUs hay una sola cola de listos compartida y, por CPU, un hilo
//! en ejecución y un hilo idle propio (el idle nunca se encola). Un hilo con
//! afinidad solo lo toma la CPU indicada. Un `wake` que llega mientras el
//! hilo todavía está `Running` (otra CPU lo despierta entre "anotarse en la
//! cola de espera" y "bloquearse") queda pendiente y convierte el siguiente
//! bloqueo en una cesión: ningún despertar se pierde.

#![no_std]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tid(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked,
    Sleeping,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    UnknownThread,
    /// La operación no es válida en el estado actual del hilo.
    WrongState,
    UnknownCpu,
}

struct CpuSlot {
    current: Tid,
    idle: Tid,
}

pub struct RoundRobin {
    states: BTreeMap<Tid, State>,
    ready: VecDeque<Tid>,
    /// (tick de despertar, hilo), sin orden particular.
    sleeping: Vec<(u64, Tid)>,
    cpus: Vec<CpuSlot>,
    /// Hilo -> única CPU en la que puede ejecutar.
    affinity: BTreeMap<Tid, usize>,
    /// Despertares recibidos por hilos que aún estaban `Running`.
    pending_wake: BTreeSet<Tid>,
    next_tid: u32,
}

impl RoundRobin {
    /// `current` es el hilo que ya está ejecutando en la CPU 0 (el contexto de
    /// arranque); `idle` es el que esa CPU elige cuando no hay nada listo.
    /// Ambos quedan registrados: `current` como `Running`, `idle` como
    /// `Ready` pero fuera de la cola (nunca se encola).
    pub fn new(current: Tid, idle: Tid) -> Self {
        let mut states = BTreeMap::new();
        states.insert(current, State::Running);
        states.insert(idle, State::Ready);
        Self {
            states,
            ready: VecDeque::new(),
            sleeping: Vec::new(),
            cpus: alloc::vec![CpuSlot { current, idle }],
            affinity: BTreeMap::new(),
            pending_wake: BTreeSet::new(),
            next_tid: current.0.max(idle.0) + 1,
        }
    }

    /// Registra la CPU `cpu` (la siguiente en orden) con su hilo idle, que
    /// arranca ya en ejecución en ella.
    pub fn add_cpu(&mut self, cpu: usize, idle: Tid) -> Result<(), SchedError> {
        if cpu != self.cpus.len() {
            return Err(SchedError::UnknownCpu);
        }
        self.states.insert(idle, State::Running);
        self.cpus.push(CpuSlot {
            current: idle,
            idle,
        });
        Ok(())
    }

    pub fn cpu_count(&self) -> usize {
        self.cpus.len()
    }

    /// Identificador nuevo, único durante la vida del planificador.
    pub fn allocate_tid(&mut self) -> Tid {
        let t = Tid(self.next_tid);
        self.next_tid += 1;
        t
    }

    /// Registra un hilo nuevo y lo pone al final de la cola.
    pub fn add(&mut self, tid: Tid) {
        self.states.insert(tid, State::Ready);
        self.ready.push_back(tid);
    }

    /// Fija (o quita) la CPU en la que `tid` puede ejecutar.
    pub fn set_affinity(&mut self, tid: Tid, cpu: Option<usize>) -> Result<(), SchedError> {
        if !self.states.contains_key(&tid) {
            return Err(SchedError::UnknownThread);
        }
        match cpu {
            Some(c) if c >= self.cpus.len() => Err(SchedError::UnknownCpu),
            Some(c) => {
                self.affinity.insert(tid, c);
                Ok(())
            }
            None => {
                self.affinity.remove(&tid);
                Ok(())
            }
        }
    }

    pub fn affinity(&self, tid: Tid) -> Option<usize> {
        self.affinity.get(&tid).copied()
    }

    pub fn current(&self, cpu: usize) -> Tid {
        self.cpus[cpu].current
    }
    pub fn idle(&self, cpu: usize) -> Tid {
        self.cpus[cpu].idle
    }
    /// `true` si la CPU ejecuta su hilo idle.
    pub fn is_idle(&self, cpu: usize) -> bool {
        self.cpus[cpu].current == self.cpus[cpu].idle
    }
    pub fn state(&self, tid: Tid) -> Option<State> {
        self.states.get(&tid).copied()
    }
    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }
    pub fn thread_count(&self) -> usize {
        self.states.len()
    }

    fn eligible(&self, tid: Tid, cpu: usize) -> bool {
        self.affinity.get(&tid).is_none_or(|c| *c == cpu)
    }

    /// `true` si hay algún hilo listo que `cpu` pueda ejecutar.
    pub fn has_ready_for(&self, cpu: usize) -> bool {
        self.ready.iter().any(|t| self.eligible(*t, cpu))
    }

    /// Saca de la cola el primer hilo que `cpu` pueda ejecutar.
    fn pop_ready_for(&mut self, cpu: usize) -> Option<Tid> {
        let pos = self.ready.iter().position(|t| self.eligible(*t, cpu))?;
        self.ready.remove(pos)
    }

    /// Siguiente hilo a ejecutar en `cpu` tras dejar su hilo actual en
    /// `state`. Devuelve `None` si no hay cambio (el mismo hilo sigue).
    fn switch(&mut self, cpu: usize, current_state: State) -> Option<Tid> {
        let cur = self.cpus[cpu].current;
        let idle = self.cpus[cpu].idle;
        // Un despertar pendiente convierte el bloqueo o el sueño en cesión.
        let current_state = match current_state {
            State::Blocked | State::Sleeping if self.pending_wake.remove(&cur) => {
                self.sleeping.retain(|(_, t)| *t != cur);
                State::Ready
            }
            s => s,
        };
        if current_state == State::Ready && cur != idle {
            self.ready.push_back(cur);
        }
        self.states.insert(cur, current_state);
        let next = self.pop_ready_for(cpu).unwrap_or(idle);
        self.states.insert(next, State::Running);
        self.cpus[cpu].current = next;
        if next == cur {
            None
        } else {
            Some(next)
        }
    }

    /// El hilo actual de `cpu` cede la CPU (vuelve al final de la cola).
    pub fn yield_current(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Ready)
    }

    /// Preempción por fin de quantum: igual que `yield`, pero el idle no se encola.
    pub fn preempt(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Ready)
    }

    /// El hilo actual de `cpu` se bloquea hasta que alguien lo despierte.
    pub fn block_current(&mut self, cpu: usize) -> Option<Tid> {
        self.switch(cpu, State::Blocked)
    }

    /// Despierta un hilo bloqueado y lo encola.
    pub fn unblock(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Blocked) => {
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// Despierta un hilo bloqueado o dormido (antes de su plazo) y lo encola.
    /// Sirve a las colas de espera con timeout: el hilo duerme con plazo y un
    /// `wake` anticipado lo saca de la lista de dormidos. Si el hilo todavía
    /// está `Running` (va a bloquearse en otra CPU), el despertar queda
    /// pendiente y su próximo bloqueo no bloquea.
    pub fn wake(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Blocked) => {
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(State::Sleeping) => {
                self.sleeping.retain(|(_, t)| *t != tid);
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                Ok(())
            }
            Some(State::Running) => {
                self.pending_wake.insert(tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// El hilo actual de `cpu` duerme hasta el tick `until`.
    pub fn sleep_current(&mut self, cpu: usize, until: u64) -> Option<Tid> {
        let cur = self.cpus[cpu].current;
        self.sleeping.push((until, cur));
        self.switch(cpu, State::Sleeping)
    }

    /// Encola los hilos cuyo tick de despertar ha llegado. Devuelve cuántos.
    pub fn wake_due(&mut self, now: u64) -> usize {
        let mut woken = 0;
        let mut i = 0;
        while i < self.sleeping.len() {
            if self.sleeping[i].0 <= now {
                let (_, tid) = self.sleeping.swap_remove(i);
                self.states.insert(tid, State::Ready);
                self.ready.push_back(tid);
                woken += 1;
            } else {
                i += 1;
            }
        }
        woken
    }

    /// El hilo actual de `cpu` termina. Devuelve el siguiente (siempre hay cambio).
    pub fn exit_current(&mut self, cpu: usize) -> Tid {
        assert!(!self.is_idle(cpu), "el hilo idle no puede terminar");
        let cur = self.cpus[cpu].current;
        self.pending_wake.remove(&cur);
        self.affinity.remove(&cur);
        self.switch(cpu, State::Terminated)
            .expect("exit siempre cambia de hilo")
    }

    /// Retira del registro un hilo terminado (para liberar sus recursos).
    pub fn reap(&mut self, tid: Tid) -> Result<(), SchedError> {
        match self.states.get(&tid) {
            None => Err(SchedError::UnknownThread),
            Some(State::Terminated) => {
                self.states.remove(&tid);
                Ok(())
            }
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// Hilos terminados pendientes de `reap`.
    pub fn terminated(&self) -> impl Iterator<Item = Tid> + '_ {
        self.states
            .iter()
            .filter(|(_, s)| **s == State::Terminated)
            .map(|(t, _)| *t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Tid = Tid(0);
    const IDLE: Tid = Tid(1);

    fn rr() -> RoundRobin {
        RoundRobin::new(MAIN, IDLE)
    }

    #[test]
    fn round_robin_order_is_fair_and_cyclic() {
        let mut s = rr();
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        assert_eq!((a, b), (Tid(2), Tid(3)));
        s.add(a);
        s.add(b);
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Ready));
        assert_eq!(s.state(a), Some(State::Running));
    }

    #[test]
    fn idle_runs_when_nothing_is_ready_and_is_never_queued() {
        let mut s = rr();
        assert_eq!(s.block_current(0), Some(IDLE));
        assert_eq!(s.current(0), IDLE);
        assert!(s.is_idle(0));
        assert_eq!(s.yield_current(0), None, "idle sigue si no hay nadie");
        assert_eq!(s.ready_len(), 0);
        s.unblock(MAIN).unwrap();
        assert!(s.has_ready_for(0));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert_eq!(s.state(IDLE), Some(State::Ready));
        assert_eq!(s.ready_len(), 0, "idle no se encola al ceder");
    }

    #[test]
    fn block_and_unblock() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Blocked));
        assert_eq!(s.unblock(MAIN), Ok(()));
        assert_eq!(s.unblock(MAIN), Err(SchedError::WrongState));
        assert_eq!(s.unblock(Tid(99)), Err(SchedError::UnknownThread));
        assert_eq!(s.yield_current(0), Some(MAIN));
    }

    #[test]
    fn sleep_wakes_at_tick_in_queue_order() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(0, 10), Some(a)); // MAIN duerme hasta 10
        assert_eq!(s.sleep_current(0, 5), Some(IDLE)); // a duerme hasta 5
        assert_eq!(s.wake_due(4), 0);
        assert_eq!(s.wake_due(5), 1);
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.state(MAIN), Some(State::Sleeping));
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.wake_due(100), 1);
        assert_eq!(s.yield_current(0), Some(MAIN));
    }

    #[test]
    fn wake_handles_blocked_and_sleeping() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(0, 100), Some(a)); // MAIN duerme
        assert_eq!(s.wake(MAIN), Ok(()), "dormido -> listo antes del plazo");
        assert_eq!(s.wake_due(100), 0, "ya no esta en la lista de dormidos");
        assert_eq!(s.wake(MAIN), Err(SchedError::WrongState), "ya listo");
        assert_eq!(s.block_current(0), Some(MAIN)); // a se bloquea
        assert_eq!(s.wake(a), Ok(()));
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.wake(Tid(77)), Err(SchedError::UnknownThread));
    }

    #[test]
    fn wake_of_running_thread_is_kept_until_it_blocks() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        // MAIN sigue Running: el despertar queda pendiente...
        assert_eq!(s.wake(MAIN), Ok(()));
        // ...y su bloqueo se convierte en cesión: vuelve a la cola.
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Ready));
        assert_eq!(s.yield_current(0), Some(MAIN));
        // Consumido: el siguiente bloqueo sí bloquea.
        assert_eq!(s.block_current(0), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Blocked));
        // Lo mismo con un sueño: no queda en la lista de dormidos.
        assert_eq!(s.wake(a), Ok(()));
        assert_eq!(
            s.sleep_current(0, 50),
            None,
            "a cede pero es el unico listo"
        );
        assert_eq!(s.state(a), Some(State::Running));
        assert_eq!(s.wake_due(50), 0);
    }

    #[test]
    fn exit_and_reap() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.yield_current(0), Some(a));
        assert_eq!(s.exit_current(0), MAIN);
        assert_eq!(s.state(a), Some(State::Terminated));
        assert_eq!(s.terminated().collect::<Vec<_>>(), [a]);
        assert_eq!(s.reap(MAIN), Err(SchedError::WrongState));
        assert_eq!(s.reap(a), Ok(()));
        assert_eq!(s.state(a), None);
        assert_eq!(s.thread_count(), 2);
    }

    #[test]
    #[should_panic(expected = "idle no puede terminar")]
    fn idle_cannot_exit() {
        let mut s = rr();
        s.block_current(0);
        s.exit_current(0);
    }

    #[test]
    fn cpus_share_one_ready_queue_and_have_their_own_idle() {
        let mut s = rr();
        let idle1 = s.allocate_tid();
        assert_eq!(s.add_cpu(2, idle1), Err(SchedError::UnknownCpu), "en orden");
        s.add_cpu(1, idle1).unwrap();
        assert_eq!(s.cpu_count(), 2);
        assert_eq!(s.current(1), idle1);
        assert!(s.is_idle(1));
        assert_eq!(s.state(idle1), Some(State::Running));
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        s.add(a);
        s.add(b);
        // La CPU 1 toma el primero de la cola; la 0 el segundo.
        assert_eq!(s.yield_current(1), Some(a));
        assert_eq!(s.state(idle1), Some(State::Ready));
        assert_eq!(s.ready_len(), 1);
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.ready_len(), 1, "main volvio a la cola");
        // a termina en la CPU 1: sigue main; luego idle1 al quedarse sin nada.
        assert_eq!(s.exit_current(1), MAIN);
        assert_eq!(s.block_current(1), Some(idle1));
        assert_eq!(s.yield_current(1), None);
        assert!(s.is_idle(1));
        assert_eq!(s.thread_count(), 5);
    }

    #[test]
    fn affinity_restricts_which_cpu_takes_a_thread() {
        let mut s = rr();
        let idle1 = s.allocate_tid();
        s.add_cpu(1, idle1).unwrap();
        let (a, b) = (s.allocate_tid(), s.allocate_tid());
        s.add(a);
        s.add(b);
        assert_eq!(s.set_affinity(a, Some(1)), Ok(()));
        assert_eq!(s.set_affinity(a, Some(7)), Err(SchedError::UnknownCpu));
        assert_eq!(
            s.set_affinity(Tid(99), Some(0)),
            Err(SchedError::UnknownThread)
        );
        assert_eq!(s.affinity(a), Some(1));
        assert!(!s.has_ready_for(0) || s.ready_len() == 2);
        // La CPU 0 salta a y toma b, aunque a fuera antes en la cola.
        assert_eq!(s.yield_current(0), Some(b));
        assert_eq!(s.yield_current(0), Some(MAIN));
        assert!(s.has_ready_for(1));
        assert_eq!(s.yield_current(1), Some(a));
        // Sin afinidad, cualquiera vale.
        s.set_affinity(a, None).unwrap();
        assert_eq!(s.affinity(a), None);
        assert_eq!(s.yield_current(1), Some(b));
        assert_eq!(s.yield_current(0), Some(a));
        // Un hilo con afinidad a otra CPU no impide que esta se quede en idle.
        s.set_affinity(b, Some(0)).unwrap();
        assert_eq!(s.exit_current(0), MAIN);
        // b cede en la CPU 1 y esta no puede volver a tomarlo: queda en idle.
        assert_eq!(s.yield_current(1), Some(idle1));
        assert_eq!(
            s.state(b),
            Some(State::Ready),
            "b sigue esperando a la CPU 0"
        );
        assert!(!s.has_ready_for(1));
        assert!(s.has_ready_for(0));
    }
}
