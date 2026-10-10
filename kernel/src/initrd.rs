//! Initrd entregado por AriBoot (`BootInfo::initrd_*`), accesible por el
//! mapa físico. Contiene `init` y los programas de prueba.

use crate::sync::IrqLock;
use aricode_initrd::Initrd;
use boot_protocol::BootInfo;
use log::{info, warn};

static IMAGE: IrqLock<Option<&'static [u8]>> = IrqLock::new(None);
/// Prefijo del SHA-256 del initrd tal como lo encuentra el kernel al entrar
/// (antes de tocar memoria): si difiere del que anuncia AriBoot, la imagen
/// se corrompió antes del kernel; si difiere del calculado en `init`, después.
static ENTRY_DIGEST: IrqLock<[u8; 4]> = IrqLock::new([0; 4]);

fn digest_prefix(bytes: &[u8]) -> [u8; 4] {
    let d = aricode_crypto::sha256::sha256(bytes);
    [d[0], d[1], d[2], d[3]]
}

fn bytes_of(info: &BootInfo) -> &'static [u8] {
    let virt = crate::memory::phys::phys_to_virt(info.initrd_address);
    // SAFETY: AriBoot copió la imagen a páginas que el kernel conserva
    // (`BootInfo::kernel_ranges`); el mapa físico está activo.
    unsafe { core::slice::from_raw_parts(virt as *const u8, info.initrd_size as usize) }
}

/// Toma la huella del initrd en `kernel_entry`, antes de PMM, VMM o heap.
pub fn snapshot_digest(info: &BootInfo) {
    if info.initrd_size != 0 {
        *ENTRY_DIGEST.lock() = digest_prefix(bytes_of(info));
    }
}

pub fn init(info: &BootInfo) {
    if info.initrd_size == 0 {
        warn!("initrd: ausente");
        return;
    }
    let bytes = bytes_of(info);
    let entry = *ENTRY_DIGEST.lock();
    let now = digest_prefix(bytes);
    match Initrd::parse(bytes) {
        Ok(rd) => {
            info!(
                "initrd: {} ficheros, {} KiB @ {:#x}, sha256 {:02x}{:02x}{:02x}{:02x}",
                rd.len(),
                bytes.len() / 1024,
                info.initrd_address,
                now[0],
                now[1],
                now[2],
                now[3]
            );
            *IMAGE.lock() = Some(bytes);
        }
        Err(e) => warn!(
            "initrd: imagen invalida ({e:?}), se ignora; sha256 al entrar {:02x}{:02x}{:02x}{:02x}, ahora {:02x}{:02x}{:02x}{:02x}",
            entry[0], entry[1], entry[2], entry[3], now[0], now[1], now[2], now[3]
        ),
    }
}

/// Contenido de un fichero del initrd por nombre.
pub fn find(name: &str) -> Option<&'static [u8]> {
    let image = (*IMAGE.lock())?;
    Initrd::parse(image).ok()?.find(name)
}
