//! Agota la tabla de handles del proceso (etapa 3, cuotas): crea endpoints
//! hasta que el kernel responde `Limit`, comprueba que el rechazo es
//! predecible (la tabla tiene 256 entradas y ya hay cuatro ocupadas), libera
//! todos y demuestra que se puede seguir creando. El límite es del proceso:
//! los demás no lo notan.
#![no_std]
#![no_main]

use aricode_rt::sys::{self, Error, Handle};

/// Entradas de la tabla de handles de un proceso (`HANDLE_LIMIT` del kernel).
const TABLE: usize = 256;
/// Proceso, hilo, consola y el `SYSTEM` que el broker da sin manifiesto.
const HELD: usize = 4;

fn main(_arg: u64) -> u64 {
    let mut created = [Handle(0); TABLE];
    let mut count = 0;
    let stop = loop {
        match sys::endpoint_create() {
            Ok(h) if count < TABLE => {
                created[count] = h;
                count += 1;
            }
            Ok(_) => break Err(()),
            Err(e) => break Ok(e),
        }
    };
    let mut b = [0u8; 20];
    match stop {
        Ok(Error::Limit) if count == TABLE - HELD => {
            sys::print(b"handle_hog: limite de handles alcanzado (Limit) tras crear ");
            sys::print(aricode_rt::fmt_u64(count as u64, &mut b));
            sys::print(b" endpoints\n");
        }
        Ok(e) => {
            sys::print(b"handle_hog: error inesperado tras ");
            sys::print(aricode_rt::fmt_u64(count as u64, &mut b));
            sys::print(b" endpoints: ");
            aricode_rt::print_error(b"", e);
            return 1;
        }
        Err(()) => {
            sys::print(b"handle_hog: la tabla no tiene limite\n");
            return 2;
        }
    }
    for h in &created[..count] {
        if sys::handle_close(*h).is_err() {
            return 3;
        }
    }
    match sys::endpoint_create() {
        Ok(h) => {
            let _ = sys::handle_close(h);
            sys::print(b"handle_hog: tabla liberada, se puede seguir creando ok\n");
            0
        }
        Err(_) => 4,
    }
}

aricode_rt::entry!(main);
