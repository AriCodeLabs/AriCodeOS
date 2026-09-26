//! Soporte común de los programas de prueba: entrada, panic y macros.
//!
//! Cada binario define `_start` (en Rust vía [`entry!`] o en ensamblador).

#![no_std]

pub use aricode_sys as sys;

/// Código de salida que usa el panic handler de los programas de prueba.
pub const PANIC_EXIT_CODE: u64 = 255;

/// Define `_start` para un `fn main(arg: u64) -> u64`: `arg` es el valor que
/// pasó quien creó el proceso y el resultado es el código de salida.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn _start(arg: u64) -> ! {
            let code: u64 = $main(arg);
            $crate::sys::process_exit(code)
        }

        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            $crate::sys::print(b"panic en programa de usuario\n");
            $crate::sys::process_exit($crate::PANIC_EXIT_CODE)
        }
    };
}

/// Binario cuya entrada está en ensamblador (`_start` global en el .s).
#[macro_export]
macro_rules! asm_program {
    ($file:literal) => {
        core::arch::global_asm!(include_str!($file));

        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            $crate::sys::process_exit($crate::PANIC_EXIT_CODE)
        }
    };
}
