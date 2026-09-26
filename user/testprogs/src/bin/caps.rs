//! Capabilities (Fase 13): revocacion y derivacion. Exito = 21; si no, el paso.
#![no_std]
#![no_main]

use user_testprogs::sys::{self, rights, Error, Handle};

fn main() -> u64 {
    // Derivacion: un duplicado con menos derechos sigue siendo el mismo objeto.
    let Ok(dup) = sys::handle_duplicate(Handle::CONSOLE, rights::WRITE) else {
        return 1;
    };
    if sys::console_write(dup, b"escribo por el duplicado\n").is_err() {
        return 2;
    }
    // Revocar exige MANAGE: la consola no lo tiene.
    if sys::object_revoke(Handle::CONSOLE) != Err(Error::Denied) {
        return 3;
    }
    // El proceso si tiene MANAGE sobre si mismo: duplicar sin MANAGE y revocar
    // por el original invalida tambien el duplicado.
    let Ok(self_ro) = sys::handle_duplicate(Handle::SELF_PROCESS, rights::READ) else {
        return 4;
    };
    if sys::object_id(self_ro).is_err() {
        return 5;
    }
    if sys::object_revoke(Handle::SELF_PROCESS).is_err() {
        return 6;
    }
    if sys::object_id(self_ro) != Err(Error::Revoked) {
        return 7;
    }
    if sys::object_id(Handle::SELF_PROCESS) != Err(Error::Revoked) {
        return 8;
    }
    // Un handle revocado se puede cerrar, pero no duplicar.
    if sys::handle_duplicate(self_ro, rights::READ) != Err(Error::Revoked) {
        return 9;
    }
    if sys::handle_close(self_ro).is_err() {
        return 10;
    }
    // La consola sigue viva: la revocacion es por objeto.
    if sys::console_write(dup, b"la consola sigue viva\n").is_err() {
        return 11;
    }
    21
}

user_testprogs::entry!(main);
