//! Driver que falla en su primer arranque (arg = 0) y funciona en el segundo.
#![no_std]
#![no_main]

fn main(arg: u64) -> u64 {
    if arg == 0 {
        // SAFETY: fallo deliberado; la pagina 0 nunca esta mapeada y la
        // direccion se oculta al compilador para que emita la lectura.
        let bad = core::hint::black_box(0x40u64) as *const u64;
        return unsafe { bad.read_volatile() };
    }
    0
}

aricode_rt::entry!(main);
