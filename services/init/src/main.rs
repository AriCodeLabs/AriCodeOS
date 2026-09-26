//! AriInit (Fase 15): primer proceso de usuario. Recibe del kernel el handle
//! `SPAWNER` (0x10004) con derecho `MANAGE`, único que permite crear procesos
//! del initrd. Lanza los servicios de arranque y supervisa su salida.
//!
//! En 0.0.7 su lista de servicios es de prueba: demuestra lanzar, esperar y
//! reaccionar a un proceso terminado por fallo.
#![no_std]
#![no_main]

use user_testprogs::sys::{self, Error, Handle, TIMEOUT_INFINITE};

const SPAWNER: Handle = Handle(0x0001_0004);
const PCI_ROOT: Handle = Handle(0x0001_0005);

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
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (0..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::vendor(v) == vendor && sys::abi::pci_info::device(v) == device
        })
    })
}

/// Reclama un dispositivo y lanza su driver con las capabilities transferidas.
///
/// Contrato con el driver: recibe en orden el espacio de configuración, la
/// capacidad de hardware, la IRQ (si existe) y solo los BARs presentes;
/// `arg` lleva una máscara: bit 15 = hay IRQ, bits 0..5 = BAR i presente.
fn start_driver(name: &str, index: u64) -> Result<u64, Error> {
    let handles = sys::pci_claim(PCI_ROOT, index)?;
    let mut to_send = [Handle(0); 9];
    let mut n = 0;
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
    let child = sys::process_spawn_with(SPAWNER, name, mask, &to_send[..n])?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

fn main(_arg: u64) -> u64 {
    say(b"arrancando");
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
    // 6. Dispositivos PCI vistos desde usuario y arranque del driver virtio-blk.
    let mut buf = [0u8; 20];
    match sys::pci_count(PCI_ROOT) {
        Ok(n) => {
            sys::print(b"AriInit: ");
            sys::print(user_testprogs::fmt_u64(n, &mut buf));
            sys::print(b" funciones PCI\n");
        }
        Err(_) => return 8,
    }
    match find_pci(0x1AF4, 0x1042) {
        Some(index) => match start_driver("virtio_blk", index) {
            Ok(0) => say(b"virtio-blk listo"),
            Ok(_) | Err(_) => return 9,
        },
        None => say(b"sin virtio-blk"),
    }
    say(b"servicios de arranque completados");
    0
}

user_testprogs::entry!(main);
