//! Hilos del kernel (Fase 9): pilas con guard page, cambio de contexto y
//! planificación cooperativa sobre `aricode-sched`.

pub mod context;
pub mod scheduler;
pub mod stack;
pub mod thread;

pub use scheduler::init;
