// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Wall clock from the CMOS real-time clock (HTTPS certificate dates). The
//! RTC is read once at boot; afterwards the time is that reading plus the
//! uptime. The RTC is assumed to hold UTC (QEMU does; a machine that keeps
//! local time in it is off by its time zone, harmless for certificate
//! validity periods).

use crate::arch::x86_64::port::{inb, outb};
use core::sync::atomic::{AtomicU64, Ordering};
use log::{info, warn};

/// Unix seconds at boot (0: no valid reading).
static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);

fn cmos(reg: u8) -> u8 {
    // SAFETY: CMOS index/data ports; bit 7 of the index keeps NMIs enabled
    // as they were (we never disable them).
    unsafe {
        outb(0x70, reg);
        inb(0x71)
    }
}

fn update_in_progress() -> bool {
    cmos(0x0A) & 0x80 != 0
}

/// (second, minute, hour, day, month, year in two digits, century register).
fn raw_time() -> [u8; 7] {
    [
        cmos(0x00),
        cmos(0x02),
        cmos(0x04),
        cmos(0x07),
        cmos(0x08),
        cmos(0x09),
        cmos(0x32),
    ]
}

/// Days from 1970-01-01 to `y-m-d` (proleptic Gregorian).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads the RTC (twice until two readings agree, outside an update).
pub fn init() {
    let mut last = [0u8; 7];
    let mut stable = None;
    for _ in 0..10 {
        while update_in_progress() {
            core::hint::spin_loop();
        }
        let t = raw_time();
        if t == last {
            stable = Some(t);
            break;
        }
        last = t;
    }
    let Some(t) = stable else {
        warn!("rtc: unstable readings, no wall clock");
        return;
    };
    let status_b = cmos(0x0B);
    let bcd = status_b & 0x04 == 0;
    let conv = |v: u8| if bcd { (v & 0x0F) + (v >> 4) * 10 } else { v };
    let sec = conv(t[0]) as i64;
    let min = conv(t[1]) as i64;
    let mut hour = conv(t[2] & 0x7F) as i64;
    if status_b & 0x02 == 0 && t[2] & 0x80 != 0 {
        hour = (hour % 12) + 12; // 12-hour mode, PM
    }
    let day = conv(t[3]) as u32;
    let month = conv(t[4]) as u32;
    let century = match conv(t[6]) {
        19..=21 => conv(t[6]) as i64,
        _ => 20,
    };
    let year = century * 100 + conv(t[5]) as i64;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || min > 59
        || sec > 60
        || year < 2024
    {
        warn!("rtc: implausible date {year}-{month:02}-{day:02} {hour:02}:{min:02}, no wall clock");
        return;
    }
    let unix = days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec;
    let boot = (unix as u64).saturating_sub(crate::time::uptime_ms() / 1000);
    BOOT_UNIX.store(boot, Ordering::Relaxed);
    info!("rtc: {year}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} UTC (unix {unix})");
}

/// Unix seconds now, or 0 if the RTC could not be read.
pub fn unix_now() -> u64 {
    match BOOT_UNIX.load(Ordering::Relaxed) {
        0 => 0,
        boot => boot + crate::time::uptime_ms() / 1000,
    }
}
