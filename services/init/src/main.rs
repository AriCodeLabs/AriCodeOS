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

fn say(msg: &[u8]) {
    sys::print(b"AriInit: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn run(name: &str, arg: u64) -> Result<u64, Error> {
    let child = sys::process_spawn(SPAWNER, name, arg)?;
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
    say(b"servicios de arranque completados");
    0
}

user_testprogs::entry!(main);
