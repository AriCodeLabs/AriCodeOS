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
