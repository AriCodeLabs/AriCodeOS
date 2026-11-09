// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The temperature of the CPU, for `diag` and for watching heavy runs
//! (`aricode_x86_cpu::thermal`). Intel CPUs only so far: AMD reads it elsewhere.

use super::msr;
use aricode_x86_cpu::thermal::*;
use core::arch::x86_64::__cpuid;

/// Degrees now and the limit at which the CPU throttles itself, from the package's
/// sensor if it has one, else this core's. `None` if this CPU has none we can read,
/// as under most hypervisors.
pub fn read() -> Option<(u32, u32)> {
    let vendor = __cpuid(0);
    let intel = (vendor.ebx, vendor.edx, vendor.ecx) == (0x756E_6547, 0x4965_6E69, 0x6C65_746E);
    if !intel || vendor.eax < 6 {
        return None;
    }
    let sensors = __cpuid(6).eax;
    if sensors & CPUID_SENSOR == 0 {
        return None;
    }
    let id = __cpuid(1).eax;
    let family = (id >> 8) & 0xF;
    let model = ((id >> 4) & 0xF) | ((id >> 16) & 0xF) << 4;
    let limit = if has_target(family, model) {
        limit(msr::read(IA32_TEMPERATURE_TARGET))
    } else {
        DEFAULT_LIMIT
    };
    let register = if sensors & CPUID_PACKAGE_SENSOR != 0 {
        IA32_PACKAGE_THERM_STATUS
    } else {
        IA32_THERM_STATUS
    };
    Some((celsius(msr::read(register), limit)?, limit))
}

/// How often the watch reads the sensor (ms).
const WATCH_MS: u64 = 2000;
/// A reading this far below the limit is worth an event, once per climb.
const WARN_MARGIN: u32 = 10;
/// A reading this far below the limit powers the machine off, unless the command
/// line says otherwise (`thermal_off=`).
const OFF_MARGIN: u32 = 3;

static OFF_AT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Starts the watch, the system's own: what runs on the machine cannot delay it, as
/// it can a reading from outside. `off_at` is the degrees that power it off, from
/// the command line; `None` for the default; 0 turns that off, leaving the CPU's own
/// throttling and the events. Without a sensor there is nothing to watch.
/// The temperature at which the watch powers the machine off, 0 for never
/// or before `start_watch`.
pub fn off_at() -> u32 {
    OFF_AT.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn start_watch(off_at: Option<u32>) {
    let Some((_, limit)) = read() else {
        return;
    };
    let off = off_at.unwrap_or(limit.saturating_sub(OFF_MARGIN));
    OFF_AT.store(off, core::sync::atomic::Ordering::Relaxed);
    log::info!("thermal: limit {limit} C, watch powers off at {off} C (0: never)");
    if let Err(e) = crate::task::scheduler::spawn("thermal", watch_thread, 0) {
        log::warn!("thermal: no watch ({e})");
    }
}

extern "C" fn watch_thread(_arg: usize) {
    let mut warned = false;
    let off = OFF_AT.load(core::sync::atomic::Ordering::Relaxed);
    loop {
        crate::task::scheduler::sleep_ms(WATCH_MS);
        let Some((degrees, limit)) = read() else {
            continue;
        };
        let warm = degrees + WARN_MARGIN >= limit;
        if warm && !warned {
            crate::events::record(
                aricode_abi::sysinfo::EventKind::Fault,
                0,
                degrees as u64,
                "cpu hot, degrees",
            );
            log::warn!("thermal: {degrees} C, limit {limit} C");
        }
        // Falling well below, a later climb says so again.
        if !warm && degrees + 2 * WARN_MARGIN < limit {
            warned = false;
        } else if warm {
            warned = true;
        }
        if off != 0 && degrees >= off {
            log::error!("thermal: {degrees} C reached {off} C: powering off");
            crate::shutdown();
        }
    }
}
