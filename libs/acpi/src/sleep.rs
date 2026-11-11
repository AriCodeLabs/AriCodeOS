// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Entering S5 (soft off) through the FADT's fixed hardware, without an AML
//! interpreter: the order of ACPICA's `acpi_hw_legacy_sleep` and
//! `acpi_hw_extended_sleep` (studied, not copied: audit
//! `2026-11-11-acpi-power-off-study`), checked against ACPI 6.5 section 4.8
//! (fixed hardware registers) and 16.1 (sleeping states).
//!
//! Legacy hardware, in order:
//!
//! 1. ACPI mode: if `SCI_EN` is clear, `ACPI_ENABLE` goes to `SMI_CMD` and
//!    `SCI_EN` is awaited (ACPICA waits up to 3 s: 30 000 times 100 us).
//! 2. `WAK_STS` cleared (write one, 4.8.3.1.1).
//! 3. Every GPE disabled, then every fixed and GPE status bit cleared (no
//!    wake event pending, none enabled: an AML-less kernel has no `_PRW`
//!    wake devices to keep).
//! 4. `SLP_TYPx` written alone, then `SLP_TYPx | SLP_EN`, PM1a before PM1b
//!    each time: ACPICA splits the two writes "to workaround poorly
//!    implemented hardware".
//! 5. If the machine is still on after the wait, `SLP_TYPx | SLP_EN` once
//!    more (ACPICA retries after ten seconds), then the caller reports.
//!
//! Hardware-reduced platforms (ACPI 5.0, 4.8.3.7): `WAK_STS` cleared in the
//! sleep status register, then `SLP_TYPa | SLP_EN` in the sleep control
//! register.
//!
//! Nothing is written before every block the sequence needs has been
//! checked: a refusal leaves the hardware as it was.

use crate::fadt::{BlockError, Fadt, Gas, SPACE_IO};
use core::fmt;

/// PM1 status bits (ACPI 6.5, table 4.16), write one to clear.
pub const PM1_TMR_STS: u16 = 1 << 0;
pub const PM1_BM_STS: u16 = 1 << 4;
pub const PM1_GBL_STS: u16 = 1 << 5;
pub const PM1_PWRBTN_STS: u16 = 1 << 8;
pub const PM1_SLPBTN_STS: u16 = 1 << 9;
pub const PM1_RTC_STS: u16 = 1 << 10;
pub const PM1_PCIEXP_WAKE_STS: u16 = 1 << 14;
pub const PM1_WAK_STS: u16 = 1 << 15;
/// Every fixed status bit the specification defines (bit 11 is ignored
/// and not written).
pub const PM1_ALL_FIXED_STATUS: u16 = PM1_TMR_STS
    | PM1_BM_STS
    | PM1_GBL_STS
    | PM1_PWRBTN_STS
    | PM1_SLPBTN_STS
    | PM1_RTC_STS
    | PM1_PCIEXP_WAKE_STS
    | PM1_WAK_STS;

/// PM1 control bits (ACPI 6.5, table 4.18).
pub const PM1_SCI_EN: u16 = 1 << 0;
/// Write-only: reads are masked out, never written back.
pub const PM1_GBL_RLS: u16 = 1 << 2;
pub const PM1_SLP_TYP_SHIFT: u16 = 10;
pub const PM1_SLP_TYP: u16 = 7 << PM1_SLP_TYP_SHIFT;
/// Write-only.
pub const PM1_SLP_EN: u16 = 1 << 13;

/// Sleep control and status registers (ACPI 6.5, 4.8.3.7).
pub const SLEEP_TYP_SHIFT: u8 = 2;
pub const SLEEP_TYP: u8 = 7 << SLEEP_TYP_SHIFT;
pub const SLEEP_EN: u8 = 1 << 5;
pub const SLEEP_WAK_STS: u8 = 1 << 7;

/// ACPICA's wait for `SCI_EN` after `ACPI_ENABLE`: 3 s.
pub const ACPI_ENABLE_WAIT_MS: u64 = 3000;
/// How often `SCI_EN` is polled meanwhile.
const ACPI_ENABLE_POLL_US: u64 = 100;

/// Port I/O and a delay, as the kernel gives them (a fake register bank in
/// the host tests).
pub trait PortIo {
    fn read8(&mut self, port: u16) -> u8;
    fn read16(&mut self, port: u16) -> u16;
    fn write8(&mut self, port: u16, value: u8);
    fn write16(&mut self, port: u16, value: u16);
    /// Waits at least `us` microseconds.
    fn delay_us(&mut self, us: u64);
}

/// The policy: what a caller may tune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// How long `SCI_EN` is awaited after `ACPI_ENABLE`.
    pub acpi_enable_ms: u64,
    /// How long the machine is given to go off after each `SLP_EN` write.
    pub wait_ms: u64,
    /// `false`: everything but the `SLP_EN` writes (tests: firmware that
    /// ignores the power-off).
    pub enter: bool,
}

/// Why nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The PM1a control block cannot be used.
    Pm1aControl(BlockError),
    /// A PM1b control block is declared but cannot be used: writing only
    /// half of the pair is not the sequence the firmware expects.
    Pm1bControl(BlockError),
    /// The PM1a control block reads all ones: nothing answers there.
    Pm1aFloating(u16),
    /// Hardware-reduced: the sleep control or status register cannot be used.
    SleepControl(BlockError),
    SleepStatus(BlockError),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Refusal::Pm1aControl(e) => write!(f, "PM1a control block {e}"),
            Refusal::Pm1bControl(e) => write!(f, "PM1b control block {e}"),
            Refusal::Pm1aFloating(p) => {
                write!(f, "PM1a control block {p:#x} reads 0xffff, nothing answers")
            }
            Refusal::SleepControl(e) => {
                write!(f, "hardware-reduced, sleep control register {e}")
            }
            Refusal::SleepStatus(e) => write!(f, "hardware-reduced, sleep status register {e}"),
        }
    }
}

/// How the sleep state was entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// PM1 control blocks.
    Legacy,
    /// The ACPI 5.0 sleep control register.
    SleepControl,
}

/// What an optional block's step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The FADT declares no such block.
    Absent,
    /// Done on this many bytes of the block.
    Done(u8),
    /// Declared but unusable: skipped.
    Skipped(BlockError),
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Step::Absent => write!(f, "none"),
            Step::Done(n) => write!(f, "{n} bytes"),
            Step::Skipped(e) => write!(f, "skipped ({e})"),
        }
    }
}

/// What was done, said when the machine is still on afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub method: Method,
    pub typ_a: u8,
    pub typ_b: u8,
    /// The control register written (PM1a or the sleep control register),
    /// and PM1b if there is one.
    pub control_a: u16,
    pub control_b: Option<u16>,
    /// The value written with `SLP_EN` to the first.
    pub value_a: u16,
    /// `SCI_EN` before the sequence, and before the write (after
    /// `ACPI_ENABLE`, if it was needed and possible).
    pub sci_en_before: bool,
    pub sci_en: bool,
    pub acpi_enable_written: bool,
    /// PM1 status cleared through the event blocks (`WAK_STS` first).
    pub pm1_status: Step,
    /// GPE0 and GPE1: enables cleared, then status.
    pub gpe: [Step; 2],
    /// `SLP_EN` writes made: 0 when `Params::enter` is off, otherwise 2
    /// (the second after the first had no effect).
    pub slp_en_writes: u8,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.method {
            Method::Legacy => {
                write!(
                    f,
                    "S5 type {}/{} to PM1a {:#x} ({:#06x} with SLP_EN), PM1b ",
                    self.typ_a, self.typ_b, self.control_a, self.value_a
                )?;
                match self.control_b {
                    Some(b) => write!(f, "{b:#x}")?,
                    None => write!(f, "none")?,
                }
                write!(
                    f,
                    "; SCI_EN {} (was {}{}); PM1 status {}; GPE0 {}, GPE1 {}",
                    self.sci_en as u8,
                    self.sci_en_before as u8,
                    if self.acpi_enable_written {
                        ", ACPI_ENABLE written"
                    } else {
                        ""
                    },
                    self.pm1_status,
                    self.gpe[0],
                    self.gpe[1]
                )?;
            }
            Method::SleepControl => write!(
                f,
                "hardware-reduced, S5 type {} to sleep control {:#x} ({:#04x})",
                self.typ_a, self.control_a, self.value_a
            )?,
        }
        write!(f, "; SLP_EN written {} times", self.slp_en_writes)
    }
}

/// A sleep register (GAS) as one I/O port.
fn gas_port(g: &Gas) -> Result<u16, BlockError> {
    if !g.is_present() {
        return Err(BlockError::Absent);
    }
    if g.space != SPACE_IO {
        return Err(BlockError::NotIo(g.space));
    }
    if g.address >= 0x1_0000 {
        return Err(BlockError::Beyond(g.address));
    }
    Ok(g.address as u16)
}

/// A PM1 event or GPE block: its status half and its enable half.
#[derive(Clone, Copy)]
struct Halves {
    status: u16,
    enable: u16,
    half: u8,
}

fn halves(block: &crate::fadt::Block, min_len: u8) -> Result<Option<Halves>, BlockError> {
    if !block.is_present() {
        return Ok(None);
    }
    let port = block.io_ports(min_len, true)?;
    let half = block.len / 2;
    Ok(Some(Halves {
        status: port,
        enable: port + half as u16,
        half,
    }))
}

fn step(r: &Result<Option<Halves>, BlockError>) -> Step {
    match r {
        Ok(None) => Step::Absent,
        Ok(Some(h)) => Step::Done(h.half * 2),
        Err(e) => Step::Skipped(*e),
    }
}

/// Puts the machine in S5 through the FADT's registers, with `\_S5_`'s
/// `typ` values. Returns only if the machine is still on: a refusal (nothing
/// written) or what was done.
pub fn enter_s5<T: PortIo>(
    fadt: &Fadt,
    typ: (u8, u8),
    params: &Params,
    io: &mut T,
) -> Result<Report, Refusal> {
    let (typ_a, typ_b) = (typ.0 & 7, typ.1 & 7);
    if fadt.hw_reduced() {
        return enter_s5_reduced(fadt, typ_a, typ_b, params, io);
    }
    // Every check first.
    let a_cnt = fadt
        .pm1a_cnt
        .io_ports(2, false)
        .map_err(Refusal::Pm1aControl)?;
    let b_cnt = match fadt.pm1b_cnt.is_present() {
        true => Some(
            fadt.pm1b_cnt
                .io_ports(2, false)
                .map_err(Refusal::Pm1bControl)?,
        ),
        false => None,
    };
    let a_now = io.read16(a_cnt);
    if a_now == 0xFFFF {
        return Err(Refusal::Pm1aFloating(a_cnt));
    }
    let evt = [halves(&fadt.pm1a_evt, 4), halves(&fadt.pm1b_evt, 4)];
    let pm1_status = step(&evt[0]);
    let gpe = [halves(&fadt.gpe0, 2), halves(&fadt.gpe1, 2)];

    // 1. ACPI mode.
    let sci_en_before = a_now & PM1_SCI_EN != 0;
    let mut acpi_enable_written = false;
    if !sci_en_before && fadt.smi_cmd != 0 && fadt.smi_cmd <= 0xFFFF && fadt.acpi_enable != 0 {
        io.write8(fadt.smi_cmd as u16, fadt.acpi_enable);
        acpi_enable_written = true;
        let polls = params.acpi_enable_ms * 1000 / ACPI_ENABLE_POLL_US;
        for _ in 0..polls {
            if io.read16(a_cnt) & PM1_SCI_EN != 0 {
                break;
            }
            io.delay_us(ACPI_ENABLE_POLL_US);
        }
    }
    let sci_en = io.read16(a_cnt) & PM1_SCI_EN != 0;

    // 2. WAK_STS, then 3. GPEs off, every status bit cleared.
    // PM1b's event block is written only along with PM1a's.
    let mut pm1_ports = [None, None];
    if let Ok(Some(a)) = evt[0] {
        pm1_ports[0] = Some(a.status);
        if let Ok(Some(b)) = evt[1] {
            pm1_ports[1] = Some(b.status);
        }
    }
    for port in pm1_ports.iter().flatten() {
        io.write16(*port, PM1_WAK_STS);
    }
    for h in gpe.iter().flatten().flatten() {
        for i in 0..h.half as u16 {
            io.write8(h.enable + i, 0);
        }
    }
    for port in pm1_ports.iter().flatten() {
        io.write16(*port, PM1_ALL_FIXED_STATUS);
    }
    for h in gpe.iter().flatten().flatten() {
        for i in 0..h.half as u16 {
            io.write8(h.status + i, 0xFF);
        }
    }

    // 4. SLP_TYP alone, then with SLP_EN; PM1a before PM1b.
    let mut control = io.read16(a_cnt);
    if let Some(b) = b_cnt {
        control |= io.read16(b);
    }
    control &= !(PM1_SLP_TYP | PM1_SLP_EN | PM1_GBL_RLS);
    let value_a = control | (typ_a as u16) << PM1_SLP_TYP_SHIFT;
    let value_b = control | (typ_b as u16) << PM1_SLP_TYP_SHIFT;
    let write = |io: &mut T, extra: u16| {
        io.write16(a_cnt, value_a | extra);
        if let Some(b) = b_cnt {
            io.write16(b, value_b | extra);
        }
    };
    write(io, 0);
    let mut slp_en_writes = 0;
    if params.enter {
        // 5. One write and, if the machine is still on, one more.
        for _ in 0..2 {
            write(io, PM1_SLP_EN);
            slp_en_writes += 1;
            io.delay_us(params.wait_ms * 1000);
        }
    }
    Ok(Report {
        method: Method::Legacy,
        typ_a,
        typ_b,
        control_a: a_cnt,
        control_b: b_cnt,
        value_a: value_a | PM1_SLP_EN,
        sci_en_before,
        sci_en,
        acpi_enable_written,
        pm1_status,
        gpe: [step(&gpe[0]), step(&gpe[1])],
        slp_en_writes,
    })
}

/// The sequence for hardware-reduced platforms (ACPI 5.0, 4.8.3.7; ACPICA
/// `acpi_hw_extended_sleep`): only `SLP_TYPa` is used.
fn enter_s5_reduced(
    fadt: &Fadt,
    typ_a: u8,
    typ_b: u8,
    params: &Params,
    io: &mut impl PortIo,
) -> Result<Report, Refusal> {
    let control = gas_port(&fadt.sleep_control).map_err(Refusal::SleepControl)?;
    let status = gas_port(&fadt.sleep_status).map_err(Refusal::SleepStatus)?;
    io.write8(status, SLEEP_WAK_STS);
    let value = (typ_a << SLEEP_TYP_SHIFT) & SLEEP_TYP | SLEEP_EN;
    let mut slp_en_writes = 0;
    if params.enter {
        for _ in 0..2 {
            io.write8(control, value);
            slp_en_writes += 1;
            io.delay_us(params.wait_ms * 1000);
        }
    }
    Ok(Report {
        method: Method::SleepControl,
        typ_a,
        typ_b,
        control_a: control,
        control_b: None,
        value_a: value as u16,
        sci_en_before: false,
        sci_en: false,
        acpi_enable_written: false,
        pm1_status: Step::Absent,
        gpe: [Step::Absent, Step::Absent],
        slp_en_writes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fadt::{Block, Source, FLAG_HW_REDUCED, SPACE_MEMORY};
    use alloc::vec;
    use alloc::vec::Vec;
    use alloc::{format, string::String};

    /// One access to the fake bank.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        W8(u16, u8),
        W16(u16, u16),
        Delay(u64),
    }

    /// A register bank built from ACPI 6.5's fixed hardware and the Intel
    /// 500 Series On-Package PCH (631120-002, 4.2): PM1 status/enable at
    /// 0x1800, PM1 control at 0x1804 (SCI_EN read/write, SLP_EN reads 0),
    /// GPE0 status at 0x1860 and enables at 0x1870, SMI_CMD at 0xB2. Writes
    /// are logged; `ACPI_ENABLE` sets SCI_EN after `sci_after` polls.
    struct Bank {
        ops: Vec<Op>,
        pm1_cnt: u16,
        pm1b_cnt: u16,
        floating: bool,
        sci_after: Option<u32>,
        polls: u32,
        enabled: bool,
    }

    impl Bank {
        fn new(pm1_cnt: u16) -> Self {
            Bank {
                ops: Vec::new(),
                pm1_cnt,
                pm1b_cnt: 0,
                floating: false,
                sci_after: None,
                polls: 0,
                enabled: false,
            }
        }
        fn writes(&self) -> Vec<Op> {
            self.ops.clone()
        }
    }

    impl PortIo for Bank {
        fn read8(&mut self, _: u16) -> u8 {
            0
        }
        fn read16(&mut self, port: u16) -> u16 {
            if self.floating {
                return 0xFFFF;
            }
            match port {
                0x1804 => {
                    if self.enabled {
                        self.polls += 1;
                        if Some(self.polls) >= self.sci_after {
                            self.pm1_cnt |= PM1_SCI_EN;
                        }
                    }
                    self.pm1_cnt & !(PM1_SLP_EN | PM1_GBL_RLS)
                }
                0x1808 => self.pm1b_cnt,
                _ => 0,
            }
        }
        fn write8(&mut self, port: u16, value: u8) {
            if port == 0xB2 && value == 0xA0 {
                self.enabled = self.sci_after.is_some();
            }
            self.ops.push(Op::W8(port, value));
        }
        fn write16(&mut self, port: u16, value: u16) {
            if port == 0x1804 {
                self.pm1_cnt = value;
            }
            self.ops.push(Op::W16(port, value));
        }
        fn delay_us(&mut self, us: u64) {
            // The polls in between are not logged: only the waits after
            // a write are of interest, and a wait follows each poll.
            if us != ACPI_ENABLE_POLL_US {
                self.ops.push(Op::Delay(us));
            }
        }
    }

    fn io(address: u64, len: u8) -> Block {
        Block {
            space: crate::fadt::SPACE_IO,
            address,
            len,
            source: Source::Extended,
            mismatch: false,
        }
    }

    /// The office Lenovo's layout (Tiger Lake PCH, ACPI base 0x1800 in its
    /// datasheet's register map), not its tables: PM1a only, GPE0 of 32
    /// bytes, SMI_CMD 0xB2.
    fn tiger_lake() -> Fadt {
        Fadt {
            smi_cmd: 0xB2,
            acpi_enable: 0xA0,
            pm1a_evt: io(0x1800, 4),
            pm1a_cnt: io(0x1804, 2),
            gpe0: io(0x1860, 32),
            ..Fadt::default()
        }
    }

    const PARAMS: Params = Params {
        acpi_enable_ms: ACPI_ENABLE_WAIT_MS,
        wait_ms: 3000,
        enter: true,
    };

    #[test]
    fn legacy_order_of_writes() {
        let mut bank = Bank::new(PM1_SCI_EN | 0x0200);
        let r = enter_s5(&tiger_lake(), (7, 0), &PARAMS, &mut bank).unwrap();
        let mut want = vec![Op::W16(0x1800, PM1_WAK_STS)];
        want.extend((0x1870..0x1880).map(|p| Op::W8(p, 0)));
        want.push(Op::W16(0x1800, PM1_ALL_FIXED_STATUS));
        want.extend((0x1860..0x1870).map(|p| Op::W8(p, 0xFF)));
        // SCI_EN and the ignored bit 9 kept; SLP_TYP alone, then with
        // SLP_EN, twice, with the wait after each.
        let typ = 0x0201 | 7 << 10;
        want.push(Op::W16(0x1804, typ));
        want.push(Op::W16(0x1804, typ | PM1_SLP_EN));
        want.push(Op::Delay(3_000_000));
        want.push(Op::W16(0x1804, typ | PM1_SLP_EN));
        want.push(Op::Delay(3_000_000));
        assert_eq!(bank.writes(), want);
        assert_eq!(r.slp_en_writes, 2);
        assert!(r.sci_en_before && r.sci_en && !r.acpi_enable_written);
        assert_eq!(r.pm1_status, Step::Done(4));
        assert_eq!(r.gpe, [Step::Done(32), Step::Absent]);
        assert_eq!(
            format!("{r}"),
            "S5 type 7/0 to PM1a 0x1804 (0x3e01 with SLP_EN), PM1b none; SCI_EN 1 (was 1); \
             PM1 status 4 bytes; GPE0 32 bytes, GPE1 none; SLP_EN written 2 times"
        );
    }

    /// PM1a is written before PM1b, both with their own SLP_TYP, and the
    /// value read is the OR of both control registers.
    #[test]
    fn pm1a_before_pm1b() {
        let mut f = tiger_lake();
        f.pm1b_cnt = io(0x1808, 2);
        f.pm1b_evt = io(0x1810, 4);
        f.gpe0 = Block::default();
        let mut bank = Bank::new(PM1_SCI_EN);
        bank.pm1b_cnt = 0x0200;
        let r = enter_s5(&f, (5, 3), &PARAMS, &mut bank).unwrap();
        let w = bank.writes();
        assert_eq!(
            &w[..2],
            &[Op::W16(0x1800, PM1_WAK_STS), Op::W16(0x1810, PM1_WAK_STS)]
        );
        let base = 0x0201;
        assert_eq!(
            &w[4..8],
            &[
                Op::W16(0x1804, base | 5 << 10),
                Op::W16(0x1808, base | 3 << 10),
                Op::W16(0x1804, base | 5 << 10 | PM1_SLP_EN),
                Op::W16(0x1808, base | 3 << 10 | PM1_SLP_EN),
            ]
        );
        assert_eq!(r.control_b, Some(0x1808));
    }

    /// With SCI_EN clear, ACPI_ENABLE goes to SMI_CMD first and SCI_EN is
    /// awaited; if it never comes the wait is bounded and the write made.
    #[test]
    fn acpi_enable_when_sci_en_is_clear() {
        let mut bank = Bank::new(0);
        bank.sci_after = Some(5);
        let r = enter_s5(&tiger_lake(), (7, 0), &PARAMS, &mut bank).unwrap();
        assert_eq!(bank.writes()[0], Op::W8(0xB2, 0xA0));
        assert!(!r.sci_en_before && r.sci_en && r.acpi_enable_written);
        assert!(bank.polls >= 5);

        let mut bank = Bank::new(0);
        let r = enter_s5(&tiger_lake(), (7, 0), &PARAMS, &mut bank).unwrap();
        assert!(r.acpi_enable_written && !r.sci_en);
        assert!(bank
            .writes()
            .contains(&Op::W16(0x1804, 7 << 10 | PM1_SLP_EN)));

        // No SMI_CMD (ACPI-only firmware): nothing written there.
        let mut f = tiger_lake();
        f.smi_cmd = 0;
        let mut bank = Bank::new(0);
        let r = enter_s5(&f, (7, 0), &PARAMS, &mut bank).unwrap();
        assert!(!r.acpi_enable_written);
        assert!(!bank.writes().iter().any(|o| matches!(o, Op::W8(0xB2, _))));
    }

    /// `enter: false` (poweroff_ignored) does everything but SLP_EN.
    #[test]
    fn without_entering() {
        let mut bank = Bank::new(PM1_SCI_EN);
        let p = Params {
            enter: false,
            ..PARAMS
        };
        let r = enter_s5(&tiger_lake(), (7, 0), &p, &mut bank).unwrap();
        assert_eq!(r.slp_en_writes, 0);
        assert!(!bank
            .writes()
            .iter()
            .any(|o| matches!(o, Op::W16(0x1804, v) if v & PM1_SLP_EN != 0)));
        assert_eq!(
            bank.writes().last(),
            Some(&Op::W16(0x1804, 0x0001 | 7 << 10))
        );
    }

    /// Refusals write nothing at all.
    #[test]
    fn refusals_leave_the_hardware_alone() {
        let cases: Vec<(Fadt, Refusal, &str)> = vec![
            (
                Fadt {
                    pm1a_cnt: Block::default(),
                    ..tiger_lake()
                },
                Refusal::Pm1aControl(BlockError::Absent),
                "PM1a control block absent",
            ),
            (
                Fadt {
                    pm1a_cnt: Block {
                        space: SPACE_MEMORY,
                        ..io(0xFE00_1804, 2)
                    },
                    ..tiger_lake()
                },
                Refusal::Pm1aControl(BlockError::NotIo(SPACE_MEMORY)),
                "PM1a control block in address space 0, not I/O",
            ),
            (
                Fadt {
                    pm1b_cnt: io(0xFFFF, 2),
                    ..tiger_lake()
                },
                Refusal::Pm1bControl(BlockError::Beyond(0xFFFF)),
                "PM1b control block at 0xffff, past the I/O ports",
            ),
            (
                Fadt {
                    flags: FLAG_HW_REDUCED,
                    ..tiger_lake()
                },
                Refusal::SleepControl(BlockError::Absent),
                "hardware-reduced, sleep control register absent",
            ),
        ];
        for (f, want, text) in cases {
            let mut bank = Bank::new(PM1_SCI_EN);
            assert_eq!(enter_s5(&f, (7, 0), &PARAMS, &mut bank), Err(want));
            assert!(bank.writes().is_empty(), "{want:?}");
            assert_eq!(format!("{want}"), text);
        }
        let mut bank = Bank::new(0);
        bank.floating = true;
        assert_eq!(
            enter_s5(&tiger_lake(), (7, 0), &PARAMS, &mut bank),
            Err(Refusal::Pm1aFloating(0x1804))
        );
        assert!(bank.writes().is_empty());
    }

    /// Unusable optional blocks are skipped and said, not written.
    #[test]
    fn unusable_event_and_gpe_blocks_are_skipped() {
        let mut f = tiger_lake();
        f.gpe0 = io(0x1860, 33);
        f.pm1a_evt = Block {
            space: SPACE_MEMORY,
            ..io(0xFE00_0000, 4)
        };
        let mut bank = Bank::new(PM1_SCI_EN);
        let r = enter_s5(&f, (7, 0), &PARAMS, &mut bank).unwrap();
        assert_eq!(r.gpe[0], Step::Skipped(BlockError::Length(33)));
        assert_eq!(r.pm1_status, Step::Skipped(BlockError::NotIo(SPACE_MEMORY)));
        assert_eq!(bank.writes()[0], Op::W16(0x1804, 0x0001 | 7 << 10));
        let s: String = format!("{r}");
        assert!(s.contains("GPE0 skipped (length 33 not usable)"), "{s}");
    }

    /// Hardware-reduced: WAK_STS in the status register, then SLP_TYPa and
    /// SLP_EN in the control register; PM1 blocks are ignored.
    #[test]
    fn hardware_reduced_sleep_registers() {
        let mut f = tiger_lake();
        f.flags = FLAG_HW_REDUCED;
        f.sleep_control = Gas {
            space: SPACE_IO,
            bit_width: 8,
            bit_offset: 0,
            access_size: 1,
            address: 0x404,
        };
        f.sleep_status = Gas {
            address: 0x400,
            ..f.sleep_control
        };
        let mut bank = Bank::new(PM1_SCI_EN);
        let r = enter_s5(&f, (5, 0), &PARAMS, &mut bank).unwrap();
        let v = 5 << 2 | SLEEP_EN;
        assert_eq!(
            bank.writes(),
            vec![
                Op::W8(0x400, SLEEP_WAK_STS),
                Op::W8(0x404, v),
                Op::Delay(3_000_000),
                Op::W8(0x404, v),
                Op::Delay(3_000_000),
            ]
        );
        assert_eq!(r.method, Method::SleepControl);
        assert_eq!(
            format!("{r}"),
            "hardware-reduced, S5 type 5 to sleep control 0x404 (0x34); SLP_EN written 2 times"
        );
        // A sleep register in memory space is refused.
        f.sleep_status.space = SPACE_MEMORY;
        let mut bank = Bank::new(0);
        assert_eq!(
            enter_s5(&f, (5, 0), &PARAMS, &mut bank),
            Err(Refusal::SleepStatus(BlockError::NotIo(SPACE_MEMORY)))
        );
        assert!(bank.writes().is_empty());
    }
}
