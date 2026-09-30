//! Planificador: tabla de hilos + política `aricode-sched` + cambio de contexto.
//!
//! Regla de bloqueo (Fase 32): el lock del planificador se toma para decidir
//! y se mantiene tomado durante el cambio de contexto; lo suelta el hilo que
//! reanuda (`force_unlock`), que no tiene el guard. Así ninguna otra CPU
//! puede elegir al hilo saliente antes de que su `rsp` esté guardado. El
//! cambio se hace con las interrupciones deshabilitadas; el hilo que reanuda
//! restaura las suyas.
//!
//! Preempción (Fase 10): el handler del timer llama a `on_tick` tras el EOI.
//! Un cambio de contexto desde el handler deja el estado del hilo en su
//! propia pila (marco de interrupción + registros del trampolín); cuando
//! vuelve a ser elegido, retorna por el handler y el `iretq` restaura sus
//! flags. Todos los locks del kernel deshabilitan interrupciones, así que
//! ninguna sección crítica puede ser preemptada.
//!
//! SMP (Fase 32): una cola de listos compartida; cada CPU tiene su hilo
//! actual y su idle (`cpu::Cpu`). Solo el BSP cuenta el tiempo y despierta a
//! los dormidos; cuando un hilo pasa a listo y otra CPU está en idle se le
//! envía una IPI para que lo tome sin esperar a su siguiente tick.

use super::context::switch_context;
use super::thread::Thread;
use crate::arch::x86_64::apic;
use crate::cpu;
use crate::sync::{IrqLock, KernelIrq};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aricode_sched::{RoundRobin, State, Tid};
use aricode_sync::IrqPolicy;
use core::sync::atomic::{AtomicU64, Ordering};
use log::{debug, info};

struct Scheduler {
    policy: RoundRobin,
    threads: BTreeMap<Tid, Box<Thread>>,
}

static SCHED: IrqLock<Option<Scheduler>> = IrqLock::new(None);
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

/// Adopta el contexto de arranque como hilo `main` y crea el hilo idle del BSP.
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
    let bsp = cpu::slot(0);
    bsp.set_idle_tid(IDLE_TID.0);
    bsp.set_current(MAIN_TID.0, QUANTUM_TICKS);
    info!(
        "task: hilo main adoptado, idle creado (pilas de {} KiB con guard); scheduler round robin preemptivo, quantum {} ms",
        super::stack::STACK_SIZE / 1024,
        QUANTUM_TICKS as u64 * crate::time::MS_PER_TICK
    );
}

/// Crea el hilo idle de la CPU `idx` (Fase 32), que ese AP adopta como su
/// hilo en ejecución: devuelve la cima de su pila para arrancarlo ahí.
pub fn create_idle_for_cpu(idx: usize) -> Result<u64, &'static str> {
    let kernel_cr3 = crate::memory::vmm::with(|m| m.root_phys());
    let tid = with(|s| s.policy.allocate_tid());
    let stack = super::stack::KernelStack::allocate(tid.0)?;
    let top = stack.top();
    let mut name = alloc::string::String::from("idle-");
    name.push_str(&alloc::format!("{idx}"));
    let thread = Thread::adopt_on_stack(tid, &name, stack, kernel_cr3);
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy
            .add_cpu(idx, tid)
            .map_err(|_| "scheduler: CPU fuera de orden")
    })?;
    cpu::slot(idx).set_idle_tid(tid.0);
    Ok(top)
}

/// El AP `idx` empieza a planificar: su idle es el hilo actual.
pub fn adopt_idle(idx: usize) {
    let c = cpu::slot(idx);
    let idle = Tid(c.idle_tid());
    let top = with(|s| {
        s.threads
            .get(&idle)
            .expect("idle del AP")
            .kernel_stack_top()
    });
    c.set_current(idle.0, QUANTUM_TICKS);
    crate::arch::x86_64::gdt::set_kernel_stack(top);
}

/// Llamado desde el handler del timer (interrupciones deshabilitadas, EOI
/// ya enviado). En el BSP despierta a los dormidos; en todas, preempta si el
/// quantum expiró. Antes de `init` no hace nada.
pub fn on_tick(now: u64, bsp: bool, from_user: bool) {
    // Un hilo de usuario que no hace syscalls (bucle infinito) solo pasa por
    // aquí: si su proceso tiene una terminación pendiente, termina ya, igual
    // que ante una excepción (auditoría 6ee19fb, hallazgo 4).
    if from_user && crate::user::process::current_kill_requested() {
        crate::user::process::kill_current(crate::user::process::KILLED_BY_SUPERVISOR);
    }
    let woke = if bsp {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else {
            return;
        };
        s.policy.wake_due(now)
    } else {
        0
    };
    if woke > 0 {
        kick_idle();
    }
    let expired = cpu::current().quantum_tick();
    if expired || woke > 0 {
        PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
        switch_with(|p, c| p.preempt(c));
    }
}

/// Si otra CPU está en idle, la saca de `hlt` para que mire la cola.
fn kick_idle() {
    if let Some(c) = cpu::idle_other_than(cpu::id()) {
        apic::send_ipi(c.lapic_id(), apic::RESCHED_VECTOR);
    }
}

/// Duerme el hilo actual hasta el tick `until` (o cede si ya pasó).
pub fn sleep_until(until: u64) {
    if crate::time::ticks() >= until {
        yield_now();
        return;
    }
    switch_with(|p, c| p.sleep_current(c, until));
}

/// Duerme al menos `ms`. Un despertar anticipado (un `wake` espurio de una
/// cola en la que este hilo esperó antes) vuelve a dormir hasta el plazo.
pub fn sleep_ms(ms: u64) {
    // Un tick más: el actual ya ha transcurrido en parte, y "al menos `ms`"
    // se mide con el reloj de µs, no con el tick (etapa 5).
    let until = crate::time::ticks() + ms.div_ceil(crate::time::MS_PER_TICK) + 1;
    loop {
        sleep_until(until);
        // Un despertar anticipado por `process_kill` no vuelve a dormir: la
        // syscall termina el proceso al salir (auditoría 6ee19fb, 4).
        if crate::time::ticks() >= until || crate::user::process::current_kill_requested() {
            return;
        }
    }
}

/// Bloquea el hilo actual hasta que alguien llame a `wake`.
pub fn block_current() {
    switch_with(|p, c| p.block_current(c));
}

/// Despierta un hilo bloqueado o dormido antes de su plazo (o deja el
/// despertar pendiente si aún está ejecutando en otra CPU).
pub fn wake(tid: Tid) -> Result<(), aricode_sched::SchedError> {
    let r = with(|s| s.policy.wake(tid));
    if r.is_ok() {
        kick_idle();
    }
    r
}

/// Fija la CPU en la que `tid` puede ejecutar (`None` = cualquiera).
pub fn set_affinity(tid: Tid, cpu: Option<usize>) -> Result<(), aricode_sched::SchedError> {
    with(|s| s.policy.set_affinity(tid, cpu))
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
    idle_loop()
}

/// Bucle idle de una CPU: si hay algo listo para ella lo toma; si no,
/// duerme. `sti; hlt` es atómico: una IPI o un tick que llegue justo antes
/// de dormir despierta a la CPU igualmente.
pub fn idle_loop() -> ! {
    loop {
        let irq = KernelIrq::save_and_disable();
        let work = with(|s| s.policy.has_ready_for(cpu::id() as usize));
        if work {
            KernelIrq::restore(irq);
            yield_now();
        } else {
            // SAFETY: rdtsc solo lee el contador de tiempo.
            let t0 = unsafe { core::arch::x86_64::_rdtsc() };
            // SAFETY: habilitar interrupciones y esperar la siguiente.
            unsafe { core::arch::asm!("sti", "hlt", options(nomem, nostack)) };
            // SAFETY: ídem.
            let t1 = unsafe { core::arch::x86_64::_rdtsc() };
            // El hilo idle nunca migra: `current()` es su propia CPU.
            cpu::current().add_idle_cycles(t1.wrapping_sub(t0));
        }
    }
}

pub fn current_tid() -> Tid {
    Tid(cpu::current_tid())
}

/// Crea un hilo del kernel listo para ejecutar.
pub fn spawn(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
) -> Result<Tid, &'static str> {
    let cr3 = crate::memory::vmm::with(|m| m.root_phys());
    spawn_thread(name, entry, arg, None, cr3, None)
}

/// Crea un hilo del kernel que solo ejecutará en la CPU `cpu`.
pub fn spawn_on(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    cpu: usize,
) -> Result<Tid, &'static str> {
    let cr3 = crate::memory::vmm::with(|m| m.root_phys());
    spawn_thread(name, entry, arg, None, cr3, Some(cpu))
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
    kick_idle();
    tid
}

fn spawn_thread(
    name: &str,
    entry: super::context::ThreadEntry,
    arg: usize,
    process: Option<u32>,
    cr3: u64,
    affinity: Option<usize>,
) -> Result<Tid, &'static str> {
    let tid = with(|s| s.policy.allocate_tid());
    let thread = Thread::new(tid, name, entry, arg, process, cr3)?;
    with(|s| {
        s.threads.insert(tid, Box::new(thread));
        s.policy.add(tid);
        if let Some(c) = affinity {
            s.policy
                .set_affinity(tid, Some(c))
                .map_err(|_| "scheduler: CPU desconocida")?;
        }
        Ok::<(), &'static str>(())
    })?;
    kick_idle();
    debug!("task: spawn {tid:?} '{name}'");
    Ok(tid)
}

/// Proceso del hilo actual.
pub fn current_process() -> Option<u32> {
    let me = current_tid();
    with(|s| s.threads.get(&me).and_then(|t| t.process))
}

/// Estado de un hilo, si existe.
pub fn state(tid: Tid) -> Option<State> {
    with(|s| s.policy.state(tid))
}

/// Cede la CPU al siguiente hilo listo.
pub fn yield_now() {
    switch_with(|p, c| p.yield_current(c));
}

/// Termina el hilo actual. Nunca retorna.
pub fn exit() -> ! {
    switch_with(|p, c| Some(p.exit_current(c)));
    unreachable!("un hilo terminado no reanuda");
}

#[no_mangle]
extern "C" fn thread_exit_trampoline() -> ! {
    exit()
}

/// Primer código de un hilo nuevo (desde `thread_start`, IF = 0): suelta el
/// lock del planificador que tomó la CPU que conmutó hacia él.
#[no_mangle]
extern "C" fn thread_first_run() {
    // SAFETY: protocolo de cambio de contexto (ver cabecera del módulo).
    unsafe { SCHED.force_unlock() };
    reap();
}

/// Aplica `decide` a la política (para la CPU actual) y, si hay cambio,
/// conmuta de contexto con el lock del planificador tomado.
fn switch_with(decide: impl FnOnce(&mut RoundRobin, usize) -> Option<Tid>) {
    let irq = KernelIrq::save_and_disable();
    let c = cpu::current();
    let mut guard = SCHED.lock();
    let switch = {
        let s = guard.as_mut().expect("scheduler no inicializado");
        let prev = s.policy.current(c.index());
        decide(&mut s.policy, c.index()).map(|next| {
            let old = &mut s.threads.get_mut(&prev).expect("hilo actual").saved_rsp as *mut u64;
            let nt = s.threads.get(&next).expect("hilo siguiente");
            (next, old, nt.saved_rsp, nt.kernel_stack_top(), nt.cr3)
        })
    };
    match switch {
        Some((next, old, new, kstack, cr3)) => {
            c.set_current(next.0, QUANTUM_TICKS);
            SWITCHES.fetch_add(1, Ordering::Relaxed);
            // Entradas a ring 0 desde ring 3 del hilo siguiente: su pila de kernel.
            crate::arch::x86_64::gdt::set_kernel_stack(kstack);
            // Espacio de direcciones del hilo siguiente (la mitad del kernel es
            // común, así que el cambio es seguro desde cualquier pila de kernel).
            let current_cr3: u64;
            // SAFETY: lectura de CR3.
            unsafe {
                core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack))
            };
            if cr3 != current_cr3 {
                // SAFETY: `cr3` es un PML4 construido por el VMM o por `process::spawn`.
                unsafe { core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack)) };
            }
            // El lock sigue tomado durante el cambio: lo suelta el hilo que
            // reanuda (aquí abajo, o `thread_first_run` si es su primera vez).
            core::mem::forget(guard);
            // SAFETY: `old` apunta al campo de un `Box<Thread>` que no se mueve ni
            // se libera mientras el hilo exista; `new` fue guardado por un
            // `switch_context` previo o preparado por `initial_stack_frame`.
            unsafe { switch_context(old, new) };
            // SAFETY: quien conmutó hacia nosotros dejó el lock tomado.
            unsafe { SCHED.force_unlock() };
            // Al reanudar aquí, otro hilo puede haber terminado: liberar sus pilas.
            reap();
        }
        None => drop(guard),
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
    use core::sync::atomic::{AtomicBool, AtomicU64};

    static LOG: IrqLock<Vec<u8>> = IrqLock::new(Vec::new());
    static GO: AtomicBool = AtomicBool::new(false);
    extern "C" fn ping(arg: usize) {
        // Nadie anota hasta que los dos hilos existen: si no, un tick entre
        // los dos spawn deja a uno solo en la cola y rompe la alternancia.
        while !GO.load(Ordering::Acquire) {
            yield_now();
        }
        for _ in 0..3 {
            LOG.lock().push(arg as u8);
            yield_now();
        }
    }
    assert_eq!(current_tid(), MAIN_TID, "la autoprueba corre en main");
    // La alternancia estricta solo tiene sentido en una CPU: se fijan los
    // tres hilos a la CPU 0.
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let a = spawn_on("ping-a", ping, b'A' as usize, 0).unwrap();
    let b = spawn_on("ping-b", ping, b'B' as usize, 0).unwrap();
    GO.store(true, Ordering::Release);
    while state(a).is_some() || state(b).is_some() {
        yield_now();
    }
    let log = LOG.lock().clone();
    // Quien anota primero depende de en que punto del turno cayo el ultimo
    // tick; lo que se exige es la alternancia estricta.
    let alternates = log.len() == 6
        && log.windows(2).all(|w| w[0] != w[1])
        && log.iter().filter(|&&c| c == b'A').count() == 3;
    assert!(
        alternates,
        "los dos hilos deben alternar: {:?}",
        core::str::from_utf8(&log)
    );
    set_affinity(MAIN_TID, None).unwrap();

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
    // Los ultimos zombis los recoge el hilo que reanuda tras su salida, que
    // puede estar en otra CPU: ceder un par de veces les da tiempo.
    for _ in 0..8 {
        sleep_ms(10);
    }
    assert_eq!(DONE.load(Ordering::Relaxed), N as u64);
    let free_after = pmm::stats().free_frames;
    // Las pilas se devuelven; solo pueden quedar tablas de páginas nuevas.
    assert!(
        free_before - free_after <= 8,
        "frames no devueltos tras reap: {}",
        free_before - free_after
    );
    let expected = 1 + cpu::online_count();
    assert_eq!(thread_count(), expected, "solo main y un idle por CPU");
    assert_eq!(current_tid(), MAIN_TID);
    info!(
        "threads selftest ok (alternancia {}, {N} workers terminados y recogidos, {} frames en tablas)",
        core::str::from_utf8(&log).unwrap_or("?"),
        free_before - free_after
    );
}

/// Autoprueba preemptiva (`test=sched`): hilos que nunca ceden reparten la
/// CPU, el sueño planificado respeta el tiempo, y bloqueo/despertar funcionan
/// a través de una cola de espera.
pub fn selftest() {
    use super::wait::WaitQueue;
    use core::sync::atomic::{AtomicBool, AtomicU64};

    // 1. Equidad: tres hilos ocupados sin yield durante 300 ms, todos en la
    // CPU 0 para que compitan de verdad por ella.
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTERS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
    extern "C" fn busy(i: usize) {
        while !STOP.load(Ordering::Relaxed) {
            COUNTERS[i].fetch_add(1, Ordering::Relaxed);
            core::hint::spin_loop();
        }
    }
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let switches_before = stats().switches;
    let busy_tids: Vec<Tid> = (0..3)
        .map(|i| spawn_on("busy", busy, i, 0).unwrap())
        .collect();
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
    set_affinity(MAIN_TID, None).unwrap();

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

/// Autoprueba SMP (`test=smp`): todas las CPUs de la MADT en linea, hilos
/// con afinidad que ejecutan a la vez en CPUs distintas, hilos sin afinidad
/// que migran, TLB shootdown observado desde otra CPU y un contador bajo
/// lock disputado por todas las CPUs.
pub fn smp_selftest() {
    use crate::memory::{phys, pmm, vmm};
    use aricode_x86_paging::{GLOBAL, NO_EXECUTE, WRITABLE};
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};

    let cpus = cpu::online_count();
    let listed = crate::acpi::with(|a| a.madt.lapic_ids.len()).clamp(1, cpu::MAX_CPUS);
    assert_eq!(cpus, listed, "no todas las CPUs de la MADT estan en linea");
    assert!(cpus <= cpu::MAX_CPUS);

    // 1. Afinidad y paralelismo: un hilo ocupado por CPU durante 150 ms.
    static STOP: AtomicBool = AtomicBool::new(false);
    static COUNTS: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];
    static SEEN: [AtomicU32; cpu::MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; cpu::MAX_CPUS];
    extern "C" fn pinned(i: usize) {
        let me = cpu::id();
        SEEN[i].store(me, Ordering::Relaxed);
        while !STOP.load(Ordering::Relaxed) {
            COUNTS[i].fetch_add(1, Ordering::Relaxed);
            if cpu::id() != me {
                SEEN[i].store(u32::MAX - 1, Ordering::Relaxed);
            }
            core::hint::spin_loop();
        }
    }
    let t0 = crate::time::ticks();
    let tids: Vec<Tid> = (0..cpus)
        .map(|i| spawn_on("pinned", pinned, i, i).unwrap())
        .collect();
    sleep_ms(150);
    STOP.store(true, Ordering::Relaxed);
    while tids.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let elapsed = crate::time::ticks() - t0;
    let counts: Vec<u64> = COUNTS[..cpus]
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();
    for i in 0..cpus {
        assert_eq!(
            SEEN[i].load(Ordering::Relaxed),
            i as u32,
            "el hilo fijado a la CPU {i} corrio en otra"
        );
        assert!(counts[i] > 0, "el hilo de la CPU {i} no ejecuto");
    }
    let (min, max) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
    assert!(
        min * 4 >= max,
        "las CPUs no trabajaron a la vez: {counts:?}"
    );
    assert!(
        elapsed < 40,
        "{cpus} hilos ocupados en paralelo tardaron {elapsed} ticks"
    );

    // 2. Migración: hilos sin afinidad que ceden en bucle pasan por varias CPUs.
    static MASKS: [AtomicU32; 2 * cpu::MAX_CPUS] = [const { AtomicU32::new(0) }; 2 * cpu::MAX_CPUS];
    extern "C" fn roamer(i: usize) {
        for _ in 0..500 {
            MASKS[i].fetch_or(1 << cpu::id(), Ordering::Relaxed);
            yield_now();
        }
    }
    let roamers: Vec<Tid> = (0..2 * cpus)
        .map(|i| spawn("roamer", roamer, i).unwrap())
        .collect();
    while roamers.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let union = MASKS[..2 * cpus]
        .iter()
        .fold(0u32, |m, x| m | x.load(Ordering::Relaxed));
    let migrated = MASKS[..2 * cpus]
        .iter()
        .filter(|m| m.load(Ordering::Relaxed).count_ones() >= 2)
        .count();
    assert_eq!(
        union,
        (1u32 << cpus) - 1,
        "los hilos libres deben pasar por todas las CPUs"
    );
    if cpus > 1 {
        assert!(migrated > 0, "ningun hilo cambio de CPU: {MASKS:?}");
    }

    // 3. TLB shootdown: otra CPU tiene la traducción en su TLB; se cambia el
    // frame y tras el shootdown ve el contenido nuevo.
    const VIRT: u64 = 0xFFFF_9000_0010_0000;
    static READY: AtomicBool = AtomicBool::new(false);
    static GO: AtomicBool = AtomicBool::new(false);
    static SEEN_VALUE: AtomicU64 = AtomicU64::new(0);
    extern "C" fn reader(_: usize) {
        // SAFETY: la página está mapeada por la autoprueba mientras dura.
        let first = unsafe { (VIRT as *const u64).read_volatile() };
        assert_eq!(first, 0x11, "primera lectura");
        READY.store(true, Ordering::Release);
        while !GO.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
        // SAFETY: ídem.
        SEEN_VALUE.store(
            unsafe { (VIRT as *const u64).read_volatile() },
            Ordering::Relaxed,
        );
    }
    let served_before = crate::arch::x86_64::smp::flushes_served();
    let (f1, f2) = pmm::with(|p| (p.allocate().unwrap(), p.allocate().unwrap()));
    // SAFETY: frames recién asignados, accesibles por el mapa físico.
    unsafe {
        (phys::phys_to_virt(f1.addr()) as *mut u64).write_volatile(0x11);
        (phys::phys_to_virt(f2.addr()) as *mut u64).write_volatile(0x22);
    }
    vmm::with(|m| m.map_4k(VIRT, f1.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).unwrap();
    let reader_cpu = if cpus > 1 { 1 } else { 0 };
    set_affinity(MAIN_TID, Some(0)).unwrap();
    let r = spawn_on("reader", reader, 0, reader_cpu).unwrap();
    while !READY.load(Ordering::Acquire) {
        yield_now();
    }
    vmm::with(|m| {
        m.unmap_4k(VIRT).unwrap();
        m.map_4k(VIRT, f2.addr(), WRITABLE | NO_EXECUTE | GLOBAL)
            .unwrap();
    });
    vmm::flush_page(VIRT);
    GO.store(true, Ordering::Release);
    while state(r).is_some() {
        yield_now();
    }
    assert_eq!(
        SEEN_VALUE.load(Ordering::Relaxed),
        0x22,
        "la CPU {reader_cpu} siguio viendo la traduccion vieja"
    );
    let served = crate::arch::x86_64::smp::flushes_served() - served_before;
    assert_eq!(served, cpus - 1, "IPI de invalidacion atendidas");
    vmm::with(|m| m.unmap_4k(VIRT)).unwrap();
    vmm::flush_page(VIRT);
    pmm::with(|p| {
        p.free(f1).unwrap();
        p.free(f2).unwrap();
    });
    set_affinity(MAIN_TID, None).unwrap();

    // 4. Locks disputados: un contador bajo IrqLock incrementado desde todas
    // las CPUs a la vez.
    static COUNTER: IrqLock<u64> = IrqLock::new(0);
    const PER_THREAD: u64 = 20_000;
    extern "C" fn incrementer(_: usize) {
        for _ in 0..PER_THREAD {
            *COUNTER.lock() += 1;
        }
    }
    *COUNTER.lock() = 0;
    let incs: Vec<Tid> = (0..cpus)
        .map(|i| spawn_on("inc", incrementer, i, i).unwrap())
        .collect();
    while incs.iter().any(|t| state(*t).is_some()) {
        yield_now();
    }
    let total = *COUNTER.lock();
    assert_eq!(
        total,
        PER_THREAD * cpus as u64,
        "incrementos perdidos bajo el lock"
    );

    info!(
        "smp selftest ok ({cpus} CPUs: ocupados {counts:?} en {elapsed} ticks, {migrated} de {} hilos migraron, shootdown visto por la CPU {reader_cpu} ({served} IPI), contador {total})",
        2 * cpus
    );
}
