//! ABI de syscalls de AriCodeOS (sección 22 del documento maestro).
//!
//! Convención x86-64: número en `rax`, argumentos en `rdi, rsi, rdx, r10, r8,
//! r9`, resultado en `rax`. `rcx` y `r11` los destruye la instrucción
//! `syscall`; el resto de registros se conservan. Un resultado negativo en
//! el rango `-4095..=-1` es un [`Error`].
//!
//! Los recursos se nombran por handles (sección 24): enteros opacos de 32
//! bits con derechos asociados; `0` nunca es válido. Todo proceso arranca con
//! los handles de [`handle::SELF_PROCESS`], [`handle::MAIN_THREAD`] y
//! [`handle::CONSOLE`].
//!
//! Durante `0.x` el ABI puede cambiar (sección 74). Versión actual: 2.

#![no_std]

pub const ABI_VERSION: u32 = 2;

/// Handles iniciales de todo proceso.
pub mod handle {
    /// Handle con el que `init` recibe el objeto `SPAWNER` (`MANAGE`).
    pub const INIT_SPAWNER: u32 = 0x0001_0004;
    /// El propio proceso (`MANAGE | READ | DUPLICATE`).
    pub const SELF_PROCESS: u32 = 0x0001_0001;
    /// El hilo principal (`READ | WAIT | DUPLICATE`).
    pub const MAIN_THREAD: u32 = 0x0001_0002;
    /// Consola del kernel (`WRITE | DUPLICATE`).
    pub const CONSOLE: u32 = 0x0001_0003;
}

/// Derechos (bits) tal y como los ve el espacio de usuario.
pub mod rights {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const DUPLICATE: u32 = 1 << 2;
    pub const TRANSFER: u32 = 1 << 3;
    pub const WAIT: u32 = 1 << 4;
    pub const SIGNAL: u32 = 1 << 5;
    pub const MANAGE: u32 = 1 << 6;
}

/// Tipos de objeto devueltos por `object_kind`.
pub mod kind {
    pub const THREAD: u64 = 1;
    pub const PROCESS: u64 = 2;
    pub const CONSOLE: u64 = 3;
    pub const ENDPOINT: u64 = 4;
    /// Capacidad de crear procesos desde el initrd (solo `init` la recibe).
    pub const SPAWNER: u64 = 5;
}

/// Límite exclusivo del espacio de usuario (mitad baja canónica).
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

/// Números de syscall.
pub mod nr {
    /// `process_exit(code)`: nunca retorna.
    pub const PROCESS_EXIT: u64 = 0;
    /// `console_write(handle, ptr, len) -> bytes escritos`; exige `WRITE`.
    pub const CONSOLE_WRITE: u64 = 1;
    /// `thread_yield()`.
    pub const THREAD_YIELD: u64 = 2;
    /// `thread_sleep(ms)`.
    pub const THREAD_SLEEP: u64 = 3;
    /// `thread_get_tid() -> tid`.
    pub const THREAD_GET_TID: u64 = 4;
    /// `abi_version() -> ABI_VERSION`.
    pub const ABI_VERSION: u64 = 5;
    /// `handle_close(handle)`.
    pub const HANDLE_CLOSE: u64 = 6;
    /// `handle_duplicate(handle, rights) -> handle`; exige `DUPLICATE` y
    /// `rights` subconjunto de los del origen.
    pub const HANDLE_DUPLICATE: u64 = 7;
    /// `object_kind(handle) -> kind::*`.
    pub const OBJECT_KIND: u64 = 8;
    /// `object_id(handle) -> tid | pid`; exige `READ`. La consola devuelve 0.
    pub const OBJECT_ID: u64 = 9;
    /// `object_revoke(handle)`: invalida el objeto para todos sus handles,
    /// en todos los procesos; exige `MANAGE`.
    pub const OBJECT_REVOKE: u64 = 10;
    /// `endpoint_create() -> handle` con `READ|WRITE|DUPLICATE|TRANSFER|MANAGE`.
    pub const ENDPOINT_CREATE: u64 = 11;
    /// `ipc_send(ep, data_ptr, data_len, handles_ptr, handles_len, timeout_ms)`;
    /// exige `WRITE` sobre `ep` y `TRANSFER` sobre cada handle enviado (que
    /// se retira del emisor). `timeout_ms`: 0 = no bloquear,
    /// [`TIMEOUT_INFINITE`] = esperar siempre.
    pub const IPC_SEND: u64 = 12;
    /// `ipc_receive(ep, buf_ptr, buf_len, handles_out_ptr, handles_max, timeout_ms)
    /// -> data_len | (handles_recibidos << 32)`; exige `READ` sobre `ep`.
    pub const IPC_RECEIVE: u64 = 13;
    /// `process_spawn(spawner, name_ptr, name_len, arg) -> handle de proceso`;
    /// exige `MANAGE` sobre un objeto `SPAWNER`. El hijo recibe `arg` en `rdi`.
    pub const PROCESS_SPAWN: u64 = 14;
    /// `process_wait(process, timeout_ms) -> código de salida`; exige `WAIT`.
    /// Un proceso terminado por el kernel devuelve `Error::Killed`.
    pub const PROCESS_WAIT: u64 = 15;
}

/// Límites de un mensaje IPC (sección 23: mensajes pequeños y rápidos).
pub const IPC_MAX_DATA: usize = 256;
pub const IPC_MAX_HANDLES: usize = 4;
/// Capacidad de la cola de un endpoint (protección frente a agotamiento).
pub const IPC_QUEUE_CAPACITY: usize = 16;
/// Timeout que significa "esperar indefinidamente".
pub const TIMEOUT_INFINITE: u64 = u64::MAX;

/// Descompone el resultado de `ipc_receive`.
pub const fn receive_result(raw: u64) -> (usize, usize) {
    ((raw & 0xFFFF_FFFF) as usize, (raw >> 32) as usize)
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
    /// Handle inexistente, cerrado o inventado.
    BadHandle = 4,
    /// El handle no tiene los derechos necesarios.
    Denied = 5,
    /// Intento de obtener más derechos de los que se tienen.
    Escalation = 6,
    /// Límite de recursos del proceso.
    Limit = 7,
    /// El objeto fue revocado: el handle existe pero ya no da acceso.
    Revoked = 8,
    /// Se agotó el plazo (o `timeout_ms == 0` y la operación bloquearía).
    Timeout = 9,
    /// Mensaje o buffer por encima de los límites.
    TooBig = 10,
    /// El proceso esperado fue terminado por el kernel (excepción en ring 3).
    Killed = 11,
    /// No existe un fichero con ese nombre en el initrd.
    NotFound = 12,
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
            4 => Some(Error::BadHandle),
            5 => Some(Error::Denied),
            6 => Some(Error::Escalation),
            7 => Some(Error::Limit),
            8 => Some(Error::Revoked),
            9 => Some(Error::Timeout),
            10 => Some(Error::TooBig),
            11 => Some(Error::Killed),
            12 => Some(Error::NotFound),
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
    fn receive_result_packs_len_and_handles() {
        assert_eq!(receive_result(4 | (2 << 32)), (4, 2));
        assert_eq!(receive_result(0), (0, 0));
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
