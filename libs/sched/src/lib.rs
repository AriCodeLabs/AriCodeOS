//! Política de planificación (Fases 9 y 10): round robin sobre identificadores
//! de hilo, con estados, sueño por tick, bloqueo y salida.
//!
//! No sabe nada de pilas ni registros: el kernel le pregunta "¿quién sigue?"
//! y hace el cambio de contexto. Así la política se prueba en host de forma
//! determinista.

#![no_std]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
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
}

pub struct RoundRobin {
    states: BTreeMap<Tid, State>,
    ready: VecDeque<Tid>,
    /// (tick de despertar, hilo), sin orden particular.
    sleeping: Vec<(u64, Tid)>,
    current: Tid,
    idle: Tid,
    next_tid: u32,
}

impl RoundRobin {
    /// `current` es el hilo que ya está ejecutando (el contexto de arranque);
    /// `idle` es el que se elige cuando no hay nada listo. Ambos quedan
    /// registrados: `current` como `Running`, `idle` como `Ready` pero fuera
    /// de la cola (nunca se encola).
    pub fn new(current: Tid, idle: Tid) -> Self {
        let mut states = BTreeMap::new();
        states.insert(current, State::Running);
        states.insert(idle, State::Ready);
        Self {
            states,
            ready: VecDeque::new(),
            sleeping: Vec::new(),
            current,
            idle,
            next_tid: current.0.max(idle.0) + 1,
        }
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

    pub fn current(&self) -> Tid {
        self.current
    }
    pub fn idle(&self) -> Tid {
        self.idle
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

    /// Siguiente hilo a ejecutar tras dejar `current` en `state`.
    /// Devuelve `None` si no hay cambio (el mismo hilo sigue).
    fn switch(&mut self, current_state: State) -> Option<Tid> {
        let cur = self.current;
        if current_state == State::Ready && cur != self.idle {
            self.ready.push_back(cur);
        }
        self.states.insert(cur, current_state);
        let next = self.ready.pop_front().unwrap_or(self.idle);
        self.states.insert(next, State::Running);
        self.current = next;
        if next == cur {
            None
        } else {
            Some(next)
        }
    }

    /// El hilo actual cede la CPU (vuelve al final de la cola).
    pub fn yield_current(&mut self) -> Option<Tid> {
        self.switch(State::Ready)
    }

    /// Preempción por fin de quantum: igual que `yield`, pero el idle no se encola.
    pub fn preempt(&mut self) -> Option<Tid> {
        self.switch(State::Ready)
    }

    /// El hilo actual se bloquea hasta que alguien lo despierte con `unblock`.
    pub fn block_current(&mut self) -> Option<Tid> {
        self.switch(State::Blocked)
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
    /// `wake` anticipado lo saca de la lista de dormidos.
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
            Some(_) => Err(SchedError::WrongState),
        }
    }

    /// El hilo actual duerme hasta el tick `until`.
    pub fn sleep_current(&mut self, until: u64) -> Option<Tid> {
        let cur = self.current;
        self.sleeping.push((until, cur));
        self.switch(State::Sleeping)
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

    /// El hilo actual termina. Devuelve el siguiente (siempre hay cambio).
    pub fn exit_current(&mut self) -> Tid {
        assert!(self.current != self.idle, "el hilo idle no puede terminar");
        self.switch(State::Terminated)
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
        assert_eq!(s.yield_current(), Some(a));
        assert_eq!(s.yield_current(), Some(b));
        assert_eq!(s.yield_current(), Some(MAIN));
        assert_eq!(s.yield_current(), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Ready));
        assert_eq!(s.state(a), Some(State::Running));
    }

    #[test]
    fn idle_runs_when_nothing_is_ready_and_is_never_queued() {
        let mut s = rr();
        assert_eq!(s.block_current(), Some(IDLE));
        assert_eq!(s.current(), IDLE);
        assert_eq!(s.yield_current(), None, "idle sigue si no hay nadie");
        assert_eq!(s.ready_len(), 0);
        s.unblock(MAIN).unwrap();
        assert_eq!(s.yield_current(), Some(MAIN));
        assert_eq!(s.state(IDLE), Some(State::Ready));
        assert_eq!(s.ready_len(), 0, "idle no se encola al ceder");
    }

    #[test]
    fn block_and_unblock() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.block_current(), Some(a));
        assert_eq!(s.state(MAIN), Some(State::Blocked));
        assert_eq!(s.unblock(MAIN), Ok(()));
        assert_eq!(s.unblock(MAIN), Err(SchedError::WrongState));
        assert_eq!(s.unblock(Tid(99)), Err(SchedError::UnknownThread));
        assert_eq!(s.yield_current(), Some(MAIN));
    }

    #[test]
    fn sleep_wakes_at_tick_in_queue_order() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(10), Some(a)); // MAIN duerme hasta 10
        assert_eq!(s.sleep_current(5), Some(IDLE)); // a duerme hasta 5
        assert_eq!(s.wake_due(4), 0);
        assert_eq!(s.wake_due(5), 1);
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.state(MAIN), Some(State::Sleeping));
        assert_eq!(s.yield_current(), Some(a));
        assert_eq!(s.wake_due(100), 1);
        assert_eq!(s.yield_current(), Some(MAIN));
    }

    #[test]
    fn wake_handles_blocked_and_sleeping() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.sleep_current(100), Some(a)); // MAIN duerme
        assert_eq!(s.wake(MAIN), Ok(()), "dormido -> listo antes del plazo");
        assert_eq!(s.wake_due(100), 0, "ya no esta en la lista de dormidos");
        assert_eq!(s.wake(MAIN), Err(SchedError::WrongState), "ya listo");
        assert_eq!(s.block_current(), Some(MAIN)); // a se bloquea
        assert_eq!(s.wake(a), Ok(()));
        assert_eq!(s.state(a), Some(State::Ready));
        assert_eq!(s.wake(Tid(77)), Err(SchedError::UnknownThread));
    }

    #[test]
    fn exit_and_reap() {
        let mut s = rr();
        let a = s.allocate_tid();
        s.add(a);
        assert_eq!(s.yield_current(), Some(a));
        assert_eq!(s.exit_current(), MAIN);
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
        s.block_current();
        s.exit_current();
    }
}
