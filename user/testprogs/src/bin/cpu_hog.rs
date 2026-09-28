//! Consume CPU sin parar (etapa 3, cuotas y saturación): se lanza en segundo
//! plano con `spawn` para comprobar que el resto del sistema sigue
//! progresando con el planificador por turnos. No termina nunca; muere con
//! el apagado.
#![no_std]
#![no_main]

use aricode_rt::sys;

fn main(_arg: u64) -> u64 {
    sys::print(b"cpu_hog: consumiendo CPU\n");
    let mut x = 0u64;
    loop {
        // La suma se oculta al compilador para que el bucle no se elimine.
        x = core::hint::black_box(x)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
    }
}

aricode_rt::entry!(main);
