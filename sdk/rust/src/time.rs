//! Time: the system monotonic clock (ms since boot) and sleeps.

use crate::{self as sys, abi, Error, Handle};

/// Milliseconds since boot, read with a `SYSTEM` handle (`READ`).
pub fn uptime_ms(system: Handle) -> Result<u64, Error> {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME, &mut b)? {
        8 => Ok(u64::from_le_bytes(b)),
        _ => Err(Error::Invalid),
    }
}

/// Microseconds since boot (TSC calibrated in the kernel).
pub fn uptime_us(system: Handle) -> Result<u64, Error> {
    let mut b = [0u8; 8];
    match sys::system_info(system, abi::sysinfo::UPTIME_US, &mut b)? {
        8 => Ok(u64::from_le_bytes(b)),
        _ => Err(Error::Invalid),
    }
}

/// Sleeps for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    sys::thread_sleep(ms);
}

/// Clock over a `SYSTEM` handle: `elapsed` since its creation.
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
