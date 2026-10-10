//! Instantánea del mapa de memoria UEFI con llamadas directas a boot services.
//!
//! `uefi::boot::memory_map` encadena tamaño, reserva y lectura con `assert!`s
//! y sin trazas; en hardware real hace falta saber en cuál de las llamadas se
//! para el firmware (docs/hardware.md), así que cada una se anota antes y
//! después. Los descriptores se recorren con el iterador de `boot-protocol`,
//! el mismo que usa el kernel.

use alloc::vec::Vec;
use boot_protocol::{MemoryDescriptor, MemoryMap};
use log::info;
use uefi::{table, Status};

/// Margen para las reservas que el cargador hace después de leer el mapa
/// (el mismo que usa Linux).
const EXTRA_DESCRIPTORS: usize = 8;

pub struct Snapshot {
    /// Buffer con los descriptores; `map.descriptors` apunta a él.
    _buf: Vec<u8>,
    pub map: MemoryMap,
}

impl Snapshot {
    pub fn regions(&self) -> impl Iterator<Item = MemoryDescriptor> + '_ {
        // SAFETY: `_buf` vive tanto como `self` e identidad-mapeado (boot
        // services activos); no se modifica tras construirlo.
        unsafe { self.map.iter(0) }
    }
}

/// Lee el mapa de memoria actual. Error si el firmware responde fuera de la
/// especificación (tamaño o descriptor imposibles).
pub fn snapshot() -> Result<Snapshot, &'static str> {
    let st = table::system_table_raw().ok_or("sin tabla de sistema")?;
    // SAFETY: boot services activos (antes de ExitBootServices) y tabla
    // registrada por `uefi::helpers::init`.
    let bs = unsafe { &*(*st.as_ptr()).boot_services };
    let (mut size, mut key, mut desc_size, mut desc_version) = (0usize, 0usize, 0usize, 0u32);
    info!("memoria: GetMemoryMap (tamano)");
    // SAFETY: llamada de boot services con punteros a locales válidos; con
    // tamaño 0 el firmware solo rellena los tamaños.
    let status = unsafe {
        (bs.get_memory_map)(
            &mut size,
            core::ptr::null_mut(),
            &mut key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    info!(
        "memoria: GetMemoryMap -> {status:?}, {size} bytes, descriptor de {desc_size} (version {desc_version})"
    );
    if status != Status::BUFFER_TOO_SMALL
        || size == 0
        || desc_size < core::mem::size_of::<MemoryDescriptor>()
    {
        return Err("GetMemoryMap: respuesta invalida");
    }
    let len = size + EXTRA_DESCRIPTORS * desc_size;
    let mut buf = alloc::vec![0u8; len];
    size = len;
    // SAFETY: `buf` tiene `size` bytes escribibles; el resto son locales.
    let status = unsafe {
        (bs.get_memory_map)(
            &mut size,
            buf.as_mut_ptr().cast(),
            &mut key,
            &mut desc_size,
            &mut desc_version,
        )
    };
    info!("memoria: GetMemoryMap({len}) -> {status:?}, {size} bytes");
    if status != Status::SUCCESS || size > len || desc_size == 0 || size % desc_size != 0 {
        return Err("GetMemoryMap: respuesta invalida");
    }
    let map = MemoryMap {
        descriptors: buf.as_ptr() as u64,
        descriptor_size: desc_size as u32,
        descriptor_count: (size / desc_size) as u32,
    };
    Ok(Snapshot { _buf: buf, map })
}
