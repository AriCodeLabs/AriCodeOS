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

use user_testprogs::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};

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
fn run_block_client(name: &str, service: Handle) -> Result<u64, Error> {
    let ep = sys::handle_duplicate(service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    let child = sys::process_spawn_with(SPAWNER, name, 0, &[ep, hw])?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Lanza el shell con el endpoint de fsd (WRITE), hardware para su buffer,
/// el sistema (READ|MANAGE), la entrada de consola, la raíz PCI solo
/// lectura y el endpoint de netd (WRITE); `arg` = servicios activos. Espera
/// a que termine.
fn run_shell(fs_service: Handle, net_service: Handle, services: u64) -> Result<u64, Error> {
    let fsd = sys::handle_duplicate(fs_service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    let system = sys::handle_duplicate(SYSTEM, rights::READ | rights::MANAGE | rights::TRANSFER)?;
    let console_in = sys::handle_duplicate(CONSOLE_IN, rights::READ | rights::TRANSFER)?;
    let pci = sys::handle_duplicate(PCI_ROOT, rights::READ | rights::TRANSFER)?;
    let netd = sys::handle_duplicate(net_service, rights::WRITE | rights::TRANSFER)?;
    let child = sys::process_spawn_with(
        SPAWNER,
        "shell",
        services,
        &[fsd, hw, system, console_in, pci, netd],
    )?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Lanza el compositor (endpoint propio, framebuffer, SYSTEM, hardware para
/// su buffer trasero y los servicios de entrada) y dos aplicaciones gráficas
/// que comparten pantalla; espera a que las aplicaciones terminen.
fn run_gui(inputs: &[Handle]) -> Result<(), u64> {
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
    Ok(())
}

fn main(arg: u64) -> u64 {
    say(b"arrancando");
    let mut fs_service = None;
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
            sys::print(user_testprogs::fmt_u64(n, &mut buf));
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
            // Servicio de ficheros sobre la particion ARI0 (LBA 34) y su cliente.
            let Ok(fs_ep) = sys::endpoint_create() else {
                return 12;
            };
            match start_fsd(fs_ep, service, 34) {
                Ok(_) => services |= SERVICE_FS,
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: fsd", e);
                    return 13;
                }
            }
            match run_block_client("fs_client", fs_ep) {
                Ok(0) => {
                    say(b"servicio de ficheros ok");
                    fs_service = Some(fs_ep);
                }
                Ok(code) => {
                    sys::print(b"AriInit: fs_client salio con ");
                    sys::print(user_testprogs::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 14;
                }
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: fs_client", e);
                    return 14;
                }
            }
            match run_block_client("blk_client", service) {
                Ok(0) => say(b"servicio de bloques ok"),
                Ok(code) => {
                    sys::print(b"AriInit: blk_client salio con ");
                    sys::print(user_testprogs::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 11;
                }
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: blk_client", e);
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
                    user_testprogs::print_error(b"AriInit: virtio_net", e);
                    return 17;
                }
            }
            match run_block_client("net_client", net_service) {
                Ok(0) => say(b"servicio de red ok"),
                Ok(code) => {
                    sys::print(b"AriInit: net_client salio con ");
                    sys::print(user_testprogs::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 18;
                }
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: net_client", e);
                    return 18;
                }
            }
            match run_block_client("tcp_client", net_service) {
                Ok(0) => say(b"servicio TCP ok"),
                Ok(code) => {
                    sys::print(b"AriInit: tcp_client salio con ");
                    sys::print(user_testprogs::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 19;
                }
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: tcp_client", e);
                    return 19;
                }
            }
            match run_block_client("http_client", net_service) {
                Ok(0) => say(b"cliente HTTP ok"),
                Ok(code) => {
                    sys::print(b"AriInit: http_client salio con ");
                    sys::print(user_testprogs::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 20;
                }
                Err(e) => {
                    user_testprogs::print_error(b"AriInit: http_client", e);
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
                user_testprogs::print_error(b"AriInit: virtio_input", e);
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
                sys::print(user_testprogs::fmt_u64(code, &mut buf));
                sys::print(b"\n");
                return 24;
            }
            Err(e) => {
                user_testprogs::print_error(b"AriInit: input_client", e);
                return 24;
            }
        }
    } else {
        say(b"sin dispositivos virtio-input");
    }
    // 10. Interfaz grafica: compositor con el framebuffer y las entradas, y dos
    //     aplicaciones que dibujan a la vez.
    if arg & sys::abi::init_arg::GUI != 0 {
        match run_gui(&input_eps[..inputs]) {
            Ok(()) => {
                services |= SERVICE_GUI;
                say(b"gui ok (2 aplicaciones)");
            }
            Err(code) => {
                sys::print(b"AriInit: gui fallo con ");
                sys::print(user_testprogs::fmt_u64(code, &mut buf));
                sys::print(b"\n");
                return 25;
            }
        }
    }
    say(b"servicios de arranque completados");
    if arg & sys::abi::init_arg::SHELL != 0 {
        let Some(fs) = fs_service else {
            say(b"sin servicio de ficheros, no se lanza el shell");
            return 15;
        };
        match run_shell(fs, net_service, services) {
            Ok(code) => {
                sys::print(b"AriInit: shell terminado con ");
                sys::print(user_testprogs::fmt_u64(code, &mut buf));
                sys::print(b"\n");
            }
            Err(e) => user_testprogs::print_error(b"AriInit: shell", e),
        }
    }
    0
}

user_testprogs::entry!(main);
