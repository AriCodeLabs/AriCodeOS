//! Servicio de prueba que nunca responde (auditoria 6ee19fb, hallazgo 4):
//! recibe su endpoint y se queda dormido para siempre sin atender nada. El
//! supervisor debe terminarlo cuando venza la sonda de arranque.
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(_arg: u64) -> u64 {
    sys::print(b"hang_service: arrancado y sin intencion de responder\n");
    loop {
        sys::thread_sleep(60_000);
    }
}

aricode_rt::entry!(main);
