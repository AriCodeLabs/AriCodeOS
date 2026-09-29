//! AriInit (Fase 15): primer proceso de usuario. Recibe del kernel el handle
//! `SPAWNER` (0x10004, `MANAGE`), la raíz PCI (0x10005, `READ|MANAGE`), una
//! capacidad de hardware (0x10006, `MANAGE`) para repartir DMA, el objeto
//! `SYSTEM` (0x10007) y la entrada de consola (0x10008, `READ`). Lanza los
//! servicios de arranque y los supervisa; hasta que exista un device manager
//! propio también reclama dispositivos y lanza sus drivers (Fases 18 y 19).
//! Si `arg` lleva `init_arg::SHELL`, al final lanza el shell (Fase 21).
//!
//! En 0.0.8 su lista de servicios es de prueba: demuestra lanzar, esperar,
//! reaccionar a un proceso terminado por fallo y montar el servicio de bloques.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_manifest::{Caps, Manifest};
use aricode_proto::launch::{
    Reply as LaunchReply, Request as LaunchRequest, Status as LaunchStatus,
};
use aricode_proto::status;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};
use aricode_supervisor::{Action, Exit, Policy, Service, State};

const SPAWNER: Handle = Handle(0x0001_0004);
const PCI_ROOT: Handle = Handle(0x0001_0005);
const HARDWARE: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
const CONSOLE_IN: Handle = Handle(0x0001_0008);
const FRAMEBUFFER: Handle = Handle(0x0001_0009);

/// Bits de la máscara de servicios que recibe el shell.
const SERVICE_BLOCK: u64 = 1;
const SERVICE_FS: u64 = 2;
const SERVICE_NET: u64 = 4;
const SERVICE_INPUT: u64 = 8;
const SERVICE_GUI: u64 = 16;

fn say(msg: &[u8]) {
    sys::print(b"AriInit: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Lanza un programa del initrd y espera su salida.
fn run(name: &str, arg: u64) -> Result<u64, Error> {
    let child = sys::process_spawn(SPAWNER, name, arg)?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Busca la primera función PCI con `vendor:device` y devuelve su índice.
fn find_pci(vendor: u16, device: u16) -> Option<u64> {
    find_pci_from(vendor, device, 0)
}

/// Primera función PCI de la clase `class.subclass` a partir de `start`.
fn find_pci_class_from(class: u8, subclass: u8, start: u64) -> Option<u64> {
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (start..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::class(v) == class && sys::abi::pci_info::subclass(v) == subclass
        })
    })
}

/// Comprueba que un servicio recién lanzado responde a `request` con algo
/// que `accept` da por bueno, antes de publicar su endpoint (auditoría
/// 4836023, hallazgo 1, generalizado por el supervisor): un proceso que
/// termina sin servir no debe dejar clientes esperando. Devuelve `false` si
/// el proceso acabó o no contestó en unos segundos.
fn probe_ready(service: Handle, driver: Handle, request: &[u8], accept: fn(&[u8]) -> bool) -> bool {
    let Ok(reply) = sys::endpoint_create() else {
        return false;
    };
    let mut ok = false;
    for _ in 0..12 {
        match sys::process_wait(driver, 0) {
            Err(Error::Timeout) => {}
            _ => break, // termino (con codigo o por fallo): no servira
        }
        let Ok(reply_w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) else {
            break;
        };
        if sys::ipc_send(service, request, &[reply_w], 250).is_err() {
            continue;
        }
        let mut msg = [0u8; 256];
        let mut none: [Handle; 0] = [];
        if let Ok((n, _)) = sys::ipc_receive(reply, &mut msg, &mut none, 250) {
            ok = accept(&msg[..n]);
            break;
        }
    }
    let _ = sys::handle_close(reply);
    ok
}

fn input_service_ready(service: Handle, driver: Handle) -> bool {
    use aricode_proto::input::{Op, Reply, Request};
    let req = Request {
        op: Op::Info,
        timeout_ms: 0,
    };
    probe_ready(service, driver, &req.encode(), |b| {
        Reply::decode(b).is_some()
    })
}

fn ping_service_ready(service: Handle, driver: Handle) -> bool {
    probe_ready(service, driver, b"ping", |b| b == b"pong")
}

/// Una petición corta al servicio de prueba ("ping" o "die") esperando "pong".
fn ping_once(service: Handle, request: &[u8]) -> bool {
    let Ok(reply) = sys::endpoint_create() else {
        return false;
    };
    let mut ok = false;
    if let Ok(reply_w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) {
        if sys::ipc_send(service, request, &[reply_w], 500).is_ok() {
            let mut msg = [0u8; 16];
            let mut none: [Handle; 0] = [];
            if let Ok((n, _)) = sys::ipc_receive(reply, &mut msg, &mut none, 500) {
                ok = &msg[..n] == b"pong";
            }
        }
    }
    let _ = sys::handle_close(reply);
    ok
}

/// `bb:dd.f` de un valor de `pci_info`.
fn print_bdf(info: u64) {
    print_hex8(sys::abi::pci_info::bus(info));
    sys::print(b":");
    print_hex8(sys::abi::pci_info::dev(info));
    sys::print(b".");
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(
        sys::abi::pci_info::func(info) as u64,
        &mut b,
    ));
}

fn print_hex8(v: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    sys::print(&[HEX[(v >> 4) as usize], HEX[(v & 0xF) as usize]]);
}

/// Como `find_pci`, buscando a partir del índice `start`.
fn find_pci_from(vendor: u16, device: u16, start: u64) -> Option<u64> {
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (start..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::vendor(v) == vendor && sys::abi::pci_info::device(v) == device
        })
    })
}

/// Reclama un dispositivo y lanza su driver como servicio residente con las
/// capabilities transferidas. Devuelve el handle del proceso.
///
/// Contrato con el driver: recibe en orden el endpoint de servicio, el
/// espacio de configuración, la capacidad de hardware, la IRQ (si existe),
/// solo los BARs presentes y después `extras` (propios de cada driver);
/// `arg` lleva una máscara: bit 15 = hay IRQ, bits 0..5 = BAR i presente.
fn start_driver(
    name: &str,
    index: u64,
    service: Handle,
    extras: &[Handle],
) -> Result<Handle, Error> {
    let handles = sys::pci_claim(PCI_ROOT, index)?;
    let mut to_send = [Handle(0); 12];
    to_send[0] = sys::handle_duplicate(service, rights::READ | rights::WRITE | rights::TRANSFER)?;
    let mut n = 1;
    let mut mask = 0u64;
    for (i, h) in handles.iter().enumerate() {
        if h.0 == 0 {
            continue;
        }
        match i {
            2 => mask |= 1 << 15,
            3..=8 => mask |= 1 << (i - 3),
            _ => {}
        }
        to_send[n] = *h;
        n += 1;
    }
    for h in extras {
        to_send[n] = *h;
        n += 1;
    }
    sys::process_spawn_with(SPAWNER, name, mask, &to_send[..n])
}

/// Primer y último LBA de la partición GPT `name`, leyendo la tabla por el
/// servicio de bloques.
fn find_partition(blk: &mut BlockClient, name: &str) -> Option<(u64, u64)> {
    blk.read(1, 1).ok()?;
    let header = aricode_gpt::Header::parse(&blk.buffer()[..512]).ok()?;
    let sectors = header.entries_bytes().div_ceil(512) as u64;
    let mut table = alloc::vec![0u8; sectors as usize * 512];
    let mut lba = 0;
    while lba < sectors {
        let n = (sectors - lba).min(16) as u16;
        blk.read(header.entries_lba + lba, n).ok()?;
        let o = lba as usize * 512;
        table[o..o + n as usize * 512].copy_from_slice(&blk.buffer()[..n as usize * 512]);
        lba += n as u64;
    }
    let mut parts = [None; 16];
    let count = aricode_gpt::partitions(&header, &table, &mut parts).ok()?;
    parts[..count]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| (p.first_lba, p.last_lba))
}

/// Lee y anuncia el estado de arranque (para los tests de no confirmación).
fn report_boot_state(block_service: Handle) {
    use aricode_ab::BootState;
    let Ok(mut blk) = BlockClient::new(block_service, HARDWARE, 16) else {
        return;
    };
    let Some((first, _)) = find_partition(&mut blk, aricode_ab::PART_STATE) else {
        return;
    };
    if blk.read(first, 1).is_err() {
        return;
    }
    let Some(state) = BootState::decode(&blk.buffer()[..512]) else {
        return;
    };
    let mut buf = [0u8; 20];
    sys::print(b"AriInit: estado de arranque: activo ");
    sys::print(state.active.name().as_bytes());
    match state.pending {
        Some((slot, tries)) => {
            sys::print(b", pendiente ");
            sys::print(slot.name().as_bytes());
            sys::print(b" (");
            sys::print(aricode_rt::fmt_u64(tries as u64, &mut buf));
            sys::print(b" intentos)\n");
        }
        None => sys::print(b", sin pendiente\n"),
    }
}

/// Arranque A/B: si el kernel arrancó desde un slot pendiente, marcarlo
/// activo en `ARIBOOT` ahora que todos los servicios de arranque funcionan.
fn confirm_boot_slot(arg: u64, block_service: Handle) {
    use aricode_ab::{confirm, BootState, Slot};
    let slot = if arg & sys::abi::init_arg::BOOT_SLOT_A != 0 {
        Slot::A
    } else if arg & sys::abi::init_arg::BOOT_SLOT_B != 0 {
        Slot::B
    } else {
        say(b"arrancado desde la ESP (sin slots)");
        return;
    };
    sys::print(b"AriInit: arrancado desde el slot ");
    sys::print(slot.name().as_bytes());
    if arg & sys::abi::init_arg::BOOT_PENDING == 0 {
        sys::print(b" (activo)\n");
        return;
    }
    sys::print(b" (pendiente de confirmacion)\n");
    let Ok(mut blk) = BlockClient::new(block_service, HARDWARE, 16) else {
        say(b"no se pudo abrir el servicio de bloques para confirmar");
        return;
    };
    let Some((first, _)) = find_partition(&mut blk, aricode_ab::PART_STATE) else {
        say(b"sin particion ARIBOOT, no se confirma");
        return;
    };
    if blk.read(first, 1).is_err() {
        return say(b"no se pudo leer el estado de arranque");
    }
    let Some(state) = BootState::decode(&blk.buffer()[..512]) else {
        return say(b"estado de arranque invalido, no se confirma");
    };
    let confirmed = confirm(state, slot);
    blk.buffer()[..512].copy_from_slice(&confirmed.encode());
    match blk.write(first, 1) {
        Ok(()) => {
            sys::print(b"AriInit: arranque confirmado en el slot ");
            sys::print(slot.name().as_bytes());
            sys::print(b"\n");
        }
        Err(_) => say(b"no se pudo escribir el estado de arranque"),
    }
}

/// Lanza fsd con su endpoint (READ|WRITE), el del servicio de bloques (WRITE)
/// y una capacidad de hardware; `arg` = LBA de la partición.
fn start_fsd(
    fs_service: Handle,
    block_service: Handle,
    partition_lba: u64,
) -> Result<Handle, Error> {
    let own = sys::handle_duplicate(fs_service, rights::READ | rights::WRITE | rights::TRANSFER)?;
    let blk = sys::handle_duplicate(block_service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    sys::process_spawn_with(SPAWNER, "fsd", partition_lba, &[own, blk, hw])
}

/// Cómo se (re)lanza un servicio residente.
#[derive(Clone, Copy)]
enum Kind {
    /// Driver PCI con el contrato de `start_driver`; `with_system` añade
    /// `SYSTEM` (lectura) como extra.
    Driver { pci_index: u64, with_system: bool },
    /// fsd sobre el servicio de bloques.
    Fs {
        block_service: Handle,
        partition_lba: u64,
    },
    /// Programa del initrd con su endpoint de servicio (servicios de prueba).
    Program { arg: u64 },
    /// Puente de consola (ADR 0013): la entrada de consola con `MANAGE` y
    /// los endpoints de los servicios de entrada. No atiende peticiones, así
    /// que su residente no tiene endpoint propio.
    ConsoleBridge {
        inputs: [Handle; MAX_INPUTS],
        count: usize,
    },
}

/// Servicios de entrada que init reparte (compositor, broker, puente).
const MAX_INPUTS: usize = 4;

/// Lanza el puente de consola con la entrada de consola (`MANAGE`) y un
/// duplicado (`WRITE`) de cada endpoint de entrada; `arg` = cuántos. Si un
/// duplicado falla, cierra los ya hechos.
fn start_console_bridge(inputs: &[Handle]) -> Result<Handle, Error> {
    let mut handles = [Handle(0); MAX_INPUTS + 1];
    let mut made = 0;
    let result = (|| {
        handles[0] = sys::handle_duplicate(CONSOLE_IN, rights::MANAGE | rights::TRANSFER)?;
        made = 1;
        for ep in inputs {
            handles[made] = sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER)?;
            made += 1;
        }
        sys::process_spawn_with(
            SPAWNER,
            "console_bridge",
            inputs.len() as u64,
            &handles[..made],
        )
    })();
    if result.is_err() {
        for h in &handles[..made] {
            let _ = sys::handle_close(*h);
        }
    }
    result
}

/// Cómo se comprueba que el servicio ya sirve.
#[derive(Clone, Copy)]
enum Probe {
    /// Lo comprueba un cliente de prueba después (bloques, ficheros, red).
    None,
    /// `InputServiceProtocol::Info`.
    Input,
    /// "ping" -> "pong".
    Ping,
}

/// Un servicio residente bajo supervisión. El endpoint es de init y no
/// cambia entre generaciones: un cliente sigue hablando con "el servicio"
/// aunque el proceso que lo atiende se haya reiniciado.
struct Resident {
    name: &'static str,
    kind: Kind,
    probe: Probe,
    endpoint: Handle,
    process: Option<Handle>,
    sup: Service,
}

const MAX_RESIDENTS: usize = 8;

/// Supervisor de servicios residentes (etapa 1 del plan posterior al
/// roadmap): estados, generaciones, reinicio con espera creciente y
/// presupuesto, y estado consultable (`status` v1) para el shell.
struct Supervisor {
    slots: [Option<Resident>; MAX_RESIDENTS],
    count: usize,
}

fn now_ms() -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(SYSTEM, sys::abi::sysinfo::UPTIME, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

impl Supervisor {
    fn new() -> Self {
        Self {
            slots: [const { None }; MAX_RESIDENTS],
            count: 0,
        }
    }

    fn register(
        &mut self,
        name: &'static str,
        kind: Kind,
        probe: Probe,
        endpoint: Handle,
        optional: bool,
        policy: Policy,
    ) -> Option<usize> {
        if self.count == MAX_RESIDENTS {
            return None;
        }
        let i = self.count;
        self.slots[i] = Some(Resident {
            name,
            kind,
            probe,
            endpoint,
            process: None,
            sup: Service::new(policy, optional),
        });
        self.count += 1;
        Some(i)
    }

    fn generation(&self, i: usize) -> u32 {
        self.slots[i].as_ref().map_or(0, |r| r.sup.generation())
    }

    fn state(&self, i: usize) -> State {
        self.slots[i]
            .as_ref()
            .map_or(State::Stopped, |r| r.sup.state())
    }

    fn endpoint(&self, i: usize) -> Handle {
        self.slots[i].as_ref().map_or(Handle(0), |r| r.endpoint)
    }

    fn has_process(&self, i: usize) -> bool {
        self.slots[i].as_ref().is_some_and(|r| r.process.is_some())
    }

    /// Retira el servicio `i`: no se relanzará aunque muera.
    fn stop(&mut self, i: usize) {
        if let Some(r) = self.slots[i].as_mut() {
            r.sup.stop();
        }
    }

    /// Lanza (o relanza) el servicio `i` y espera a que sirva. Devuelve si
    /// quedó listo; si no, el supervisor ya ha decidido qué hacer.
    fn launch(&mut self, i: usize) -> bool {
        let Some(r) = self.slots[i].as_mut() else {
            return false;
        };
        let spawned = match r.kind {
            Kind::Driver {
                pci_index,
                with_system,
            } => {
                let system = if with_system {
                    match sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) {
                        Ok(h) => Some(h),
                        Err(e) => Err(e).ok(),
                    }
                } else {
                    None
                };
                match system {
                    Some(h) => start_driver(r.name, pci_index, r.endpoint, &[h]),
                    None if with_system => Err(Error::Limit),
                    None => start_driver(r.name, pci_index, r.endpoint, &[]),
                }
            }
            Kind::Fs {
                block_service,
                partition_lba,
            } => start_fsd(r.endpoint, block_service, partition_lba),
            Kind::Program { arg } => {
                sys::handle_duplicate(r.endpoint, rights::READ | rights::WRITE | rights::TRANSFER)
                    .and_then(|ep| sys::process_spawn_with(SPAWNER, r.name, arg, &[ep]))
            }
            Kind::ConsoleBridge { inputs, count } => start_console_bridge(&inputs[..count]),
        };
        let now = now_ms();
        let process = match spawned {
            Ok(h) => h,
            Err(e) => {
                aricode_rt::print_error(b"AriInit: lanzar servicio", e);
                let _ = r.sup.starting(now);
                self.announce_death(i, now, Exit::Code(u64::MAX));
                return false;
            }
        };
        r.process = Some(process);
        let generation = r.sup.starting(now);
        let ready = match r.probe {
            Probe::None => true,
            Probe::Input => input_service_ready(r.endpoint, process),
            Probe::Ping => ping_service_ready(r.endpoint, process),
        };
        if ready {
            r.sup.ready(now_ms());
            if generation > 1 {
                sys::print(b"AriInit: servicio ");
                sys::print(r.name.as_bytes());
                sys::print(b" reiniciado (generacion ");
                let mut b = [0u8; 20];
                sys::print(aricode_rt::fmt_u64(generation as u64, &mut b));
                sys::print(b")\n");
            }
            true
        } else {
            self.announce_death(i, now_ms(), Exit::StartTimeout);
            false
        }
    }

    /// Registra la muerte de `i`, cierra su proceso y anuncia la decisión.
    fn announce_death(&mut self, i: usize, now: u64, exit: Exit) {
        let Some(r) = self.slots[i].as_mut() else {
            return;
        };
        if let Some(h) = r.process.take() {
            let _ = sys::handle_close(h);
        }
        let action = r.sup.died(now, exit);
        sys::print(b"AriInit: servicio ");
        sys::print(r.name.as_bytes());
        sys::print(match exit {
            Exit::Code(_) => b" termino",
            Exit::Killed => b" murio por fallo",
            Exit::StartTimeout => b" no llego a servir",
        });
        let mut b = [0u8; 20];
        match action {
            Action::Restart { at } => {
                sys::print(b"; reinicio en ");
                sys::print(aricode_rt::fmt_u64(at.saturating_sub(now), &mut b));
                sys::print(b" ms\n");
            }
            _ => {
                sys::print(b"; presupuesto de reinicios agotado: ");
                sys::print(if r.sup.is_optional() {
                    b"sistema degradado\n"
                } else {
                    b"servicio obligatorio fallido\n"
                });
            }
        }
    }

    /// Vigila los procesos: muertes, plazos de arranque y reinicios que ya tocan.
    fn poll(&mut self) {
        let now = now_ms();
        for i in 0..self.count {
            let (process, state) = match self.slots[i].as_ref() {
                Some(r) => (r.process, r.sup.state()),
                None => continue,
            };
            if let Some(h) = process {
                match sys::process_wait(h, 0) {
                    Err(Error::Timeout) => {
                        if let Some(r) = self.slots[i].as_mut() {
                            if r.sup.tick(now) == Action::Timeout {
                                self.announce_death(i, now, Exit::StartTimeout);
                            }
                        }
                    }
                    Ok(code) => self.announce_death(i, now, Exit::Code(code)),
                    Err(Error::Killed) => self.announce_death(i, now, Exit::Killed),
                    Err(_) => self.announce_death(i, now, Exit::Code(u64::MAX)),
                }
            } else if state == State::Stopped {
                let start = self.slots[i]
                    .as_mut()
                    .is_some_and(|r| r.sup.tick(now) == Action::Start);
                if start {
                    self.launch(i);
                }
            }
        }
    }

    /// Estado del servicio `index` para `status` v1.
    fn status(&self, index: u8) -> status::Reply {
        let Some(r) = self.slots.get(index as usize).and_then(|s| s.as_ref()) else {
            return status::Reply::not_found();
        };
        let st = r.sup.status();
        let (last_exit, last_code) = match st.last_exit {
            None => (status::LastExit::None, 0),
            Some(Exit::Code(c)) => (status::LastExit::Code, c),
            Some(Exit::Killed) => (status::LastExit::Killed, 0),
            Some(Exit::StartTimeout) => (status::LastExit::StartTimeout, 0),
        };
        status::Reply {
            found: true,
            state: match st.state {
                State::Stopped => status::State::Stopped,
                State::Starting => status::State::Starting,
                State::Ready => status::State::Ready,
                State::Degraded => status::State::Degraded,
                State::Failed => status::State::Failed,
            },
            optional: r.sup.is_optional(),
            generation: st.generation,
            restarts: st.restarts,
            last_exit,
            last_code,
            ready_for_ms: st.ready_at.map_or(0, |t| now_ms().saturating_sub(t).max(1)),
            ..status::Reply::not_found()
        }
        .with_name(r.name)
    }
}

/// Reinicios provocados (`restart_storm`): un servicio de prueba que muere
/// tras cada petición se reinicia 100 veces bajo el supervisor mientras fsd
/// sigue atendiendo; al final la memoria libre debe ser la de antes.
fn restart_storm(sup: &mut Supervisor, fs: &mut FsClient) -> Result<(), u64> {
    let ep = sys::endpoint_create().map_err(|_| 40u64)?;
    let policy = Policy {
        max_restarts: 200,
        backoff_initial_ms: 1,
        backoff_max_ms: 8,
        start_timeout_ms: 2000,
        stable_ms: 60_000,
    };
    let i = sup
        .register(
            "crashy_service",
            Kind::Program { arg: 0 },
            Probe::Ping,
            ep,
            true,
            policy,
        )
        .ok_or(40u64)?;
    let mut mem = [0u8; 16];
    let free = |mem: &mut [u8; 16]| -> u64 {
        match sys::system_info(SYSTEM, sys::abi::sysinfo::MEMORY, mem) {
            Ok(16) => u64::from_le_bytes(mem[8..].try_into().unwrap()),
            _ => 0,
        }
    };
    // Sin ningun proceso de prueba vivo: asi la medida final es comparable.
    let free_before = free(&mut mem);
    let t0 = now_ms();
    if !sup.launch(i) {
        return Err(41);
    }
    let mut fs_ok = 0u32;
    let mut fs_checks = 0u32;
    let mut pings = 0u32;
    loop {
        sup.poll();
        let generation = sup.generation(i);
        if generation > 100 {
            break;
        }
        if now_ms().saturating_sub(t0) > 60_000 {
            return Err(42);
        }
        // "die": contesta y muere; el supervisor lo relanza. Antes de pedir
        // otra muerte hay que haber visto esta: en SMP el proceso puede
        // seguir vivo un instante despues de contestar, y un segundo "die"
        // encolado mataria a la siguiente generacion antes de su sonda.
        if sup.state(i) == State::Ready && sup.has_process(i) {
            if ping_once(sup.endpoint(i), b"die") {
                pings += 1;
                if pings.is_multiple_of(10) {
                    fs_checks += 1;
                    let mut name = [0u8; 32];
                    if fs.list(0, &mut name).is_ok() {
                        fs_ok += 1;
                    }
                }
            }
            let seen = now_ms() + 2000;
            while sup.has_process(i) && now_ms() < seen {
                sup.poll();
            }
        }
    }
    // Ultima generacion: un ping mas la mata; se recoge y se retira el
    // servicio para que el supervisor no lo relance antes de medir.
    let _ = ping_once(sup.endpoint(i), b"die");
    let deadline = now_ms() + 2000;
    while sup.has_process(i) && now_ms() < deadline {
        sup.poll();
    }
    sup.stop(i);
    let elapsed = now_ms().saturating_sub(t0);
    let free_after = free(&mut mem);
    let delta = free_before as i64 - free_after as i64;
    let mut b = [0u8; 20];
    let restarts = sup.generation(i).saturating_sub(1);
    sys::print(b"AriInit: supervisor: ");
    sys::print(aricode_rt::fmt_u64(restarts as u64, &mut b));
    sys::print(b" reinicios de crashy_service en ");
    sys::print(aricode_rt::fmt_u64(elapsed, &mut b));
    sys::print(b" ms, fsd respondio ");
    sys::print(aricode_rt::fmt_u64(fs_ok as u64, &mut b));
    sys::print(b"/");
    sys::print(aricode_rt::fmt_u64(fs_checks as u64, &mut b));
    sys::print(b", memoria: ");
    if delta < 0 {
        sys::print(b"-");
    }
    sys::print(aricode_rt::fmt_u64(delta.unsigned_abs(), &mut b));
    sys::print(b" frames de diferencia\n");
    if restarts != 100 || fs_checks == 0 || fs_ok != fs_checks {
        return Err(43);
    }
    if delta > 32 {
        say(b"supervisor: se han perdido frames en los reinicios");
        return Err(44);
    }
    Ok(())
}

/// Lanza un cliente de un servicio con su endpoint (solo WRITE) y
/// una capacidad de hardware para su buffer DMA, y espera su salida.
/// Desplazamiento del area en bruto dentro de ARI0 (el disco de pruebas la
/// tiene en el LBA 4096 con ARI0 en el 34; la imagen USB, al mismo
/// desplazamiento).
const RAW_AREA_OFFSET: u64 = 4096 - 34;

fn run_block_client(name: &str, service: Handle, arg: u64) -> Result<u64, Error> {
    let ep = sys::handle_duplicate(service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    let child = sys::process_spawn_with(SPAWNER, name, arg, &[ep, hw])?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Lanza el shell con el endpoint de fsd (WRITE), hardware para su buffer,
/// el sistema (READ|MANAGE), la entrada de consola, la raíz PCI solo
/// lectura, el endpoint de netd (WRITE) y un SPAWNER para `run`; `arg` =
/// servicios activos. Espera a que termine.
fn run_shell(
    fs_service: Handle,
    net_service: Handle,
    services: u64,
    broker: &mut Broker,
    sup: &mut Supervisor,
) -> Result<u64, Error> {
    let fsd = sys::handle_duplicate(fs_service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    // Con DUPLICATE el shell puede a su vez pasar un SYSTEM de solo lectura a
    // los programas que lance con `run`.
    let system = sys::handle_duplicate(
        SYSTEM,
        rights::READ | rights::MANAGE | rights::DUPLICATE | rights::TRANSFER,
    )?;
    let console_in = sys::handle_duplicate(CONSOLE_IN, rights::READ | rights::TRANSFER)?;
    let pci = sys::handle_duplicate(PCI_ROOT, rights::READ | rights::TRANSFER)?;
    let netd = sys::handle_duplicate(net_service, rights::WRITE | rights::TRANSFER)?;
    // El shell no recibe el SPAWNER: pide lanzamientos al broker (init).
    let launch = sys::handle_duplicate(broker.endpoint, rights::WRITE | rights::TRANSFER)?;
    let child = sys::process_spawn_with(
        SPAWNER,
        "shell",
        services,
        &[fsd, hw, system, console_in, pci, netd, launch],
    )?;
    // Atender peticiones de lanzamiento y de estado, y vigilar los
    // servicios residentes, mientras el shell viva.
    loop {
        match sys::process_wait(child, 0) {
            Err(Error::Timeout) => {}
            result => {
                let _ = sys::handle_close(child);
                return result;
            }
        }
        sup.poll();
        broker.serve_one(20, sup);
        broker.poll_launches();
    }
}

/// Broker de capabilities (Fase 28): lanza programas según su manifiesto.
/// Una aplicación recibe, en este orden, solo lo que pide y se le permite:
/// `SYSTEM` (lectura), un directorio privado en fsd, el endpoint de red, el
/// del compositor, el de bloques (solo aplicaciones de confianza), los de
/// entrada, y por último `HARDWARE` si necesita buffers, acotado por la
/// memoria del manifiesto.
struct Broker {
    endpoint: Handle,
    fs: Option<FsClient>,
    block: Option<Handle>,
    net: Handle,
    compositor: Option<Handle>,
    inputs: [Handle; 4],
    input_count: usize,
    /// Programas lanzados que aún no han terminado (ADR 0015): proceso y, si
    /// alguien espera su salida, el endpoint al que responder.
    running: [Option<Running>; MAX_RUNNING],
}

/// Programas en marcha a la vez que admite el broker; el siguiente `run` o
/// `spawn` recibe `Limit`.
const MAX_RUNNING: usize = 4;

#[derive(Clone, Copy)]
struct Running {
    process: Handle,
    reply_to: Option<Handle>,
}

/// Resultado inmediato de `launch`: respuesta ya decidida, o un proceso
/// creado cuya salida hay que esperar en una ranura.
enum Launched {
    Reply(LaunchReply),
    Process(Handle),
}

impl Broker {
    fn new(
        fs_service: Option<Handle>,
        block: Option<Handle>,
        net: Handle,
        compositor: Option<Handle>,
        inputs: &[Handle],
    ) -> Result<Self, Error> {
        let fs = match fs_service {
            Some(ep) => Some(FsClient::new(ep, HARDWARE)?),
            None => None,
        };
        let mut ins = [Handle(0); 4];
        let n = inputs.len().min(4);
        ins[..n].copy_from_slice(&inputs[..n]);
        Ok(Self {
            endpoint: sys::endpoint_create()?,
            fs,
            block,
            net,
            compositor,
            inputs: ins,
            input_count: n,
            running: [None; MAX_RUNNING],
        })
    }

    /// Responde a los programas que han terminado y libera sus ranuras.
    fn poll_launches(&mut self) {
        for slot in self.running.iter_mut() {
            let Some(r) = *slot else { continue };
            let reply = match sys::process_wait(r.process, 0) {
                Err(Error::Timeout) => continue,
                Ok(code) => LaunchReply::exited(code),
                Err(Error::Killed) => LaunchReply::status(LaunchStatus::Killed),
                Err(_) => LaunchReply::status(LaunchStatus::Failed),
            };
            let _ = sys::handle_close(r.process);
            if let Some(reply_to) = r.reply_to {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            *slot = None;
        }
    }

    /// Atiende como mucho una petición (espera hasta `timeout_ms`).
    fn serve_one(&mut self, timeout_ms: u64, sup: &Supervisor) {
        let mut msg = [0u8; LaunchRequest::LEN];
        let mut handles = [Handle(0); 1];
        let Ok((n, nh)) = sys::ipc_receive(self.endpoint, &mut msg, &mut handles, timeout_ms)
        else {
            return;
        };
        if nh == 0 {
            return;
        }
        let reply_to = handles[0];
        // El mismo endpoint atiende dos protocolos: lanzamientos (6) y estado
        // de servicios (7); el primer byte los distingue.
        if let Some(req) = status::Request::decode(&msg[..n]) {
            let reply = sup.status(req.index);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        }
        let Some(req) = LaunchRequest::decode(&msg[..n]) else {
            let reply = LaunchReply::status(LaunchStatus::Invalid);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        };
        // La ranura se reserva antes de lanzar: sin ranura no hay proceso.
        let Some(slot) = self.running.iter().position(Option::is_none) else {
            sys::print(b"AriInit: ");
            sys::print(req.name_str().as_bytes());
            sys::print(b": limite de programas en marcha\n");
            let reply = LaunchReply::status(LaunchStatus::Limit);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        };
        match self.launch(req.name_str(), req.arg) {
            Launched::Reply(reply) => {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            Launched::Process(process) if req.is_detached() => {
                let reply = LaunchReply::status(LaunchStatus::Started);
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
                self.running[slot] = Some(Running {
                    process,
                    reply_to: None,
                });
            }
            Launched::Process(process) => {
                self.running[slot] = Some(Running {
                    process,
                    reply_to: Some(reply_to),
                });
            }
        }
    }

    /// Lee el manifiesto (`NOMBRE.manifest` en el initrd), aplica la política y
    /// lanza el programa; su salida la recoge `poll_launches`.
    fn launch(&self, name: &str, arg: u64) -> Launched {
        let mut path = [0u8; 48];
        let Some(path) = join(name, ".manifest", &mut path) else {
            return Launched::Reply(LaunchReply::status(LaunchStatus::Invalid));
        };
        let mut text = [0u8; 1024];
        let manifest = match sys::initrd_read(SPAWNER, path, &mut text) {
            Ok(n) => match core::str::from_utf8(&text[..n])
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
            {
                Some(m) => Some(m),
                None => return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest)),
            },
            Err(Error::NotFound) => None,
            Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest)),
        };
        let (caps, memory_kib) = match manifest {
            Some(m) if m.name_str() != name => {
                return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest))
            }
            Some(m) => (m.caps, m.memory_kib),
            // Sin manifiesto: solo consola y reloj, memoria minima.
            None => (Caps::SYSTEM, 64),
        };
        let read_files = manifest.as_ref().map(Manifest::read_files);
        let denied = caps.difference(Caps::allowed_for(name));
        sys::print(b"AriInit: lanzar ");
        sys::print(name.as_bytes());
        sys::print(if manifest.is_some() {
            b" con manifiesto ["
        } else {
            b" sin manifiesto ["
        });
        let mut first = true;
        for c in caps.names() {
            if !first {
                sys::print(b" ");
            }
            first = false;
            sys::print(c.as_bytes());
        }
        sys::print(b"] memoria ");
        let mut buf = [0u8; 20];
        sys::print(aricode_rt::fmt_u64(memory_kib as u64, &mut buf));
        sys::print(b" KiB");
        if let Some(files) = read_files.clone().filter(|f| f.clone().count() > 0) {
            sys::print(b" solo lectura [");
            for (i, f) in files.enumerate() {
                if i > 0 {
                    sys::print(b" ");
                }
                sys::print(f.as_bytes());
            }
            sys::print(b"]");
        }
        sys::print(b"\n");
        if !denied.is_empty() {
            sys::print(b"AriInit: denegado, pide capacidades del sistema:");
            for c in denied.names() {
                sys::print(b" ");
                sys::print(c.as_bytes());
            }
            sys::print(b"\n");
            return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
        }
        // Los duplicados preparados se cierran solos si cualquier camino de
        // error sale antes de transferirlos (`Granted::take` los libera al
        // kernel). Antes, una denegacion tras conceder SYSTEM dejaba una
        // entrada en la tabla por intento.
        let mut granted = Granted::new();
        let mut needs_hw = false;
        if caps.contains(Caps::SYSTEM) {
            match sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        if caps.contains(Caps::FS) {
            let Some(fs) = &self.fs else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match fs.bind(name) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        // Ficheros compartidos de solo lectura (ADR 0016): fsd liga un
        // endpoint a cada uno; uno inexistente deniega el lanzamiento.
        for file in read_files.into_iter().flatten() {
            let Some(fs) = &self.fs else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match fs.share(file) {
                Ok(h) => granted.push(h),
                Err(sys::fs::FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                    sys::print(b"AriInit: denegado, fichero compartido inexistente: ");
                    sys::print(file.as_bytes());
                    sys::print(b"\n");
                    return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
                }
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::NET) {
            match sys::handle_duplicate(self.net, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::WINDOW) {
            let Some(comp) = self.compositor else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match sys::handle_duplicate(comp, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::BLOCK) {
            let Some(block) = self.block else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match sys::handle_duplicate(block, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::INPUT) {
            for ep in &self.inputs[..self.input_count] {
                match sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER) {
                    Ok(h) => granted.push(h),
                    Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
                }
            }
        }
        if needs_hw {
            match sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        // El kernel solo retira los handles si llega a crear el proceso; en
        // cualquier fallo el guard los cierra al salir (cerrar uno ya retirado
        // devuelve BadHandle y se ignora).
        let child =
            match sys::process_spawn_limited(SPAWNER, name, arg, granted.handles(), memory_kib) {
                Ok(c) => {
                    granted.take();
                    c
                }
                Err(e) => {
                    return Launched::Reply(LaunchReply::status(match e {
                        Error::NotFound => LaunchStatus::NotFound,
                        _ => LaunchStatus::Failed,
                    }));
                }
            };
        Launched::Process(child)
    }
}

/// Handles preparados para un lanzamiento: se cierran al soltarse salvo que
/// `take` los haya entregado al kernel.
struct Granted {
    handles: [Handle; 12],
    count: usize,
    transferred: bool,
}

impl Granted {
    fn new() -> Self {
        Self {
            handles: [Handle(0); 12],
            count: 0,
            transferred: false,
        }
    }
    fn push(&mut self, h: Handle) {
        if self.count < self.handles.len() {
            self.handles[self.count] = h;
            self.count += 1;
        } else {
            let _ = sys::handle_close(h);
        }
    }
    fn handles(&self) -> &[Handle] {
        &self.handles[..self.count]
    }
    fn take(&mut self) {
        self.transferred = true;
    }
}

impl Drop for Granted {
    fn drop(&mut self) {
        if !self.transferred {
            for h in &self.handles[..self.count] {
                let _ = sys::handle_close(*h);
            }
        }
    }
}

/// `a` + `b` en `buf`.
fn join<'a>(a: &str, b: &str, buf: &'a mut [u8; 48]) -> Option<&'a str> {
    let total = a.len() + b.len();
    if total > buf.len() {
        return None;
    }
    buf[..a.len()].copy_from_slice(a.as_bytes());
    buf[a.len()..total].copy_from_slice(b.as_bytes());
    core::str::from_utf8(&buf[..total]).ok()
}

/// Lanza el compositor (endpoint propio, framebuffer, SYSTEM, hardware para
/// su buffer trasero y los servicios de entrada) y dos aplicaciones gráficas
/// que comparten pantalla; espera a que las aplicaciones terminen.
fn run_gui(inputs: &[Handle]) -> Result<Handle, u64> {
    let service = sys::endpoint_create().map_err(|_| 1u64)?;
    let mut handles = [Handle(0); 8];
    handles[0] = sys::handle_duplicate(service, rights::READ | rights::WRITE | rights::TRANSFER)
        .map_err(|_| 2u64)?;
    handles[1] =
        sys::handle_duplicate(FRAMEBUFFER, rights::READ | rights::WRITE | rights::TRANSFER)
            .map_err(|_| 3u64)?;
    handles[2] =
        sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER).map_err(|_| 4u64)?;
    handles[3] =
        sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER).map_err(|_| 5u64)?;
    let mut n = 4;
    for ep in inputs.iter().take(4) {
        handles[n] =
            sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER).map_err(|_| 6u64)?;
        n += 1;
    }
    let _compositor =
        sys::process_spawn_with(SPAWNER, "compositor", inputs.len() as u64, &handles[..n])
            .map_err(|_| 7u64)?;
    let mut apps = [Handle(0); 2];
    for (i, app) in apps.iter_mut().enumerate() {
        let ep =
            sys::handle_duplicate(service, rights::WRITE | rights::TRANSFER).map_err(|_| 8u64)?;
        let hw =
            sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER).map_err(|_| 8u64)?;
        *app =
            sys::process_spawn_with(SPAWNER, "gui_demo", i as u64, &[ep, hw]).map_err(|_| 9u64)?;
    }
    for app in apps {
        let code = sys::process_wait(app, TIMEOUT_INFINITE).map_err(|_| 10u64)?;
        let _ = sys::handle_close(app);
        if code != 0 {
            return Err(100 + code);
        }
    }
    Ok(service)
}

fn main(arg: u64) -> u64 {
    say(b"arrancando");
    let mut fs_service = None;
    let mut block_service = None;
    let mut services = 0u64;
    let mut sup = Supervisor::new();
    // 1. Un servicio que termina bien.
    match run("hello", 0) {
        Ok(42) => say(b"hello termino con 42"),
        _ => return 1,
    }
    // 2. Un servicio que muere por fallo: el supervisor lo ve como Killed.
    match run("kernel_read", 0) {
        Err(Error::Killed) => say(b"kernel_read fue terminado por el kernel"),
        _ => return 2,
    }
    // 3. Sin MANAGE no se puede lanzar: un duplicado sin derechos falla.
    let Ok(weak) = sys::handle_duplicate(SPAWNER, 0) else {
        return 3;
    };
    if sys::process_spawn(weak, "hello", 0) != Err(Error::Denied) {
        return 4;
    }
    // 4. Un nombre inexistente en el initrd.
    if sys::process_spawn(SPAWNER, "no-existe", 0) != Err(Error::NotFound) {
        return 5;
    }
    // 5. Un driver que muere se reinicia (Fase 16): el segundo intento arranca.
    match run("crashy_driver", 0) {
        Err(Error::Killed) => say(b"crashy_driver fallo, reiniciando"),
        _ => return 6,
    }
    match run("crashy_driver", 1) {
        Ok(0) => say(b"crashy_driver reiniciado con exito"),
        _ => return 7,
    }
    // 6. Dispositivos PCI vistos desde usuario.
    let mut buf = [0u8; 20];
    match sys::pci_count(PCI_ROOT) {
        Ok(n) => {
            sys::print(b"AriInit: ");
            sys::print(aricode_rt::fmt_u64(n, &mut buf));
            sys::print(b" funciones PCI\n");
        }
        Err(_) => return 8,
    }
    // 7. Servicio de bloques: driver virtio-blk residente + cliente de prueba.
    match find_pci(0x1AF4, 0x1042) {
        Some(index) => {
            let Ok(service) = sys::endpoint_create() else {
                return 9;
            };
            let launched = sup
                .register(
                    "virtio_blk",
                    Kind::Driver {
                        pci_index: index,
                        with_system: false,
                    },
                    Probe::None,
                    service,
                    false,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if !launched {
                return 10;
            }
            services |= SERVICE_BLOCK;
            block_service = Some(service);
            // Servicio de ficheros sobre la particion ARI0 (por nombre en la
            // GPT: el disco de pruebas y la imagen USB la colocan en LBA
            // distintos) y su cliente.
            let Ok(fs_ep) = sys::endpoint_create() else {
                return 12;
            };
            let ari0 = BlockClient::new(service, HARDWARE, 16)
                .ok()
                .and_then(|mut blk| find_partition(&mut blk, "ARI0"));
            let Some((ari0_lba, _)) = ari0 else {
                say(b"sin particion ARI0 en el disco");
                return 12;
            };
            let launched = sup
                .register(
                    "fsd",
                    Kind::Fs {
                        block_service: service,
                        partition_lba: ari0_lba,
                    },
                    Probe::None,
                    fs_ep,
                    false,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if !launched {
                say(b"fsd no arranco");
                return 13;
            }
            services |= SERVICE_FS;
            match run_block_client("fs_client", fs_ep, 0) {
                Ok(0) => {
                    say(b"servicio de ficheros ok");
                    fs_service = Some(fs_ep);
                }
                Ok(code) => {
                    sys::print(b"AriInit: fs_client salio con ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 14;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: fs_client", e);
                    return 14;
                }
            }
            match run_block_client("blk_client", service, ari0_lba + RAW_AREA_OFFSET) {
                Ok(0) => say(b"servicio de bloques ok"),
                Ok(code) => {
                    sys::print(b"AriInit: blk_client salio con ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 11;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: blk_client", e);
                    return 11;
                }
            }
        }
        None => say(b"sin virtio-blk"),
    }
    // 8. Servicio de red: driver virtio-net residente + cliente de prueba.
    let Ok(net_service) = sys::endpoint_create() else {
        return 16;
    };
    match find_pci(0x1AF4, 0x1041) {
        Some(index) => {
            let launched = sup
                .register(
                    "virtio_net",
                    Kind::Driver {
                        pci_index: index,
                        with_system: true,
                    },
                    Probe::None,
                    net_service,
                    true,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if !launched {
                say(b"virtio_net no arranco");
                return 17;
            }
            services |= SERVICE_NET;
            match run_block_client("net_client", net_service, 0) {
                Ok(0) => say(b"servicio de red ok"),
                Ok(code) => {
                    sys::print(b"AriInit: net_client salio con ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 18;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: net_client", e);
                    return 18;
                }
            }
            match run_block_client("tcp_client", net_service, 0) {
                Ok(0) => say(b"servicio TCP ok"),
                Ok(code) => {
                    sys::print(b"AriInit: tcp_client salio con ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 19;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: tcp_client", e);
                    return 19;
                }
            }
            match run_block_client("http_client", net_service, 0) {
                Ok(0) => say(b"cliente HTTP ok"),
                Ok(code) => {
                    sys::print(b"AriInit: http_client salio con ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 20;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: http_client", e);
                    return 20;
                }
            }
        }
        None => say(b"sin virtio-net"),
    }
    // 9. Dispositivos de entrada virtio (teclado, raton): un driver por cada uno.
    let mut input_eps = [Handle(0); 4];
    let mut inputs = 0;
    let mut next = 0;
    while let Some(index) = find_pci_from(0x1AF4, 0x1052, next) {
        next = index + 1;
        if inputs == input_eps.len() {
            break;
        }
        let Ok(ep) = sys::endpoint_create() else {
            return 21;
        };
        let launched = sup
            .register(
                "virtio_input",
                Kind::Driver {
                    pci_index: index,
                    with_system: true,
                },
                Probe::Input,
                ep,
                true,
                Policy::DRIVER,
            )
            .is_some_and(|i| sup.launch(i));
        if launched {
            input_eps[inputs] = ep;
            inputs += 1;
        } else {
            say(b"virtio_input no llego a servir; se descarta");
        }
    }
    // Controladores USB (clase 0c.03): solo los xHCI (interfaz 0x30) tienen
    // driver; usbd enumera teclados y ratones HID y los sirve como un unico
    // dispositivo de entrada, que se publica cuando responde.
    let mut next = 0;
    while let Some(index) = find_pci_class_from(0x0C, 0x03, next) {
        next = index + 1;
        let prog_if = sys::pci_info_ext(PCI_ROOT, index)
            .map(sys::abi::pci_info::prog_if)
            .unwrap_or(0xFF);
        if prog_if != sys::abi::pci_info::XHCI_PROG_IF {
            let info = sys::pci_info(PCI_ROOT, index).unwrap_or(0);
            sys::print(b"AriInit: controlador USB ");
            print_bdf(info);
            sys::print(b" sin driver (interfaz 0x");
            print_hex8(prog_if);
            sys::print(b", solo xHCI)\n");
            continue;
        }
        if inputs == input_eps.len() {
            break;
        }
        let Ok(ep) = sys::endpoint_create() else {
            return 21;
        };
        let launched = sup
            .register(
                "usb_input",
                Kind::Driver {
                    pci_index: index,
                    with_system: true,
                },
                Probe::Input,
                ep,
                true,
                Policy::DRIVER,
            )
            .is_some_and(|i| sup.launch(i));
        if launched {
            say(b"entrada usb_input lista");
            input_eps[inputs] = ep;
            inputs += 1;
        } else {
            say(b"usb_input no llego a servir; se descarta");
        }
    }
    if inputs > 0 {
        services |= SERVICE_INPUT;
        let mut dups = [Handle(0); 4];
        for (d, ep) in dups.iter_mut().zip(&input_eps[..inputs]) {
            let Ok(h) = sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER) else {
                return 23;
            };
            *d = h;
        }
        let result =
            sys::process_spawn_with(SPAWNER, "input_client", inputs as u64, &dups[..inputs])
                .and_then(|child| {
                    let r = sys::process_wait(child, TIMEOUT_INFINITE);
                    let _ = sys::handle_close(child);
                    r
                });
        match result {
            Ok(0) => say(b"servicio de entrada ok"),
            Ok(code) => {
                sys::print(b"AriInit: input_client salio con ");
                sys::print(aricode_rt::fmt_u64(code, &mut buf));
                sys::print(b"\n");
                return 24;
            }
            Err(e) => {
                aricode_rt::print_error(b"AriInit: input_client", e);
                return 24;
            }
        }
    } else {
        say(b"sin dispositivos de entrada");
    }
    // 10. Interfaz grafica: compositor con el framebuffer y las entradas, y dos
    //     aplicaciones que dibujan a la vez.
    let mut compositor = None;
    if arg & sys::abi::init_arg::GUI != 0 {
        match run_gui(&input_eps[..inputs]) {
            Ok(ep) => {
                services |= SERVICE_GUI;
                compositor = Some(ep);
                say(b"gui ok (2 aplicaciones)");
            }
            Err(code) => {
                sys::print(b"AriInit: gui fallo con ");
                sys::print(aricode_rt::fmt_u64(code, &mut buf));
                sys::print(b"\n");
                return 25;
            }
        }
    }
    // 10b. Tormenta de reinicios (solo tests, `restart_storm`): 100 reinicios
    //      de un servicio de prueba bajo el supervisor sin perder recursos.
    if arg & sys::abi::init_arg::RESTART_STORM != 0 {
        let Some(fs_ep) = fs_service else {
            return 40;
        };
        let Ok(mut fs) = FsClient::new(fs_ep, HARDWARE) else {
            return 40;
        };
        if let Err(code) = restart_storm(&mut sup, &mut fs) {
            return code;
        }
    }
    // 11. Arranque A/B: solo ahora, con todos los servicios comprobados, se
    //     confirma el slot pendiente. Cualquier retorno de error anterior deja
    //     el estado pendiente para que AriBoot agote intentos y haga rollback.
    if arg & sys::abi::init_arg::FAIL_LATE != 0 {
        say(b"fallo tardio provocado (init_fail_late): no se confirma el slot");
        if let Some(service) = block_service {
            report_boot_state(service);
        }
        return 99;
    }
    match block_service {
        Some(service) => confirm_boot_slot(arg, service),
        None => say(b"sin servicio de bloques: no se confirma el arranque"),
    }
    say(b"servicios de arranque completados");
    if arg & sys::abi::init_arg::SHELL != 0 {
        let Some(fs) = fs_service else {
            say(b"sin servicio de ficheros, no se lanza el shell");
            return 15;
        };
        let Ok(mut broker) = Broker::new(
            fs_service,
            block_service,
            net_service,
            compositor,
            &input_eps[..inputs],
        ) else {
            return 26;
        };
        // Etapa 2 (ADR 0013): los teclados de usuario alimentan la consola
        // del shell mientras no haya compositor (con GUI los eventos son
        // suyos y cada servicio de entrada admite un solo lector).
        if inputs > 0 && compositor.is_none() {
            let mut ins = [Handle(0); MAX_INPUTS];
            ins[..inputs].copy_from_slice(&input_eps[..inputs]);
            let launched = sup
                .register(
                    "console_bridge",
                    Kind::ConsoleBridge {
                        inputs: ins,
                        count: inputs,
                    },
                    Probe::None,
                    Handle(0),
                    true,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if launched {
                // El anuncio va aqui y no en el puente: init imprime antes
                // de que exista el shell, asi que nada se entrelaza con su
                // primer prompt. Los servicios de entrada guardan lo que se
                // teclee hasta que el puente lo lea.
                sys::print(b"AriInit: puente de consola con ");
                sys::print(aricode_rt::fmt_u64(inputs as u64, &mut buf));
                sys::print(b" dispositivo(s) de entrada\nCONSOLE_BRIDGE_READY\n");
            } else {
                say(b"puente de consola sin arrancar: el shell solo lee COM1 y PS/2");
            }
        }
        match run_shell(fs, net_service, services, &mut broker, &mut sup) {
            Ok(code) => {
                sys::print(b"AriInit: shell terminado con ");
                sys::print(aricode_rt::fmt_u64(code, &mut buf));
                sys::print(b"\n");
            }
            Err(e) => aricode_rt::print_error(b"AriInit: shell", e),
        }
    }
    0
}

aricode_rt::entry!(main);
