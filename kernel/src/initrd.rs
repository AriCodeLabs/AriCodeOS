//! Initrd entregado por AriBoot (`BootInfo::initrd_*`), accesible por el
//! mapa físico. Contiene `init` y los programas de prueba.

use crate::sync::IrqLock;
use aricode_initrd::Initrd;
use boot_protocol::BootInfo;
use log::{info, warn};

static IMAGE: IrqLock<Option<&'static [u8]>> = IrqLock::new(None);

pub fn init(info: &BootInfo) {
    if info.initrd_size == 0 {
        warn!("initrd: ausente");
        return;
    }
    let virt = crate::memory::phys::phys_to_virt(info.initrd_address);
    // SAFETY: AriBoot copió la imagen a memoria de tipo Kernel, que nunca se
    // libera; el mapa físico está activo.
    let bytes: &'static [u8] =
        unsafe { core::slice::from_raw_parts(virt as *const u8, info.initrd_size as usize) };
    match Initrd::parse(bytes) {
        Ok(rd) => {
            info!(
                "initrd: {} ficheros, {} KiB @ {:#x}",
                rd.len(),
                bytes.len() / 1024,
                info.initrd_address
            );
            *IMAGE.lock() = Some(bytes);
        }
        Err(e) => warn!("initrd: imagen invalida ({e:?}), se ignora"),
    }
}

/// Contenido de un fichero del initrd por nombre.
pub fn find(name: &str) -> Option<&'static [u8]> {
    let image = (*IMAGE.lock())?;
    Initrd::parse(image).ok()?.find(name)
}
