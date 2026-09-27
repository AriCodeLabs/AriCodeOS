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
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};

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
    broker: &Broker,
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
    // Atender peticiones de lanzamiento mientras el shell viva.
    loop {
        match sys::process_wait(child, 0) {
            Err(Error::Timeout) => {}
            result => {
                let _ = sys::handle_close(child);
                return result;
            }
        }
        broker.serve_one(20);
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
        })
    }

    /// Atiende como mucho una petición (espera hasta `timeout_ms`).
    fn serve_one(&self, timeout_ms: u64) {
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
        let reply = match LaunchRequest::decode(&msg[..n]) {
            Some(req) => self.launch(req.name_str(), req.arg),
            None => LaunchReply::status(LaunchStatus::Invalid),
        };
        let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
        let _ = sys::handle_close(reply_to);
    }

    /// Lee el manifiesto (`NOMBRE.manifest` en el initrd), aplica la política y
    /// ejecuta el programa hasta que termine.
    fn launch(&self, name: &str, arg: u64) -> LaunchReply {
        let mut path = [0u8; 48];
        let Some(path) = join(name, ".manifest", &mut path) else {
            return LaunchReply::status(LaunchStatus::Invalid);
        };
        let mut text = [0u8; 1024];
        let manifest = match sys::initrd_read(SPAWNER, path, &mut text) {
            Ok(n) => match core::str::from_utf8(&text[..n])
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
            {
                Some(m) => Some(m),
                None => return LaunchReply::status(LaunchStatus::BadManifest),
            },
            Err(Error::NotFound) => None,
            Err(_) => return LaunchReply::status(LaunchStatus::BadManifest),
        };
        let (caps, memory_kib) = match manifest {
            Some(m) if m.name_str() != name => {
                return LaunchReply::status(LaunchStatus::BadManifest)
            }
            Some(m) => (m.caps, m.memory_kib),
            // Sin manifiesto: solo consola y reloj, memoria minima.
            None => (Caps::SYSTEM, 64),
        };
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
        sys::print(b" KiB\n");
        if !denied.is_empty() {
            sys::print(b"AriInit: denegado, pide capacidades del sistema:");
            for c in denied.names() {
                sys::print(b" ");
                sys::print(c.as_bytes());
            }
            sys::print(b"\n");
            return LaunchReply::status(LaunchStatus::Denied);
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
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
            }
        }
        if caps.contains(Caps::FS) {
            let Some(fs) = &self.fs else {
                return LaunchReply::status(LaunchStatus::Denied);
            };
            match fs.bind(name) {
                Ok(h) => granted.push(h),
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::NET) {
            match sys::handle_duplicate(self.net, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::WINDOW) {
            let Some(comp) = self.compositor else {
                return LaunchReply::status(LaunchStatus::Denied);
            };
            match sys::handle_duplicate(comp, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::BLOCK) {
            let Some(block) = self.block else {
                return LaunchReply::status(LaunchStatus::Denied);
            };
            match sys::handle_duplicate(block, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::INPUT) {
            for ep in &self.inputs[..self.input_count] {
                match sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER) {
                    Ok(h) => granted.push(h),
                    Err(_) => return LaunchReply::status(LaunchStatus::Failed),
                }
            }
        }
        if needs_hw {
            match sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return LaunchReply::status(LaunchStatus::Failed),
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
                    return LaunchReply::status(match e {
                        Error::NotFound => LaunchStatus::NotFound,
                        _ => LaunchStatus::Failed,
                    });
                }
            };
        let result = sys::process_wait(child, TIMEOUT_INFINITE);
        let _ = sys::handle_close(child);
        match result {
            Ok(code) => LaunchReply::exited(code),
            Err(Error::Killed) => LaunchReply::status(LaunchStatus::Killed),
            Err(_) => LaunchReply::status(LaunchStatus::Failed),
        }
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
            let Ok(_driver) = start_driver("virtio_blk", index, service, &[]) else {
                return 10;
            };
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
            match start_fsd(fs_ep, service, ari0_lba) {
                Ok(_) => services |= SERVICE_FS,
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: fsd", e);
                    return 13;
                }
            }
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
            let Ok(system) = sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) else {
                return 17;
            };
            match start_driver("virtio_net", index, net_service, &[system]) {
                Ok(_) => services |= SERVICE_NET,
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: virtio_net", e);
                    return 17;
                }
            }
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
        let Ok(system) = sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) else {
            return 21;
        };
        match start_driver("virtio_input", index, ep, &[system]) {
            Ok(_) => {
                input_eps[inputs] = ep;
                inputs += 1;
            }
            Err(e) => {
                aricode_rt::print_error(b"AriInit: virtio_input", e);
                return 22;
            }
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
        say(b"sin dispositivos virtio-input");
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
        let Ok(broker) = Broker::new(
            fs_service,
            block_service,
            net_service,
            compositor,
            &input_eps[..inputs],
        ) else {
            return 26;
        };
        match run_shell(fs, net_service, services, &broker) {
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
