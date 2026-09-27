//! Tiempo: reloj monótono del sistema (ms desde el arranque) y esperas.

use crate::{self as sys, abi, Error, Handle};

/// Milisegundos desde el arranque, leídos con un handle `SYSTEM` (`READ`).
pub fn uptime_ms(system: Handle) -> Result<u64, Error> {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME, &mut b)? {
        8 => Ok(u64::from_le_bytes(b)),
        _ => Err(Error::Invalid),
    }
}

/// Duerme al menos `ms` milisegundos.
pub fn sleep_ms(ms: u64) {
    sys::thread_sleep(ms);
}

/// Reloj sobre un handle `SYSTEM`: `elapsed` desde su creación.
pub struct Clock {
    system: Handle,
    start: u64,
}

impl Clock {
    pub fn new(system: Handle) -> Result<Self, Error> {
        Ok(Self {
            system,
            start: uptime_ms(system)?,
        })
    }
    pub fn now_ms(&self) -> u64 {
        uptime_ms(self.system).unwrap_or(0)
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.now_ms().saturating_sub(self.start)
    }
}
