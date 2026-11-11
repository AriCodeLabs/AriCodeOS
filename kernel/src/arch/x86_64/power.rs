// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Machine reboot and ACPI power-off (Phase 21, `system_power`).

use super::port::{inb, inl, inw, outb, outw};
use alloc::string::String;
use aricode_acpi::sleep::{self, Params, PortIo, Refusal, Report};
use aricode_acpi::Fadt;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

/// The power-off's policy, set once from the command line at boot.
struct Policy {
    /// The CPU the power-off runs on (`poweroff.cpu`, 0 the boot CPU).
    cpu: AtomicUsize,
    /// How long the machine is given after each `SLP_EN` write
    /// (`poweroff.wait_ms`).
    wait_ms: AtomicU64,
    /// Tests only (`poweroff_from`): the CPU the call is moved to first, so
    /// a call from another processor is exercised.
    from: AtomicUsize,
}

/// Default wait after each `SLP_EN` write. ACPICA waits ten seconds before
/// its retry "to allow chipsets that poll this reg very slowly"; three are
/// given here, twice, so a photo of the last line does not take half a
/// minute (`poweroff.wait_ms=10000` matches ACPICA).
pub const DEFAULT_WAIT_MS: u64 = 3000;
const NO_CPU: usize = usize::MAX;

static POLICY: Policy = Policy {
    cpu: AtomicUsize::new(0),
    wait_ms: AtomicU64::new(DEFAULT_WAIT_MS),
    from: AtomicUsize::new(NO_CPU),
};

/// Sets the policy (`poweroff.cpu`, `poweroff.wait_ms`, `poweroff_from`).
pub fn configure(cpu: Option<usize>, wait_ms: Option<u64>, from: Option<usize>) {
    if let Some(c) = cpu {
        POLICY.cpu.store(c, Ordering::Relaxed);
    }
    if let Some(w) = wait_ms {
        POLICY.wait_ms.store(w.min(60_000), Ordering::Relaxed);
    }
    if let Some(f) = from {
        POLICY.from.store(f, Ordering::Relaxed);
    }
}

/// Moves the calling thread to the CPU the power-off runs on, as Linux's
/// `migrate_to_reboot_cpu` and FreeBSD's `kern_reboot` (bound to
/// `CPU_FIRST()`: "Some systems don't shutdown properly (i.e., ACPI power
/// off) if we run on another processor") do. Interrupts must be on: the
/// scheduler does the move. Says where it was called and where it runs.
pub fn move_to_poweroff_cpu() {
    let from = POLICY.from.load(Ordering::Relaxed);
    if from != NO_CPU {
        match crate::task::scheduler::move_current_to(from) {
            Ok(()) => crate::println!("AriCodeOS: poweroff_from: the call moved to CPU {from}"),
            Err(e) => crate::println!("AriCodeOS: poweroff_from: CPU {from} {e}"),
        }
    }
    let called = crate::cpu::id();
    let want = POLICY.cpu.load(Ordering::Relaxed);
    let online = want < crate::cpu::MAX_CPUS && crate::cpu::slot(want).is_online();
    let target = if online { want } else { 0 };
    match crate::task::scheduler::move_current_to(target) {
        Ok(()) => crate::println!(
            "AriCodeOS: the power-off runs on CPU {} (called on CPU {called})",
            crate::cpu::id()
        ),
        Err(e) => {
            crate::println!("AriCodeOS: the power-off stays on CPU {called}: CPU {target} {e}")
        }
    }
}

/// What [`acpi_poweroff`] did, for the line said when the machine is
/// still running afterwards (incident 0065: a report must tell a write the
/// firmware ignored from one never made).
#[derive(Clone, Copy, Debug)]
pub enum PowerOff {
    /// No valid FADT.
    NoFadt,
    /// No `\_S5_` package found in the DSDT or SSDTs.
    NoS5,
    /// The FADT's registers cannot be used: nothing was written.
    Refused(Refusal),
    /// The sequence ran and the machine is still on.
    Ignored(Report),
    /// `poweroff_ignored`: the sequence ran without the `SLP_EN` writes.
    Skipped(Report),
}

/// Port I/O for the sequence in `aricode_acpi::sleep`.
struct Ports;

impl PortIo for Ports {
    fn read8(&mut self, port: u16) -> u8 {
        // SAFETY: ports the FADT declares for the fixed hardware, checked
        // to lie within the I/O space by `aricode_acpi::sleep`.
        unsafe { inb(port) }
    }
    fn read16(&mut self, port: u16) -> u16 {
        // SAFETY: as `read8`.
        unsafe { inw(port) }
    }
    fn write8(&mut self, port: u16, value: u8) {
        // SAFETY: as `read8`; the values are the ones ACPI defines.
        unsafe { outb(port, value) }
    }
    fn write16(&mut self, port: u16, value: u16) {
        // SAFETY: as `write8`.
        unsafe { outw(port, value) }
    }
    fn delay_us(&mut self, us: u64) {
        crate::time::spin_us(us)
    }
}

/// What the power-off needs, gathered while interrupts are still on and
/// the other CPUs run: after they are halted no lock they might hold (the
/// ACPI tables', the PCI list's, the heap's) is taken again.
pub struct Prepared {
    fadt: Option<Fadt>,
    s5: Option<(u8, u8)>,
    /// `SMI on SLP_EN ...`, for the last line.
    pub smi: String,
}

pub fn prepare() -> Prepared {
    let (fadt, s5) = crate::acpi::with(|a| (a.fadt, a.s5));
    let smi = fadt.map(|f| smi_on_sleep_text(&f)).unwrap_or_default();
    Prepared { fadt, s5, smi }
}

/// ACPI power-off: the S5 sequence of `aricode_acpi::sleep::enter_s5`
/// (ACPI mode, wake status and GPEs cleared, `SLP_TYP` then `SLP_TYP |
/// SLP_EN`, PM1a before PM1b, or the hardware-reduced sleep control
/// register). With interrupts off, on the power-off CPU, the others halted.
/// Returns only if the machine is still running afterwards, saying why.
/// `enter` false skips the `SLP_EN` writes (`poweroff_ignored`).
pub fn acpi_poweroff(p: &Prepared, enter: bool) -> PowerOff {
    let Some(fadt) = p.fadt else {
        return PowerOff::NoFadt;
    };
    let Some(typ) = p.s5 else {
        return PowerOff::NoS5;
    };
    let params = Params {
        acpi_enable_ms: sleep::ACPI_ENABLE_WAIT_MS,
        wait_ms: POLICY.wait_ms.load(Ordering::Relaxed),
        enter,
    };
    match sleep::enter_s5(&fadt, typ, &params, &mut Ports) {
        Err(r) => PowerOff::Refused(r),
        Ok(r) if enter => PowerOff::Ignored(r),
        Ok(r) => PowerOff::Skipped(r),
    }
}

/// Chipsets whose `SMI_EN` register is known from their manual: the
/// LPC/eSPI bridge's ID at 00:1f.0, and the manual. `SMI_EN` is at the
/// ACPI base (the FADT's PM1a event block) plus 0x30; its bit 4,
/// `SMI_ON_SLP_EN`, makes a `SLP_EN` write raise an SMI instead of the
/// sleep, for the firmware's handler to finish (Intel 500 Series On-Package
/// PCH datasheet vol. 2, 631120-002, 4.2.5). Other chipsets are not read.
const SMI_EN_KNOWN: &[(u16, u16, &str)] = &[(0x8086, 0xA082, "Intel 500 Series PCH")];
const SMI_EN_OFFSET: u16 = 0x30;
const SMI_ON_SLP_EN: u32 = 1 << 4;
const GBL_SMI_EN: u32 = 1 << 0;

/// `SMI_ON_SLP_EN` and `GBL_SMI_EN` on a chipset of [`SMI_EN_KNOWN`], and
/// its name; `None` elsewhere. Read only.
fn smi_on_sleep(fadt: &Fadt) -> Option<(bool, bool, &'static str)> {
    let id = crate::pci::id_at(0, 0x1F, 0)?;
    let (_, _, name) = SMI_EN_KNOWN.iter().find(|(v, d, _)| (*v, *d) == id)?;
    let base = fadt.pm1a_evt.io_ports(4, true).ok()?;
    // The register's four bytes within the I/O space.
    let port = base
        .checked_add(SMI_EN_OFFSET)
        .filter(|p| p.checked_add(3).is_some())?;
    // SAFETY: a plain read/write register of that chipset's ACPI I/O
    // space (datasheet 4.2.5): reading it has no side effect.
    let v = unsafe { inl(port) };
    Some((v & SMI_ON_SLP_EN != 0, v & GBL_SMI_EN != 0, name))
}

/// `SMI on SLP_EN 1` or `SMI on SLP_EN not read (unknown chipset)`, for the
/// last line and `diag`.
pub fn smi_on_sleep_text(fadt: &Fadt) -> String {
    match smi_on_sleep(fadt) {
        Some((slp, gbl, name)) => alloc::format!(
            "SMI on SLP_EN {} (GBL_SMI_EN {}, {name})",
            slp as u8,
            gbl as u8
        ),
        None => String::from("SMI on SLP_EN not read (chipset not in the table)"),
    }
}

/// The FADT's power facts and the registers' state now, as `diag`'s
/// `power` lines: what a report from a machine without serial needs to
/// tell why a power-off did nothing. Read only.
pub fn report(out: &mut String) {
    let (fadt, s5) = crate::acpi::with(|a| (a.fadt, a.s5));
    let Some(f) = fadt else {
        let _ = writeln!(out, "power   no FADT: shutdown only halts");
        return;
    };
    let _ = writeln!(
        out,
        "power   FADT rev {}.{} ({} bytes), flags {:#x}: hardware-reduced {}, low-power S0 {}; SCI IRQ {}; SMI_CMD {:#x} ACPI_ENABLE {:#x}",
        f.revision,
        f.minor_revision,
        f.length,
        f.flags,
        f.hw_reduced() as u8,
        (f.flags & aricode_acpi::fadt::FLAG_LOW_POWER_S0 != 0) as u8,
        f.sci_interrupt,
        f.smi_cmd,
        f.acpi_enable
    );
    match s5 {
        Some((a, b)) => {
            let _ = write!(out, "power   S5 SLP_TYP {a}/{b}");
        }
        None => {
            let _ = write!(out, "power   no \\_S5_ in the DSDT or SSDTs");
        }
    }
    if f.hw_reduced() {
        let _ = writeln!(
            out,
            "; sleep control {:#x} (space {}), status {:#x} (space {})",
            f.sleep_control.address,
            f.sleep_control.space,
            f.sleep_status.address,
            f.sleep_status.space
        );
        return;
    }
    let _ = writeln!(
        out,
        "; PM1a evt {}, cnt {}; PM1b evt {}, cnt {}; GPE0 {}, GPE1 {} (base {})",
        f.pm1a_evt, f.pm1a_cnt, f.pm1b_evt, f.pm1b_cnt, f.gpe0, f.gpe1, f.gpe1_base
    );
    let _ = write!(out, "power   now:");
    match f.pm1a_cnt.io_ports(2, false) {
        Ok(p) => {
            // SAFETY: the FADT's PM1a control port, read only.
            let v = unsafe { inw(p) };
            let _ = write!(
                out,
                " PM1a control {v:#06x} (SCI_EN {})",
                v & sleep::PM1_SCI_EN
            );
        }
        Err(e) => {
            let _ = write!(out, " PM1a control {e}");
        }
    }
    if let Ok(p) = f.pm1a_evt.io_ports(4, true) {
        let half = (f.pm1a_evt.len / 2) as u16;
        // SAFETY: the FADT's PM1a status and enable ports, read only
        // (status bits clear on a write of one, not on a read).
        let (sts, en) = unsafe { (inw(p), inw(p + half)) };
        let _ = write!(out, ", PM1 status {sts:#06x} enable {en:#06x}");
    }
    for (name, g) in [("GPE0", &f.gpe0), ("GPE1", &f.gpe1)] {
        let Ok(p) = g.io_ports(2, true) else {
            continue;
        };
        let half = (g.len / 2) as u16;
        let (mut enabled, mut pending) = (0, 0);
        for i in 0..half {
            // SAFETY: the FADT's GPE status and enable bytes, read only.
            let (sts, en) = unsafe { (inb(p + i), inb(p + half + i)) };
            enabled += en.count_ones();
            pending += (sts & en).count_ones();
        }
        let _ = write!(
            out,
            ", {name} {enabled} of {} enabled, {pending} of those set",
            half * 8
        );
    }
    let _ = writeln!(out, "; {}", smi_on_sleep_text(&f));
}
