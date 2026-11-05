// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The panel's own size on Intel graphics whose firmware does not offer it
//! (ADR 0079). Some firmware offers only small modes, yet drives the panel at
//! its native timing and scales a small image up with the panel fitter. Then
//! the native size needs no mode set: the pipe's source size can change on the
//! running pipe, and the fitter can step aside.
//!
//! Written from Intel's Programmer's Reference Manual for Ivy Bridge (Volume 3
//! Part 3, May 2012: section 2, "Pipe timings change", and sections 4.1.7, 4.5
//! and 5.3) and compared with Linux's `intel_pipe_fastset`.
//!
//! [`plan`] only reads, and refuses unless the hardware is in exactly the state
//! the method needs. An unknown model or any other state keeps the firmware's
//! mode, and says why. [`apply`] writes, checks, and restores on failure.

#![no_std]

/// Access to BAR 0 of the graphics device: registers, and from
/// [`GTT_BASE`] the global GTT.
pub trait Regs {
    fn read(&self, offset: u32) -> u32;
    fn write(&mut self, offset: u32, value: u32);
}

/// Device IDs of the family this is written and tested for: Ivy Bridge
/// (mobile, desktop and server graphics).
pub const SUPPORTED: [u16; 6] = [0x0156, 0x0166, 0x0152, 0x015A, 0x0162, 0x016A];

/// Why the firmware's mode stays. The numbers are those of
/// `aricode_abi::sysinfo::display_refusal`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not a device of a family in [`SUPPORTED`].
    NotThisFamily = 1,
    /// BAR 0 or the aperture is not where the framebuffer needs them.
    BadBars = 2,
    /// The VGA plane is on.
    VgaPlane = 3,
    /// Not exactly one pipe is running.
    PipeCount = 4,
    Interlaced = 5,
    /// The pipe's timing is not the panel's size: a real mode set would
    /// be needed.
    NotPanelTiming = 6,
    /// The pipe's source is not the firmware's mode.
    SourceNotFirmwareMode = 7,
    /// No panel fitter fills the panel from this pipe.
    NoFitter = 8,
    /// The primary plane does not scan out the framebuffer as described.
    PlaneNotFramebuffer = 9,
    /// An odd width (Linux: dual-channel LVDS cannot show one).
    OddWidth = 10,
    /// The buffer does not fit the limit, the stride or the aperture.
    TooLarge = 11,
    /// The firmware's GTT does not map the whole buffer.
    NotMapped = 12,
    /// The pipe stopped counting frames.
    NoFrames = 13,
    /// A register did not take its value.
    NotApplied = 14,
}

/// What is known without the device's registers.
#[derive(Debug, Clone, Copy)]
pub struct Request {
    /// PCI device ID.
    pub device: u16,
    /// The panel's size, from its EDID.
    pub panel: (u32, u32),
    /// The firmware's mode.
    pub firmware: (u32, u32),
    /// Pixels per row of the firmware's mode.
    pub firmware_stride: u32,
    /// The framebuffer's offset in the aperture.
    pub surface: u64,
    pub aperture_size: u64,
    /// The largest buffer allowed.
    pub max_bytes: u64,
}

/// The change [`apply`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pipe: u32,
    fitter: u32,
    pub width: u32,
    pub height: u32,
    /// Pixels per row.
    pub stride: u32,
    /// Bytes the screen takes.
    pub bytes: u64,
}

/// Start of the global GTT in BAR 0 (Volume 3 Part 2, GTTMMADR).
pub const GTT_BASE: u32 = 0x20_0000;
/// Size of BAR 0.
pub const BAR0_SIZE: u64 = 0x40_0000;
const GTT_ENTRIES: u64 = (BAR0_SIZE - GTT_BASE as u64) / 4;
const PTE_VALID: u32 = 1;
const PAGE: u64 = 4096;
/// PRM, PRI_SURF workaround: 128 more entries beyond the displayed surface.
const EXTRA_ENTRIES: u64 = 128;

const VGA_CONTROL: u32 = 0x41000;
const VGA_DISABLE: u32 = 1 << 31;

const PIPES: u32 = 3;
const fn pipe_timing(pipe: u32, reg: u32) -> u32 {
    0x60000 + 0x1000 * pipe + reg
}
const HTOTAL: u32 = 0x00;
const VTOTAL: u32 = 0x0C;
const SRCSZ: u32 = 0x1C;

const fn pipe_ctl(pipe: u32, reg: u32) -> u32 {
    0x70000 + 0x1000 * pipe + reg
}
const PIPE_CONF: u32 = 0x08;
const PIPE_ENABLED: u32 = 3 << 30;
const PIPE_INTERLACE_MASK: u32 = 7 << 21;
const PIPE_FRMCNT: u32 = 0x40;

const PRI_CTL: u32 = 0x180;
const PRI_LINOFF: u32 = 0x184;
const PRI_STRIDE: u32 = 0x188;
const PRI_SURF: u32 = 0x19C;
const PRI_ENABLE: u32 = 1 << 31;
const PRI_FORMAT_MASK: u32 = 0xF << 26;
/// 32-bit BGRX 8:8:8.
const PRI_FORMAT_BGRX: u32 = 0b0110 << 26;
const PRI_TILED: u32 = 1 << 10;
/// PRM, PRI_STRIDE: the stride of linear memory.
const STRIDE_ALIGN: u64 = 64;
const STRIDE_MAX: u64 = 32 * 1024;

const FITTERS: u32 = 3;
const fn fitter_reg(fitter: u32, reg: u32) -> u32 {
    0x68000 + 0x800 * fitter + reg
}
const PF_WIN_POS: u32 = 0x70;
const PF_WIN_SZ: u32 = 0x74;
const PF_CTRL: u32 = 0x80;
const PF_ENABLE: u32 = 1 << 31;
const PF_PIPE_SHIFT: u32 = 29;
const PF_PIPE_MASK: u32 = 3;

/// Reads of the frame counter before giving up. Each is an uncached read,
/// so this is far more than a frame of any refresh rate.
const FRAME_READS: u32 = 2_000_000;

/// The aperture's size from the byte of MSAC (PCI configuration 62h, Volume 3
/// Part 2, section 1.29); `None` for its illegal value.
pub const fn aperture_size(msac: u8) -> Option<u64> {
    match (msac >> 1) & 3 {
        0b00 => Some(128 << 20),
        0b01 => Some(256 << 20),
        0b11 => Some(512 << 20),
        _ => None,
    }
}

const fn size_field(w: u32, h: u32) -> u32 {
    (w - 1) << 16 | (h - 1)
}

/// The active size in a timing register: bits 11:0, minus one.
const fn active(reg: u32) -> u32 {
    (reg & 0xFFF) + 1
}

/// Reads the hardware and decides. Writes nothing.
pub fn plan(regs: &impl Regs, r: &Request) -> Result<Plan, Refusal> {
    if !SUPPORTED.contains(&r.device) {
        return Err(Refusal::NotThisFamily);
    }
    if regs.read(VGA_CONTROL) & VGA_DISABLE == 0 {
        return Err(Refusal::VgaPlane);
    }
    let mut running =
        (0..PIPES).filter(|&p| regs.read(pipe_ctl(p, PIPE_CONF)) & PIPE_ENABLED == PIPE_ENABLED);
    let (Some(pipe), None) = (running.next(), running.next()) else {
        return Err(Refusal::PipeCount);
    };
    if regs.read(pipe_ctl(pipe, PIPE_CONF)) & PIPE_INTERLACE_MASK != 0 {
        return Err(Refusal::Interlaced);
    }
    let (width, height) = r.panel;
    if width == 0
        || height == 0
        || active(regs.read(pipe_timing(pipe, HTOTAL))) != width
        || active(regs.read(pipe_timing(pipe, VTOTAL))) != height
    {
        return Err(Refusal::NotPanelTiming);
    }
    let (fw, fh) = r.firmware;
    if fw == 0 || fh == 0 || regs.read(pipe_timing(pipe, SRCSZ)) != size_field(fw, fh) {
        return Err(Refusal::SourceNotFirmwareMode);
    }
    let fitter = find_fitter(regs, pipe, width, height).ok_or(Refusal::NoFitter)?;

    let ctl = regs.read(pipe_ctl(pipe, PRI_CTL));
    if ctl & PRI_ENABLE == 0
        || ctl & PRI_FORMAT_MASK != PRI_FORMAT_BGRX
        || ctl & PRI_TILED != 0
        || regs.read(pipe_ctl(pipe, PRI_LINOFF)) != 0
        || regs.read(pipe_ctl(pipe, PRI_STRIDE)) as u64 != r.firmware_stride as u64 * 4
        || regs.read(pipe_ctl(pipe, PRI_SURF)) as u64 != r.surface
    {
        return Err(Refusal::PlaneNotFramebuffer);
    }
    if width % 2 != 0 {
        return Err(Refusal::OddWidth);
    }
    let stride_bytes = (width as u64 * 4).next_multiple_of(STRIDE_ALIGN);
    let bytes = stride_bytes * height as u64;
    if stride_bytes > STRIDE_MAX
        || bytes > r.max_bytes
        || r.surface.saturating_add(bytes) > r.aperture_size
    {
        return Err(Refusal::TooLarge);
    }
    let first = r.surface / PAGE;
    let entries = bytes.div_ceil(PAGE) + EXTRA_ENTRIES;
    if first + entries > GTT_ENTRIES {
        return Err(Refusal::NotMapped);
    }
    let mapped =
        (first..first + entries).all(|e| regs.read(GTT_BASE + e as u32 * 4) & PTE_VALID != 0);
    if !mapped {
        return Err(Refusal::NotMapped);
    }
    Ok(Plan {
        pipe,
        fitter,
        width,
        height,
        stride: (stride_bytes / 4) as u32,
        bytes,
    })
}

/// The one enabled fitter on `pipe`, filling the whole panel from its corner.
fn find_fitter(regs: &impl Regs, pipe: u32, width: u32, height: u32) -> Option<u32> {
    let mut on_pipe = (0..FITTERS).filter(|&f| {
        let ctl = regs.read(fitter_reg(f, PF_CTRL));
        ctl & PF_ENABLE != 0 && (ctl >> PF_PIPE_SHIFT) & PF_PIPE_MASK == pipe
    });
    let (Some(fitter), None) = (on_pipe.next(), on_pipe.next()) else {
        return None;
    };
    let fills = regs.read(fitter_reg(fitter, PF_WIN_POS)) == 0
        && regs.read(fitter_reg(fitter, PF_WIN_SZ)) == (width << 16 | height);
    fills.then_some(fitter)
}

/// Waits for the start of the next vertical blank, where the registers
/// written so far take effect.
fn next_frame(regs: &impl Regs, pipe: u32) -> Result<(), Refusal> {
    let start = regs.read(pipe_ctl(pipe, PIPE_FRMCNT));
    for _ in 0..FRAME_READS {
        if regs.read(pipe_ctl(pipe, PIPE_FRMCNT)) != start {
            return Ok(());
        }
    }
    Err(Refusal::NoFrames)
}

/// Makes the change, in the order tried on the reference laptop:
/// 1. With the fitter on, the source becomes the panel's size: the PRM allows
///    this on the running pipe. A write to PRI_SURF arms the stride.
/// 2. The next frame, the fitter steps aside, as Linux does on this family.
///    The PRM requires the source to equal the active size without a fitter,
///    which it now does.
///
/// A failure restores what was there.
pub fn apply(regs: &mut impl Regs, plan: &Plan) -> Result<(), Refusal> {
    let p = plan.pipe;
    let f = plan.fitter;
    let source = size_field(plan.width, plan.height);
    let stride_bytes = plan.stride * 4;
    let old_source = regs.read(pipe_timing(p, SRCSZ));
    let old_stride = regs.read(pipe_ctl(p, PRI_STRIDE));
    let old_ctrl = regs.read(fitter_reg(f, PF_CTRL));
    let old_size = regs.read(fitter_reg(f, PF_WIN_SZ));
    let surface = regs.read(pipe_ctl(p, PRI_SURF));

    regs.write(pipe_ctl(p, PRI_STRIDE), stride_bytes);
    regs.write(pipe_timing(p, SRCSZ), source);
    regs.write(pipe_ctl(p, PRI_SURF), surface);
    let mut result = next_frame(regs, p);
    if result.is_ok() {
        regs.write(fitter_reg(f, PF_CTRL), 0);
        regs.write(fitter_reg(f, PF_WIN_POS), 0);
        regs.write(fitter_reg(f, PF_WIN_SZ), 0);
        result = next_frame(regs, p);
    }
    let taken = regs.read(pipe_timing(p, SRCSZ)) == source
        && regs.read(pipe_ctl(p, PRI_STRIDE)) == stride_bytes
        && regs.read(fitter_reg(f, PF_CTRL)) == 0
        && regs.read(pipe_ctl(p, PIPE_CONF)) & PIPE_ENABLED == PIPE_ENABLED;
    if result.is_ok() && !taken {
        result = Err(Refusal::NotApplied);
    }
    if result.is_err() {
        regs.write(fitter_reg(f, PF_CTRL), old_ctrl);
        regs.write(fitter_reg(f, PF_WIN_POS), 0);
        regs.write(fitter_reg(f, PF_WIN_SZ), old_size);
        regs.write(pipe_ctl(p, PRI_STRIDE), old_stride);
        regs.write(pipe_timing(p, SRCSZ), old_source);
        regs.write(pipe_ctl(p, PRI_SURF), surface);
    }
    result
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::vec::Vec;

    /// A register bank; the frame counter advances as it is read.
    struct Fake {
        regs: BTreeMap<u32, u32>,
        frames: Cell<u32>,
        frozen: bool,
        ignored: BTreeSet<u32>,
        writes: Vec<(u32, u32)>,
    }

    impl Regs for Fake {
        fn read(&self, offset: u32) -> u32 {
            if offset == pipe_ctl(0, PIPE_FRMCNT) && !self.frozen {
                self.frames.set(self.frames.get() + 1);
                return self.frames.get();
            }
            self.regs.get(&offset).copied().unwrap_or(0)
        }

        fn write(&mut self, offset: u32, value: u32) {
            self.writes.push((offset, value));
            if !self.ignored.contains(&offset) {
                self.regs.insert(offset, value);
            }
        }
    }

    /// What the reference laptop's firmware left, read on 2026-11-05: an
    /// LVDS panel at 1366x768 on pipe A, an 800x600 source, fitter 0.
    fn laptop() -> Fake {
        let mut regs = BTreeMap::from([
            (0x41000, 0x8000_298E),
            (0x60000, 0x05B9_0555),
            (0x6000C, 0x0313_02FF),
            (0x6001C, 0x031F_0257),
            (0x68070, 0),
            (0x68074, 0x0556_0300),
            (0x68080, 0x8080_0000),
            (0x70008, 0xD800_0050),
            (0x70180, 0x9800_0000),
            (0x70184, 0),
            (0x70188, 3200),
            (0x7019C, 0),
        ]);
        for e in 0..16384u32 {
            regs.insert(GTT_BASE + e * 4, 0xDBE0_0001 + e * 0x1000);
        }
        Fake {
            regs,
            frames: Cell::new(0),
            frozen: false,
            ignored: BTreeSet::new(),
            writes: Vec::new(),
        }
    }

    fn request() -> Request {
        Request {
            device: 0x0166,
            panel: (1366, 768),
            firmware: (800, 600),
            firmware_stride: 800,
            surface: 0,
            aperture_size: 256 << 20,
            max_bytes: 8 << 20,
        }
    }

    #[test]
    fn the_laptop_gets_its_panel_size() {
        let mut hw = laptop();
        let plan = plan(&hw, &request()).unwrap();
        assert_eq!((plan.width, plan.height), (1366, 768));
        assert_eq!(plan.stride, 1376, "1366 x 4 bytes, up to 64");
        assert_eq!(plan.bytes, 5504 * 768);
        assert!(hw.writes.is_empty(), "plan only reads");

        apply(&mut hw, &plan).unwrap();
        assert_eq!(hw.regs[&0x6001C], 1365 << 16 | 767);
        assert_eq!(hw.regs[&0x70188], 5504);
        assert_eq!(hw.regs[&0x68080], 0);
        assert_eq!(hw.regs[&0x68074], 0);
        assert_eq!(hw.regs[&0x70008], 0xD800_0050, "the pipe is not touched");
        assert_eq!(hw.regs[&0x60000], 0x05B9_0555, "nor its timing");
    }

    #[test]
    fn the_source_changes_before_the_fitter_leaves() {
        let mut hw = laptop();
        let plan = plan(&hw, &request()).unwrap();
        apply(&mut hw, &plan).unwrap();
        let at = |reg: u32| hw.writes.iter().position(|w| w.0 == reg).unwrap();
        assert!(at(0x70188) < at(0x7019C), "PRI_SURF arms the stride");
        assert!(at(0x6001C) < at(0x68080));
        assert!(at(0x68080) < at(0x68074), "PF_WIN_SZ arms the fitter");
    }

    /// Each change to the laptop's state, with the refusal it must cause.
    #[test]
    fn any_other_state_is_left_alone() {
        type Change = fn(&mut Fake, &mut Request);
        let cases: [(Change, Refusal); 15] = [
            (|_, r| r.device = 0x1912, Refusal::NotThisFamily),
            (|h, _| drop(h.regs.insert(0x41000, 0)), Refusal::VgaPlane),
            (|h, _| drop(h.regs.insert(0x70008, 0)), Refusal::PipeCount),
            (
                |h, _| drop(h.regs.insert(0x71008, 0xC000_0000)),
                Refusal::PipeCount,
            ),
            (
                |h, _| drop(h.regs.insert(0x70008, 0xD860_0050)),
                Refusal::Interlaced,
            ),
            (|_, r| r.panel = (1920, 1080), Refusal::NotPanelTiming),
            (
                |h, _| drop(h.regs.insert(0x60000, 0x05B9_031F)),
                Refusal::NotPanelTiming,
            ),
            (
                |_, r| r.firmware = (1024, 768),
                Refusal::SourceNotFirmwareMode,
            ),
            (|h, _| drop(h.regs.insert(0x68080, 0)), Refusal::NoFitter),
            (
                |h, _| drop(h.regs.insert(0x68074, 0x031F_0257)),
                Refusal::NoFitter,
            ),
            (
                |h, _| drop(h.regs.insert(0x70180, 0x9800_0400)),
                Refusal::PlaneNotFramebuffer,
            ),
            (|_, r| r.surface = 0x10_0000, Refusal::PlaneNotFramebuffer),
            (|_, r| r.max_bytes = 1 << 20, Refusal::TooLarge),
            (|_, r| r.aperture_size = 1 << 20, Refusal::TooLarge),
            (
                |h, _| drop(h.regs.insert(GTT_BASE + 1000 * 4, 0)),
                Refusal::NotMapped,
            ),
        ];
        for (i, (change, refusal)) in cases.iter().enumerate() {
            let mut hw = laptop();
            let mut r = request();
            change(&mut hw, &mut r);
            assert_eq!(plan(&hw, &r), Err(*refusal), "case {i}");
            assert!(hw.writes.is_empty(), "case {i}");
        }
    }

    #[test]
    fn an_odd_width_is_refused() {
        let mut hw = laptop();
        hw.regs.insert(0x60000, 0x05B9_0554);
        hw.regs.insert(0x68074, 0x0555_0300);
        let r = Request {
            panel: (1365, 768),
            ..request()
        };
        assert_eq!(plan(&hw, &r), Err(Refusal::OddWidth));
    }

    fn firmware_state(hw: &Fake) -> [u32; 4] {
        [0x6001C, 0x70188, 0x68080, 0x68074].map(|r| hw.regs[&r])
    }

    #[test]
    fn a_stopped_pipe_restores_the_firmware_state() {
        let mut hw = laptop();
        let before = firmware_state(&hw);
        let plan = plan(&hw, &request()).unwrap();
        hw.frozen = true;
        assert_eq!(apply(&mut hw, &plan), Err(Refusal::NoFrames));
        assert_eq!(firmware_state(&hw), before);
    }

    #[test]
    fn a_register_that_ignores_its_value_restores_the_firmware_state() {
        let mut hw = laptop();
        let before = firmware_state(&hw);
        let plan = plan(&hw, &request()).unwrap();
        hw.ignored.insert(0x6001C);
        assert_eq!(apply(&mut hw, &plan), Err(Refusal::NotApplied));
        assert_eq!(firmware_state(&hw), before);
    }

    #[test]
    fn the_aperture_size_comes_from_msac() {
        assert_eq!(aperture_size(0b000), Some(128 << 20));
        assert_eq!(aperture_size(0b010), Some(256 << 20), "the default, 02h");
        assert_eq!(aperture_size(0b110), Some(512 << 20));
        assert_eq!(aperture_size(0b100), None, "illegal");
        assert_eq!(aperture_size(0xF2), Some(256 << 20), "scratch bits");
    }

    #[test]
    fn refusal_numbers_match_the_abi() {
        use aricode_abi::sysinfo::display_refusal;
        assert_eq!(display_refusal(0), None);
        assert!(display_refusal(Refusal::NotThisFamily as u8).is_some());
        assert!(display_refusal(Refusal::NotApplied as u8).is_some());
        assert_eq!(display_refusal(Refusal::NotApplied as u8 + 1), None);
    }
}
