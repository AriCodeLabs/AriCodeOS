//! Permisos: en AriCodeOS un programa solo puede hacer lo que sus handles le
//! permiten. Este módulo deja inspeccionarlos: qué tipo de objeto hay tras
//! cada uno y cuáles recibió al arrancar.

use crate::{self as sys, kind, Error, Handle};

/// Primer handle que un proceso recibe además de los tres iniciales.
pub const FIRST_EXTRA: u32 = 0x0001_0004;
/// Máximo de handles extra que se recorren en [`granted`].
pub const MAX_EXTRA: u32 = 16;

/// Nombre legible del tipo de objeto de un handle, o `None` si no existe.
pub fn kind_name(handle: Handle) -> Option<&'static str> {
    Some(match sys::object_kind(handle).ok()? {
        kind::THREAD => "hilo",
        kind::PROCESS => "proceso",
        kind::CONSOLE => "consola",
        kind::ENDPOINT => "endpoint",
        kind::SPAWNER => "spawner",
        kind::IRQ => "irq",
        kind::MMIO => "mmio",
        kind::DMA => "dma",
        kind::HARDWARE => "hardware",
        kind::PCI_ROOT => "raiz pci",
        kind::SYSTEM => "sistema",
        _ => "desconocido",
    })
}

/// Recorre los handles iniciales y los extra que el creador entregó
/// (`0x10004..`), llamando a `f(handle, nombre)` por cada uno que exista.
pub fn granted(mut f: impl FnMut(Handle, &'static str)) {
    for h in [Handle::SELF_PROCESS, Handle::MAIN_THREAD, Handle::CONSOLE] {
        if let Some(name) = kind_name(h) {
            f(h, name);
        }
    }
    for i in 0..MAX_EXTRA {
        let h = Handle(FIRST_EXTRA + i);
        match kind_name(h) {
            Some(name) => f(h, name),
            None => break,
        }
    }
}

/// `true` si el handle existe y es del tipo indicado (`kind::*`).
pub fn is(handle: Handle, expected: u64) -> bool {
    sys::object_kind(handle) == Ok(expected)
}

/// Cierra un handle que ya no se necesita: renunciar a un permiso es la
/// forma más barata de reducir la superficie de un programa.
pub fn drop_handle(handle: Handle) -> Result<(), Error> {
    sys::handle_close(handle)
}
