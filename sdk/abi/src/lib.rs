//! ABI de syscalls de AriCodeOS (sección 22 del documento maestro).
//!
//! Convención x86-64: número en `rax`, argumentos en `rdi, rsi, rdx, r10, r8,
//! r9`, resultado en `rax`. `rcx` y `r11` los destruye la instrucción
//! `syscall`; el resto de registros se conservan. Un resultado negativo en
//! el rango `-4095..=-1` es un [`Error`].
//!
//! Durante `0.x` el ABI puede cambiar (sección 74). Versión actual: 1.

#![no_std]

pub const ABI_VERSION: u32 = 1;

/// Límite exclusivo del espacio de usuario (mitad baja canónica).
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

/// Números de syscall.
pub mod nr {
    /// `process_exit(code)`: nunca retorna.
    pub const PROCESS_EXIT: u64 = 0;
    /// `console_write(ptr, len) -> bytes escritos`.
    pub const CONSOLE_WRITE: u64 = 1;
    /// `thread_yield()`.
    pub const THREAD_YIELD: u64 = 2;
    /// `thread_sleep(ms)`.
    pub const THREAD_SLEEP: u64 = 3;
    /// `thread_get_tid() -> tid`.
    pub const THREAD_GET_TID: u64 = 4;
    /// `abi_version() -> ABI_VERSION`.
    pub const ABI_VERSION: u64 = 5;
}

/// Errores devueltos por el kernel (negados en `rax`).
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Número de syscall desconocido.
    NoSys = 1,
    /// Puntero o rango fuera del espacio de usuario o no mapeado.
    Fault = 2,
    /// Argumento inválido.
    Invalid = 3,
}

impl Error {
    pub const fn to_raw(self) -> i64 {
        -(self as i64)
    }

    pub const fn from_raw(raw: i64) -> Option<Error> {
        match -raw {
            1 => Some(Error::NoSys),
            2 => Some(Error::Fault),
            3 => Some(Error::Invalid),
            _ => None,
        }
    }
}

/// Convierte el `rax` devuelto en `Result`.
pub const fn decode(raw: i64) -> Result<u64, Error> {
    if raw < 0 && raw >= -4095 {
        match Error::from_raw(raw) {
            Some(e) => Err(e),
            None => Err(Error::Invalid),
        }
    } else {
        Ok(raw as u64)
    }
}

/// `true` si `[ptr, ptr+len)` cae íntegramente en el espacio de usuario.
pub const fn user_range_ok(ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(end) => end <= USER_SPACE_END,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_round_trip_and_decode() {
        assert_eq!(Error::Fault.to_raw(), -2);
        assert_eq!(Error::from_raw(-2), Some(Error::Fault));
        assert_eq!(Error::from_raw(-99), None);
        assert_eq!(decode(-1), Err(Error::NoSys));
        assert_eq!(
            decode(-4096),
            Ok((-4096i64) as u64),
            "fuera del rango de error es un valor"
        );
        assert_eq!(decode(42), Ok(42));
        assert_eq!(
            decode(-77),
            Err(Error::Invalid),
            "codigo desconocido se normaliza"
        );
    }

    #[test]
    fn user_range_checks() {
        assert!(user_range_ok(0x40_0000, 4096));
        assert!(user_range_ok(USER_SPACE_END - 16, 16));
        assert!(!user_range_ok(USER_SPACE_END - 16, 17));
        assert!(!user_range_ok(0xFFFF_FFFF_8000_0000, 1));
        assert!(!user_range_ok(u64::MAX, 1));
    }
}
