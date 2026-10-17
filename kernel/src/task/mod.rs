// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Kernel threads (Phases 9 and 10): stacks with a guard page, context
//! switching, preemptive round-robin scheduling, sleep and wait queues on top
//! of `aricode-sched`.

pub mod context;
pub mod scheduler;
pub mod stack;
pub mod thread;
pub mod wait;

pub use scheduler::init;
