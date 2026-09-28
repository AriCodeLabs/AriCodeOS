//! Supervisión de servicios (etapa 1 del plan posterior al roadmap):
//! el contrato común de vida de un servicio que `init` aplica a cada driver
//! y servicio residente. La política no sabe de procesos ni de IPC: recibe
//! hechos (`starting`, `ready`, `died`, `tick`) y devuelve la acción que
//! toca (`Restart`, `GiveUp`, `Timeout`). Así se prueba en host.
//!
//! - Estados: `Stopped`, `Starting`, `Ready`, `Degraded`, `Failed`.
//! - Generación: cada arranque tiene un número nuevo; un cliente que guarde
//!   la generación sabe si el servicio con el que hablaba es otro.
//! - Presupuesto de reinicios con espera creciente (se dobla hasta un tope)
//!   y que se recupera si el servicio se mantiene estable un tiempo.
//! - Plazo de arranque: un servicio que no anuncia `ready` a tiempo cuenta
//!   como muerto.
//! - Un servicio opcional que agota su presupuesto deja al sistema en modo
//!   degradado; uno obligatorio lo deja en `Failed`.

#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Ready,
    /// Sigue vivo pero con una función perdida (lo anuncia el propio servicio o init).
    Degraded,
    /// Sin presupuesto de reinicios: no se vuelve a intentar.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(u64),
    Killed,
    /// No anunció `ready` dentro del plazo de arranque.
    StartTimeout,
}

/// Lo que el supervisor debe hacer tras un hecho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Volver a lanzar el servicio en el instante indicado (ms).
    Restart {
        at: u64,
    },
    /// Ya toca lanzarlo (llamar a `starting` tras crear el proceso).
    Start,
    /// Presupuesto agotado: el servicio queda `Failed`.
    GiveUp,
    /// El arranque excedió su plazo: tratar como muerte (`died(StartTimeout)`).
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Reinicios permitidos antes de rendirse.
    pub max_restarts: u32,
    pub backoff_initial_ms: u64,
    pub backoff_max_ms: u64,
    /// Tiempo máximo entre `starting` y `ready`.
    pub start_timeout_ms: u64,
    /// Tras este tiempo en `Ready` el presupuesto vuelve a estar completo.
    pub stable_ms: u64,
}

impl Policy {
    pub const DRIVER: Policy = Policy {
        max_restarts: 5,
        backoff_initial_ms: 100,
        backoff_max_ms: 5_000,
        start_timeout_ms: 5_000,
        stable_ms: 60_000,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub generation: u32,
    pub restarts: u32,
    pub last_exit: Option<Exit>,
    pub backoff_ms: u64,
    /// Instante (ms) del último `ready`, si lo hubo.
    pub ready_at: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Service {
    policy: Policy,
    optional: bool,
    state: State,
    generation: u32,
    restarts: u32,
    last_exit: Option<Exit>,
    started_at: u64,
    ready_at: Option<u64>,
    backoff_ms: u64,
    restart_at: Option<u64>,
    budget_reset: bool,
}

impl Service {
    pub const fn new(policy: Policy, optional: bool) -> Self {
        Self {
            policy,
            optional,
            state: State::Stopped,
            generation: 0,
            restarts: 0,
            last_exit: None,
            started_at: 0,
            ready_at: None,
            backoff_ms: 0,
            restart_at: None,
            budget_reset: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    pub fn status(&self) -> Status {
        Status {
            state: self.state,
            generation: self.generation,
            restarts: self.restarts,
            last_exit: self.last_exit,
            backoff_ms: self.backoff_ms,
            ready_at: self.ready_at,
        }
    }

    /// El proceso acaba de crearse: nueva generación, plazo de arranque en marcha.
    pub fn starting(&mut self, now: u64) -> u32 {
        self.generation += 1;
        self.state = State::Starting;
        self.started_at = now;
        self.ready_at = None;
        self.restart_at = None;
        self.budget_reset = false;
        self.generation
    }

    /// El servicio responde (sonda o anuncio propio).
    pub fn ready(&mut self, now: u64) {
        if matches!(self.state, State::Starting | State::Degraded) {
            self.state = State::Ready;
            self.ready_at = Some(now);
        }
    }

    /// Retirar el servicio a propósito: queda `Stopped` sin reinicio pendiente.
    pub fn stop(&mut self) {
        self.state = State::Stopped;
        self.restart_at = None;
        self.ready_at = None;
    }

    /// Vivo, pero con una función perdida.
    pub fn degraded(&mut self) {
        if matches!(self.state, State::Starting | State::Ready) {
            self.state = State::Degraded;
        }
    }

    /// El proceso terminó (o su arranque venció). Decide si se reinicia.
    pub fn died(&mut self, now: u64, exit: Exit) -> Action {
        self.last_exit = Some(exit);
        self.ready_at = None;
        self.restarts += 1;
        if self.restarts > self.policy.max_restarts {
            self.state = State::Failed;
            self.restart_at = None;
            return Action::GiveUp;
        }
        let shift = (self.restarts - 1).min(20);
        self.backoff_ms = self
            .policy
            .backoff_initial_ms
            .saturating_mul(1u64 << shift)
            .min(self.policy.backoff_max_ms);
        self.state = State::Stopped;
        let at = now + self.backoff_ms;
        self.restart_at = Some(at);
        Action::Restart { at }
    }

    /// Paso de tiempo: reinicio pendiente que ya toca, plazo de arranque
    /// vencido o estabilidad alcanzada (presupuesto restaurado).
    pub fn tick(&mut self, now: u64) -> Action {
        match self.state {
            State::Stopped => match self.restart_at {
                Some(at) if now >= at => {
                    self.restart_at = None;
                    Action::Start
                }
                _ => Action::Nothing,
            },
            State::Starting
                if now.saturating_sub(self.started_at) >= self.policy.start_timeout_ms =>
            {
                Action::Timeout
            }
            State::Ready => {
                if !self.budget_reset
                    && self
                        .ready_at
                        .is_some_and(|r| now.saturating_sub(r) >= self.policy.stable_ms)
                {
                    self.restarts = 0;
                    self.backoff_ms = 0;
                    self.budget_reset = true;
                }
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    /// Cómo afecta al sistema un servicio en `Failed`: opcional -> degradado,
    /// obligatorio -> fallo.
    pub fn system_state_if_failed(&self) -> State {
        if self.optional {
            State::Degraded
        } else {
            State::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            max_restarts: 200,
            backoff_initial_ms: 10,
            backoff_max_ms: 160,
            start_timeout_ms: 1000,
            stable_ms: 5000,
        }
    }

    #[test]
    fn hundred_restarts_with_capped_backoff() {
        let mut s = Service::new(policy(), false);
        let mut now = 0;
        let mut generations = alloc_free_vec();
        let g = s.starting(now);
        generations.push(g);
        s.ready(now);
        for i in 1..=100u32 {
            now += 50;
            match s.died(now, Exit::Killed) {
                Action::Restart { at } => {
                    let expected = (10u64 << (i - 1).min(20)).min(160);
                    assert_eq!(at - now, expected, "espera del reinicio {i}");
                    assert_eq!(s.tick(at - 1), Action::Nothing);
                    assert_eq!(s.tick(at), Action::Start);
                    now = at;
                    generations.push(s.starting(now));
                    s.ready(now);
                }
                other => panic!("reinicio {i}: {other:?}"),
            }
        }
        assert_eq!(s.status().restarts, 100);
        assert_eq!(s.generation(), 101);
        assert!(generations.windows(2).all(|w| w[1] == w[0] + 1));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn budget_exhaustion_fails_and_never_restarts() {
        let mut s = Service::new(
            Policy {
                max_restarts: 2,
                ..policy()
            },
            true,
        );
        s.starting(0);
        assert!(matches!(s.died(0, Exit::Code(3)), Action::Restart { .. }));
        s.starting(10);
        assert!(matches!(s.died(10, Exit::Code(3)), Action::Restart { .. }));
        s.starting(40);
        assert_eq!(s.died(40, Exit::Killed), Action::GiveUp);
        assert_eq!(s.state(), State::Failed);
        assert_eq!(s.tick(100_000), Action::Nothing);
        assert_eq!(s.system_state_if_failed(), State::Degraded, "opcional");
        assert_eq!(s.status().last_exit, Some(Exit::Killed));
        let mandatory = Service::new(policy(), false);
        assert_eq!(mandatory.system_state_if_failed(), State::Failed);
    }

    #[test]
    fn start_timeout_counts_as_death() {
        let mut s = Service::new(policy(), false);
        s.starting(0);
        assert_eq!(s.tick(999), Action::Nothing);
        assert_eq!(s.tick(1000), Action::Timeout);
        assert!(matches!(
            s.died(1000, Exit::StartTimeout),
            Action::Restart { at: 1010 }
        ));
        assert_eq!(s.status().last_exit, Some(Exit::StartTimeout));
    }

    #[test]
    fn stable_service_recovers_its_budget() {
        let mut s = Service::new(policy(), false);
        s.starting(0);
        s.died(0, Exit::Killed);
        s.starting(10);
        s.died(10, Exit::Killed);
        s.starting(30);
        s.ready(30);
        assert_eq!(s.status().restarts, 2);
        assert_eq!(s.tick(30 + 4999), Action::Nothing);
        assert_eq!(s.status().restarts, 2);
        assert_eq!(s.tick(30 + 5000), Action::Nothing);
        assert_eq!(s.status().restarts, 0, "presupuesto restaurado");
        assert_eq!(s.status().backoff_ms, 0);
        // La siguiente muerte vuelve a la espera inicial.
        assert!(matches!(
            s.died(6000, Exit::Killed),
            Action::Restart { at: 6010 }
        ));
    }

    #[test]
    fn stopped_on_purpose_is_not_restarted() {
        let mut s = Service::new(policy(), true);
        s.starting(0);
        assert!(matches!(
            s.died(0, Exit::Killed),
            Action::Restart { at: 10 }
        ));
        s.stop();
        assert_eq!(s.state(), State::Stopped);
        assert_eq!(s.tick(10), Action::Nothing);
        assert_eq!(s.tick(10_000), Action::Nothing);
        // Un nuevo arranque explicito sigue siendo posible.
        assert_eq!(s.starting(20), 2);
    }

    #[test]
    fn degraded_and_ready_transitions() {
        let mut s = Service::new(policy(), true);
        assert_eq!(s.state(), State::Stopped);
        s.ready(0);
        assert_eq!(
            s.state(),
            State::Stopped,
            "ready sin starting no cambia nada"
        );
        s.starting(0);
        s.degraded();
        assert_eq!(s.state(), State::Degraded);
        s.ready(5);
        assert_eq!(s.state(), State::Ready);
        assert_eq!(s.status().ready_at, Some(5));
    }

    extern crate std;
    fn alloc_free_vec() -> std::vec::Vec<u32> {
        std::vec::Vec::new()
    }
}
