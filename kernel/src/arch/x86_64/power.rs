// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Machine reboot and ACPI power-off (Phase 21, `system_power`).

use super::port::{inb, inw, outb, outw};

/// Reboots through the 8042 controller (reset pulse) and, if that has no
/// effect, forces a triple fault. Never returns.
pub fn reboot() -> ! {
    // SAFETY: 8042 ports; standard PC reset sequence.
    unsafe {
        for _ in 0..100_000 {
            if inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
    }
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
    // Triple fault: empty IDT and an interrupt.
    let idt: [u16; 5] = [0; 5];
    // SAFETY: loading an IDT with limit 0 and raising an interrupt resets
    // the CPU; there is no way back and that is the intent.
    unsafe {
        core::arch::asm!("lidt [{}]", "int3", in(reg) &idt, options(nostack));
    }
    crate::halt_loop()
}

/// ACPI power-off: switches the firmware to ACPI mode if needed and writes
/// `SLP_TYPx | SLP_EN` to the PM1 control blocks (sleep state S5). Returns
/// only if the machine is still running afterwards (no FADT or `\_S5_`, or
/// firmware that ignores the request).
pub fn acpi_poweroff() {
    let Some((fadt, (typ_a, typ_b))) = crate::acpi::with(|a| a.fadt.zip(a.s5)) else {
        return;
    };
    if fadt.pm1a_cnt == 0 || fadt.pm1a_cnt > u16::MAX as u32 {
        return;
    }
    const SCI_EN: u16 = 1;
    const SLP_EN: u16 = 1 << 13;
    let pm1a = fadt.pm1a_cnt as u16;
    // SAFETY: ports declared by the firmware in the FADT for exactly this.
    unsafe {
        if inw(pm1a) & SCI_EN == 0 && fadt.smi_cmd != 0 && fadt.acpi_enable != 0 {
            outb(fadt.smi_cmd as u16, fadt.acpi_enable);
            for _ in 0..3_000_000 {
                if inw(pm1a) & SCI_EN != 0 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        if fadt.pm1b_cnt != 0 && fadt.pm1b_cnt <= u16::MAX as u32 {
            let pm1b = fadt.pm1b_cnt as u16;
            outw(
                pm1b,
                (inw(pm1b) & !(7 << 10)) | ((typ_b as u16) << 10) | SLP_EN,
            );
        }
        outw(
            pm1a,
            (inw(pm1a) & !(7 << 10)) | ((typ_a as u16) << 10) | SLP_EN,
        );
    }
    for _ in 0..10_000_000 {
        core::hint::spin_loop();
    }
}
