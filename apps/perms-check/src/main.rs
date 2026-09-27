//! `perms_check`: regresión de la auditoría del SDK (hallazgo 2). Recibe tres
//! handles extra (`SYSTEM`, endpoint, `HARDWARE`), cierra el primero y luego
//! el del medio, y exige que `permissions::granted` siga listando los que
//! quedan: un hueco no esconde los permisos posteriores.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, kind, permissions, Handle};

/// Devuelve cuántos handles extra enumera `granted` y una máscara por tipo.
fn survey() -> (u32, bool, bool, bool) {
    let (mut extra, mut system, mut endpoint, mut hardware) = (0, false, false, false);
    permissions::granted(|h, _| {
        if h.0 < permissions::FIRST_EXTRA {
            return;
        }
        extra += 1;
        system |= permissions::is(h, kind::SYSTEM);
        endpoint |= permissions::is(h, kind::ENDPOINT);
        hardware |= permissions::is(h, kind::HARDWARE);
    });
    (extra, system, endpoint, hardware)
}

fn report(step: &[u8], s: (u32, bool, bool, bool)) {
    sys::print(b"perms_check: ");
    sys::print(step);
    sys::print(b":");
    if s.1 {
        sys::print(b" sistema");
    }
    if s.2 {
        sys::print(b" endpoint");
    }
    if s.3 {
        sys::print(b" hardware");
    }
    if s.0 == 0 {
        sys::print(b" (ninguno)");
    }
    sys::print(b"\n");
}

fn main(_arg: u64) -> u64 {
    let start = survey();
    report(b"al arrancar", start);
    if start != (3, true, true, true) {
        return 1;
    }
    // Renunciar al primero (SYSTEM, 0x10004): debe seguir viendose el resto.
    if permissions::drop_handle(Handle(permissions::FIRST_EXTRA)).is_err() {
        return 2;
    }
    let after_first = survey();
    report(b"sin el sistema", after_first);
    if after_first != (2, false, true, true) {
        sys::print(b"perms_check: un hueco oculto los permisos restantes\n");
        return 3;
    }
    // Hueco en el medio: cerrar el endpoint (0x10005) y conservar HARDWARE.
    if permissions::drop_handle(Handle(permissions::FIRST_EXTRA + 1)).is_err() {
        return 4;
    }
    let after_middle = survey();
    report(b"sin el endpoint", after_middle);
    if after_middle != (1, false, false, true) {
        return 5;
    }
    sys::print(b"perms_check: la enumeracion salta huecos ok\n");
    0
}

aricode_rt::entry!(main);
