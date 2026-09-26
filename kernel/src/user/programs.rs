//! Programas de usuario embebidos (`programs.s`).

use core::arch::global_asm;

global_asm!(include_str!("programs.s"));

macro_rules! programs {
    ($($name:ident => $start:ident, $end:ident;)*) => {
        extern "C" {
            $( static $start: u8; static $end: u8; )*
        }

        /// Blob de un programa por nombre de test, o `None`.
        pub fn by_name(name: &str) -> Option<&'static [u8]> {
            match name {
                $( stringify!($name) => {
                    // SAFETY: símbolos del ensamblador que delimitan bytes en .rodata.
                    let (s, e) = unsafe { (&$start as *const u8, &$end as *const u8) };
                    Some(unsafe { core::slice::from_raw_parts(s, e as usize - s as usize) })
                } )*
                _ => None,
            }
        }
    };
}

programs! {
    hello => user_prog_hello_start, user_prog_hello_end;
    kernel_read => user_prog_kernel_read_start, user_prog_kernel_read_end;
    kernel_write => user_prog_kernel_write_start, user_prog_kernel_write_end;
    privileged => user_prog_privileged_start, user_prog_privileged_end;
    bad_syscall => user_prog_bad_syscall_start, user_prog_bad_syscall_end;
    bad_pointer => user_prog_bad_pointer_start, user_prog_bad_pointer_end;
    sleep_yield => user_prog_sleep_yield_start, user_prog_sleep_yield_end;
    handles => user_prog_handles_start, user_prog_handles_end;
}
