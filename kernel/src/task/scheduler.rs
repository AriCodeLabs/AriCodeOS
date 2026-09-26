//! Planificador: tabla de hilos + política `aricode-sched` + cambio de contexto.
//!
//! Regla de bloqueo: el lock del planificador se toma solo para actualizar
//! la contabilidad; el cambio de contexto se hace con las interrupciones
//! deshabilitadas pero el lock ya soltado, de modo que el hilo que reanuda
//! no hereda un lock ajeno.
//!
//! Preempción (Fase 10): el handler del timer llama a `on_tick` tras el EOI.
//! Un cambio de contexto desde el handler deja el estado del hilo en su
//! propia pila (marco de interrupción + registros del trampolín); cuando
//! vuelve a ser elegido, retorna por el handler y el `iretq` restaura sus
//! flags. Todos los locks del kernel deshabilitan interrupciones, así que
//! ninguna sección crítica puede ser preemptada.

use super::context::switch_context;
use super::thread::Thread;
use crate::sync::{IrqLock, KernelIrq};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aricode_sched::{RoundRobin, State, Tid};
use aricode_sync::IrqPolicy;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use log::{debug, info};

struct Scheduler {
    policy: RoundRobin,
    threads: BTreeMap<Tid, Box<Thread>>,
}

static SCHED: IrqLock<Option<Scheduler>> = IrqLock::new(None);
static CURRENT: AtomicU32 = AtomicU32::new(0);
/// Ticks que le quedan al hilo actual antes de ser preemptado.
static QUANTUM_LEFT: AtomicU32 = AtomicU32::new(QUANTUM_TICKS);
static SWITCHES: AtomicU64 = AtomicU64::new(0);
static PREEMPTIONS: AtomicU64 = AtomicU64::new(0);

/// Quantum de CPU: 2 ticks = 20 ms a 100 Hz.
pub const QUANTUM_TICKS: u32 = 2;

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
    let kernel_cr3 = crate::memory::vmm::with(|m| m.root_phys());
    sched.threads.insert(
        MAIN_TID,
        Box::new(Thread::adopt_current(MAIN_TID, "main", kernel_cr3)),
    );
    let idle = Thread::new(IDLE_TID, "idle", idle_entry, 0, None, kernel_cr3).expect("hilo idle");
    sched.threads.insert(IDLE_TID, Box::new(idle));
    *SCHED.lock() = Some(sched);
    CURRENT.store(MAIN_TID.0, Ordering::Relaxed);
    info!(
        "task: hilo main adoptado, idle creado (pilas de {} KiB con guard); scheduler round robin preemptivo, quantum {} ms",
        super::stack::STACK_SIZE / 1024,
        QUANTUM_TICKS as u64 * crate::time::MS_PER_TICK
    );
}

/// Llamado desde el handler del timer (interrupciones deshabilitadas, EOI
/// ya enviado). Despierta a los dormidos y preempta si el quantum expiró.
/// Antes de `init` no hace nada.
pub fn on_tick(now: u64) {
    let woke = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else {
            return;
        };
        s.policy.wake_due(now)
    };
    let expired = QUANTUM_LEFT.fetch_sub(1, Ordering::Relaxed) <= 1;
    if expired || woke > 0 {
        PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
        switch_with(|p| p.preempt());
    }
}

/// Duerme el hilo actual hasta el tick `until` (o cede si ya pasó).
pub fn sleep_until(until: u64) {
    if crate::time::ticks() >= until {
        yield_now();
        return;
    }
    switch_with(|p| p.sleep_current(until));
}

pub fn sleep_ms(ms: u64) {
    sleep_until(crate::time::ticks() + ms.div_ceil(crate::time::MS_PER_TICK));
}

/// Bloquea el hilo actual hasta que alguien llame a `unblock`.
pub fn block_current() {
    switch_with(|p| p.block_current());
}

/// Despierta un hilo bloqueado o dormido antes de su plazo.
pub fn wake(tid: Tid) -> Result<(), aricode_sched::SchedError> {
    with(|s| s.policy.wake(tid))
}

#[derive(Debug, Clone, Copy)]
pub struct SchedStats {
    pub switches: u64,
    pub preemptions: u64,
}

pub fn stats() -> SchedStats {
    SchedStats {
        switches: SWITCHES.load(Ordering::Relaxed),
        preemptions: PREEMPTIONS.load(Ordering::Relaxed),
    }
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

/// Crea un hilo del kernel listo para ejecutar.
pub fn spawn(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
) -> Result<Tid, &'static str> {
    let cr3 = crate::memory::vmm::with(|m| m.root_phys());
    spawn_thread(name, entry, arg, None, cr3)
}

/// Prepara (sin encolar) el hilo principal de un proceso de usuario: arranca
/// en `entry` (ring 0, con el espacio `cr3` activo) y de ahí salta a ring 3.
/// El llamador registra el proceso y después llama a `start_thread`, de modo
/// que el hilo nunca ejecuta antes de que su proceso exista.
pub fn prepare_for_process(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    pid: u32,
    cr3: u64,
) -> Result<Box<Thread>, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    Ok(Box::new(Thread::new(
        tid,
        name,
        entry,
        arg,
        Some(pid),
        cr3,
    )?))
}

/// Encola un hilo preparado con `prepare_for_process`.
pub fn start_thread(thread: Box<Thread>) -> Tid {
    let tid = thread.tid;
    debug!("task: spawn {tid:?} '{}'", thread.name);
    with(|s| {
        s.threads.insert(tid, thread);
        s.policy.add(tid);
    });
    tid
}

fn spawn_thread(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    process: Option<u32>,
    cr3: u64,
) -> Result<Tid, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    let thread = Thread::new(tid, name, entry, arg, process, cr3)?;
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy.add(tid);
    });
    debug!("task: spawn {tid:?} '{name}'");
    Ok(tid)
}

/// Proceso del hilo actual.
pub fn current_process() -> Option<u32> {
    with(|s| s.threads.get(&s.policy.current()).and_then(|t| t.process))
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
            let nt = s.threads.get(&next).expect("hilo siguiente");
            (next, old, nt.saved_rsp, nt.kernel_stack_top(), nt.cr3)
        })
    });
    if let Some((next, old, new, kstack, cr3)) = switch {
        CURRENT.store(next.0, Ordering::Relaxed);
        QUANTUM_LEFT.store(QUANTUM_TICKS, Ordering::Relaxed);
        SWITCHES.fetch_add(1, Ordering::Relaxed);
        // Entradas a ring 0 desde ring 3 del hilo siguiente: su pila de kernel.
        crate::arch::x86_64::gdt::set_kernel_stack(kstack);
        crate::arch::x86_64::syscall::set_kernel_stack(kstack);
        // Espacio de direcciones del hilo siguiente (la mitad del kernel es
        // común, así que el cambio es seguro desde cualquier pila de kernel).
        let current_cr3: u64;
        // SAFETY: lectura de CR3.
        unsafe { core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack)) };
        if cr3 != current_cr3 {
            // SAFETY: `cr3` es un PML4 construido por el VMM o por `process::spawn`.
            unsafe { core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack)) };
        }
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
        if let Some(pid) = z.process {
            crate::user::process::release_address_space(pid);
        }
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
pub fn threads_selftest() {
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

/// Autoprueba preemptiva (`test=sched`): hilos que nunca ceden reparten la
/// CPU, el sueño planificado respeta el tiempo, y bloqueo/despertar funcionan
/// a través de una cola de espera.
pub fn selftest() {
    use super::wait::WaitQueue;
    use core::sync::atomic::{AtomicBool, AtomicU64};

    // 1. Equidad: tres hilos ocupados sin yield durante 300 ms.
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTERS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    extern "C" fn busy(i: usize) {
        while !STOP.load(Ordering::Relaxed) {
            COUNTERS[i].fetch_add(1, Ordering::Relaxed);
            core::hint::spin_loop();
        }
    }
    let switches_before = stats().switches;
    let busy_tids: Vec<Tid> = (0..3).map(|i| spawn("busy", busy, i).unwrap()).collect();
    sleep_ms(300);
    STOP.store(true, Ordering::Relaxed);
    while busy_tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let counts: Vec<u64> = COUNTERS.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let (min, max) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
    assert!(min > 0, "un hilo ocupado no llego a ejecutar: {counts:?}");
    assert!(min * 2 >= max, "reparto injusto: {counts:?}");
    let switches = stats().switches - switches_before;
    assert!(
        switches >= 20,
        "pocos cambios de contexto en 300 ms: {switches}"
    );

    // 2. Sueño planificado: un hilo duerme 50 ms y anota cuando despierta.
    static WOKE_AT: AtomicU64 = AtomicU64::new(0);
    extern "C" fn sleeper(_: usize) {
        sleep_ms(50);
        WOKE_AT.store(crate::time::ticks(), Ordering::Relaxed);
    }
    let t0 = crate::time::ticks();
    let st = spawn("sleeper", sleeper, 0).unwrap();
    while state(st).is_some() {
        yield_now();
    }
    let slept = WOKE_AT.load(Ordering::Relaxed) - t0;
    assert!(
        (5..=7).contains(&slept),
        "sleep_ms(50) durmio {slept} ticks"
    );

    // 3. Bloqueo y despertar por cola de espera: dos consumidores, `wake_one`
    // libera exactamente uno y `wake_all` al resto.
    static QUEUE: WaitQueue = WaitQueue::new();
    static READY_FLAG: AtomicU64 = AtomicU64::new(0);
    static CONSUMED: AtomicU64 = AtomicU64::new(0);
    extern "C" fn consumer(i: usize) {
        QUEUE.wait_until(|| READY_FLAG.load(Ordering::Relaxed) & (1 << i) != 0);
        CONSUMED.fetch_add(1, Ordering::Relaxed);
    }
    let c0 = spawn("consumer-0", consumer, 0).unwrap();
    let c1 = spawn("consumer-1", consumer, 1).unwrap();
    sleep_ms(30);
    assert_eq!(state(c0), Some(State::Blocked));
    assert_eq!(state(c1), Some(State::Blocked));
    assert_eq!(CONSUMED.load(Ordering::Relaxed), 0);
    READY_FLAG.store(0b01, Ordering::Relaxed);
    QUEUE.wake_one(); // despierta a c0, que fue el primero en esperar
    sleep_ms(30);
    assert_eq!(
        CONSUMED.load(Ordering::Relaxed),
        1,
        "wake_one debe liberar solo a uno"
    );
    assert_eq!(
        state(c1),
        Some(State::Blocked),
        "el segundo sigue esperando"
    );
    READY_FLAG.store(0b11, Ordering::Relaxed);
    QUEUE.wake_all();
    while state(c0).is_some() || state(c1).is_some() {
        yield_now();
    }
    assert_eq!(CONSUMED.load(Ordering::Relaxed), 2);

    let st = stats();
    info!(
        "sched selftest ok (busy {counts:?} en 300 ms, {switches} cambios, {} preempciones, sleep 50 ms -> {slept} ticks, wait_one/wake_all)",
        st.preemptions
    );
}
