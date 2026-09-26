//! Planificador: tabla de hilos + política `aricode-sched` + cambio de contexto.
//!
//! Regla de bloqueo: el lock del planificador se toma solo para actualizar
//! la contabilidad; el cambio de contexto se hace con las interrupciones
//! deshabilitadas pero el lock ya soltado, de modo que el hilo que reanuda
//! no hereda un lock ajeno.

use super::context::switch_context;
use super::thread::Thread;
use crate::sync::{IrqLock, KernelIrq};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aricode_sched::{RoundRobin, State, Tid};
use aricode_sync::IrqPolicy;
use core::sync::atomic::{AtomicU32, Ordering};
use log::{debug, info};

struct Scheduler {
    policy: RoundRobin,
    threads: BTreeMap<Tid, Box<Thread>>,
}

static SCHED: IrqLock<Option<Scheduler>> = IrqLock::new(None);
static CURRENT: AtomicU32 = AtomicU32::new(0);

pub const MAIN_TID: Tid = Tid(0);
pub const IDLE_TID: Tid = Tid(1);

fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    let mut guard = SCHED.lock();
    f(guard.as_mut().expect("scheduler no inicializado"))
}

/// Adopta el contexto de arranque como hilo `main` y crea el hilo idle.
pub fn init() {
    let mut sched = Scheduler {
        policy: RoundRobin::new(MAIN_TID, IDLE_TID),
        threads: BTreeMap::new(),
    };
    sched
        .threads
        .insert(MAIN_TID, Box::new(Thread::adopt_current(MAIN_TID, "main")));
    let idle = Thread::new(IDLE_TID, "idle", idle_entry, 0).expect("hilo idle");
    sched.threads.insert(IDLE_TID, Box::new(idle));
    *SCHED.lock() = Some(sched);
    CURRENT.store(MAIN_TID.0, Ordering::Relaxed);
    info!(
        "task: hilo main adoptado, idle creado (pilas de {} KiB con guard)",
        super::stack::STACK_SIZE / 1024
    );
}

extern "C" fn idle_entry(_: usize) {
    loop {
        // SAFETY: con interrupciones habilitadas, `hlt` espera al siguiente evento.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
        yield_now();
    }
}

pub fn current_tid() -> Tid {
    Tid(CURRENT.load(Ordering::Relaxed))
}

/// Crea un hilo listo para ejecutar.
pub fn spawn(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
) -> Result<Tid, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    let thread = Thread::new(tid, name, entry, arg)?;
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy.add(tid);
    });
    debug!("task: spawn {tid:?} '{name}'");
    Ok(tid)
}

/// Estado de un hilo, si existe.
pub fn state(tid: Tid) -> Option<State> {
    with(|s| s.policy.state(tid))
}

/// Cede la CPU al siguiente hilo listo.
pub fn yield_now() {
    switch_with(|p| p.yield_current());
}

/// Termina el hilo actual. Nunca retorna.
pub fn exit() -> ! {
    switch_with(|p| Some(p.exit_current()));
    unreachable!("un hilo terminado no reanuda");
}

#[no_mangle]
extern "C" fn thread_exit_trampoline() -> ! {
    exit()
}

/// Aplica `decide` a la política y, si hay cambio, conmuta de contexto.
fn switch_with(decide: impl FnOnce(&mut RoundRobin) -> Option<Tid>) {
    let irq = KernelIrq::save_and_disable();
    let switch = with(|s| {
        let prev = s.policy.current();
        decide(&mut s.policy).map(|next| {
            let old = &mut s.threads.get_mut(&prev).expect("hilo actual").saved_rsp as *mut u64;
            let new = s.threads.get(&next).expect("hilo siguiente").saved_rsp;
            (next, old, new)
        })
    });
    if let Some((next, old, new)) = switch {
        CURRENT.store(next.0, Ordering::Relaxed);
        // SAFETY: `old` apunta al campo de un `Box<Thread>` que no se mueve ni
        // se libera mientras el hilo exista; `new` fue guardado por un
        // `switch_context` previo o preparado por `initial_stack_frame`.
        unsafe { switch_context(old, new) };
        // Al reanudar aquí, otro hilo puede haber terminado: liberar sus pilas.
        reap();
    }
    KernelIrq::restore(irq);
}

/// Libera los hilos terminados. Corre siempre en un hilo distinto al que se
/// libera, así que soltar su pila es seguro.
fn reap() {
    let zombies: Vec<Box<Thread>> = with(|s| {
        let dead: Vec<Tid> = s.policy.terminated().collect();
        dead.into_iter()
            .filter_map(|t| {
                s.policy.reap(t).ok()?;
                s.threads.remove(&t)
            })
            .collect()
    });
    for z in zombies {
        debug!("task: reap {:?} '{}'", z.tid, z.name);
        drop(z);
    }
}

/// `true` si `addr` cae en la guard page de la pila de algún hilo. Se usa
/// desde el handler de excepciones: `try_lock` para no bloquear si el fallo
/// ocurrió dentro del propio planificador.
pub fn in_thread_stack_guard(addr: u64) -> bool {
    let Some(guard) = SCHED.try_lock() else {
        return false;
    };
    guard.as_ref().is_some_and(|s| {
        s.threads
            .values()
            .any(|t| t.stack.as_ref().is_some_and(|st| st.in_guard(addr)))
    })
}

pub fn thread_count() -> usize {
    with(|s| s.policy.thread_count())
}

/// Autoprueba cooperativa (`test=threads`): dos hilos alternan estrictamente
/// con `yield`, y un lote de hilos termina y libera sus pilas.
pub fn selftest() {
    use crate::memory::pmm;
    use core::sync::atomic::AtomicU64;

    static LOG: IrqLock<Vec<u8>> = IrqLock::new(Vec::new());
    extern "C" fn ping(arg: usize) {
        for _ in 0..3 {
            LOG.lock().push(arg as u8);
            yield_now();
        }
    }
    assert_eq!(current_tid(), MAIN_TID, "la autoprueba corre en main");
    let a = spawn("ping-a", ping, b'A' as usize).unwrap();
    let b = spawn("ping-b", ping, b'B' as usize).unwrap();
    while state(a).is_some() || state(b).is_some() {
        yield_now();
    }
    let log = LOG.lock().clone();
    assert_eq!(
        &log[..],
        b"ABABAB",
        "los dos hilos deben alternar: {:?}",
        core::str::from_utf8(&log)
    );

    static DONE: AtomicU64 = AtomicU64::new(0);
    extern "C" fn worker(_: usize) {
        for _ in 0..10 {
            yield_now();
        }
        DONE.fetch_add(1, Ordering::Relaxed);
    }
    let free_before = pmm::stats().free_frames;
    const N: usize = 40;
    let tids: Vec<Tid> = (0..N)
        .map(|i| spawn("worker", worker, i).unwrap())
        .collect();
    while tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    assert_eq!(DONE.load(Ordering::Relaxed), N as u64);
    let free_after = pmm::stats().free_frames;
    // Las pilas se devuelven; solo pueden quedar tablas de páginas nuevas.
    assert!(
        free_before - free_after <= 8,
        "frames no devueltos tras reap: {}",
        free_before - free_after
    );
    assert_eq!(thread_count(), 2, "solo main e idle");
    assert_eq!(current_tid(), MAIN_TID);
    info!(
        "threads selftest ok (ABABAB, {N} workers terminados y recogidos, {} frames en tablas)",
        free_before - free_after
    );
}
