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
//! mode, and says why. [`apply`] writes, checks, and restores on failure. The
//! kernel calls these once, during boot, before any program has the screen.

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
    /// Watermarks set for less than the pipe's default, or low-power ones,
    /// computed for the firmware's width (no Programming Watermarks document
    /// to recompute them from).
    Watermarks = 15,
    /// The pipe's FIFO underran at the new size, or its plane faulted.
    Underrun = 16,
    /// Frame buffer compression or a sprite is on.
    OtherFeatures = 17,
}

/// What is known without the device's registers.
#[derive(Debug, Clone, Copy)]
pub struct Request {
    /// PCI device ID.
    pub device: u16,
    /// The panel's size from its EDID, if the loader had one. The pipe's
    /// timing says it too; this must not disagree.
    pub panel: Option<(u32, u32)>,
    /// The firmware's mode.
    pub firmware: (u32, u32),
    /// Pixels per row of the firmware's mode.
    pub firmware_stride: u32,
    /// The framebuffer's offset in the aperture.
    pub surface: u64,
    pub aperture_size: u64,
    /// The largest buffer allowed.
    pub max_bytes: u64,
    /// How much of the aperture, from the framebuffer's start, is known to be
    /// the screen's: the firmware's framebuffer region.
    pub region: u64,
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

const FBC_CTL: u32 = 0x43208;
const FBC_ENABLE: u32 = 1 << 31;

/// Sticky bits, cleared by writing 1 to them.
const ERR_INT: u32 = 0x44040;
const fn err_underrun(pipe: u32) -> u32 {
    1 << (3 * pipe)
}
const fn err_primary_fault(pipe: u32) -> u32 {
    1 << (16 + 3 * pipe)
}

const WM_PIPE: [u32; 3] = [0x45100, 0x45104, 0x45200];
const WM_LP: [u32; 3] = [0x45108, 0x4510C, 0x45110];
const WM_LP_ENABLE: u32 = 1 << 31;
const WM_PRIMARY_SHIFT: u32 = 16;
const WM_PRIMARY_MASK: u32 = 0x7F;
/// The field's default, which the PRM says allows any high power mode.
const WM_PRIMARY_DEFAULT: u32 = 0b111_1000;

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
const PRI_ASYNC: u32 = 1 << 9;
const SPR_CTL: u32 = 0x280;
const SPR_ENABLE: u32 = 1 << 31;
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
/// PRM, PF_CTRL: fitter 0 is the 7x5 one; the 3x3 ones take less.
const FITTER_MAX_WIDTH: [u32; 3] = [4096, 2048, 2048];

/// Reads of the frame counter before giving up. Each is an uncached read,
/// so this is far more than a frame of any refresh rate.
const FRAME_READS: u32 = 2_000_000;

const fn size_field(w: u32, h: u32) -> u32 {
    (w - 1) << 16 | (h - 1)
}

/// The active size in a timing register: bits 11:0, minus one.
const fn active(reg: u32) -> u32 {
    (reg & 0xFFF) + 1
}

/// Reads the hardware and decides. Writes nothing. `None`: the screen already
/// has the pipe's size, so there is nothing to change.
pub fn plan(regs: &impl Regs, r: &Request) -> Result<Option<Plan>, Refusal> {
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
    let width = active(regs.read(pipe_timing(pipe, HTOTAL)));
    let height = active(regs.read(pipe_timing(pipe, VTOTAL)));
    if r.panel.is_some_and(|p| p != (width, height)) {
        return Err(Refusal::NotPanelTiming);
    }
    let (fw, fh) = r.firmware;
    if (fw, fh) == (width, height) {
        return Ok(None);
    }
    if fw == 0 || fh == 0 || regs.read(pipe_timing(pipe, SRCSZ)) != size_field(fw, fh) {
        return Err(Refusal::SourceNotFirmwareMode);
    }
    let fitter = find_fitter(regs, pipe, width, height).ok_or(Refusal::NoFitter)?;
    if regs.read(FBC_CTL) & FBC_ENABLE != 0 || regs.read(pipe_ctl(pipe, SPR_CTL)) & SPR_ENABLE != 0
    {
        return Err(Refusal::OtherFeatures);
    }
    let primary_wm = (regs.read(WM_PIPE[pipe as usize]) >> WM_PRIMARY_SHIFT) & WM_PRIMARY_MASK;
    if primary_wm < WM_PRIMARY_DEFAULT || WM_LP.iter().any(|&r| regs.read(r) & WM_LP_ENABLE != 0) {
        return Err(Refusal::Watermarks);
    }

    let ctl = regs.read(pipe_ctl(pipe, PRI_CTL));
    if ctl & PRI_ENABLE == 0
        || ctl & PRI_FORMAT_MASK != PRI_FORMAT_BGRX
        || ctl & PRI_TILED != 0
        || ctl & PRI_ASYNC != 0
        || regs.read(pipe_ctl(pipe, PRI_LINOFF)) != 0
        || regs.read(pipe_ctl(pipe, PRI_STRIDE)) as u64 != r.firmware_stride as u64 * 4
        || regs.read(pipe_ctl(pipe, PRI_SURF)) as u64 != r.surface
    {
        return Err(Refusal::PlaneNotFramebuffer);
    }
    if !width.is_multiple_of(2) {
        return Err(Refusal::OddWidth);
    }
    let stride_bytes = (width as u64 * 4).next_multiple_of(STRIDE_ALIGN);
    let bytes = stride_bytes * height as u64;
    if stride_bytes > STRIDE_MAX
        || width > FITTER_MAX_WIDTH[fitter as usize]
        || bytes > r.max_bytes
        || bytes > r.region
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
    Ok(Some(Plan {
        pipe,
        fitter,
        width,
        height,
        stride: (stride_bytes / 4) as u32,
        bytes,
    }))
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
/// 3. With the new state in place, a frame shows whether the pipe's FIFO keeps
///    up and its plane reads what the GTT maps.
///
/// A failure restores what was there.
pub fn apply(regs: &mut impl Regs, plan: &Plan) -> Result<(), Refusal> {
    let p = plan.pipe;
    let f = plan.fitter;
    let old_source = regs.read(pipe_timing(p, SRCSZ));
    let old_stride = regs.read(pipe_ctl(p, PRI_STRIDE));
    let old_ctrl = regs.read(fitter_reg(f, PF_CTRL));
    let old_size = regs.read(fitter_reg(f, PF_WIN_SZ));
    let surface = regs.read(pipe_ctl(p, PRI_SURF));
    let result = change(regs, plan, surface);
    if result.is_err() {
        // PF_WIN_SZ arms the fitter, and PRI_SURF the stride.
        regs.write(fitter_reg(f, PF_CTRL), old_ctrl);
        regs.write(fitter_reg(f, PF_WIN_POS), 0);
        regs.write(fitter_reg(f, PF_WIN_SZ), old_size);
        regs.write(pipe_ctl(p, PRI_STRIDE), old_stride);
        regs.write(pipe_timing(p, SRCSZ), old_source);
        regs.write(pipe_ctl(p, PRI_SURF), surface);
    }
    result
}

fn change(regs: &mut impl Regs, plan: &Plan, surface: u32) -> Result<(), Refusal> {
    let p = plan.pipe;
    let f = plan.fitter;
    let source = size_field(plan.width, plan.height);
    let stride_bytes = plan.stride * 4;
    regs.write(pipe_ctl(p, PRI_STRIDE), stride_bytes);
    regs.write(pipe_timing(p, SRCSZ), source);
    regs.write(pipe_ctl(p, PRI_SURF), surface);
    next_frame(regs, p)?;
    regs.write(fitter_reg(f, PF_CTRL), 0);
    regs.write(fitter_reg(f, PF_WIN_POS), 0);
    regs.write(fitter_reg(f, PF_WIN_SZ), 0);
    next_frame(regs, p)?;
    let taken = regs.read(pipe_timing(p, SRCSZ)) == source
        && regs.read(pipe_ctl(p, PRI_STRIDE)) == stride_bytes
        && regs.read(fitter_reg(f, PF_CTRL)) == 0
        && regs.read(pipe_ctl(p, PIPE_CONF)) & PIPE_ENABLED == PIPE_ENABLED;
    if !taken {
        return Err(Refusal::NotApplied);
    }
    // A whole frame in the new state, starting from clean bits.
    let errors = err_underrun(p) | err_primary_fault(p);
    regs.write(ERR_INT, errors);
    next_frame(regs, p)?;
    next_frame(regs, p)?;
    if regs.read(ERR_INT) & errors != 0 {
        return Err(Refusal::Underrun);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::vec::Vec;

    /// A register bank with the manual's double buffering: writes to the
    /// source, the stride and the fitter wait for the start of a vertical
    /// blank, the last two once armed. Reading the frame counter advances it.
    struct Fake {
        pipe: u32,
        fitter: u32,
        live: RefCell<BTreeMap<u32, u32>>,
        pending: RefCell<BTreeMap<u32, u32>>,
        plane_armed: Cell<bool>,
        fitter_armed: Cell<bool>,
        frames: Cell<u32>,
        /// The pipe stops counting after this many frames.
        frames_left: Cell<Option<u32>>,
        /// Each frame sets these in ERR_INT.
        errors: u32,
        /// Registers that do not take what is written.
        ignored: Vec<u32>,
        writes: Vec<(u32, u32)>,
        /// (frame, register) as each pending value took effect.
        latched: RefCell<Vec<(u32, u32)>>,
    }

    impl Fake {
        fn value(&self, offset: u32) -> u32 {
            self.live.borrow().get(&offset).copied().unwrap_or(0)
        }

        fn set(&mut self, offset: u32, value: u32) {
            self.live.get_mut().insert(offset, value);
        }

        fn vblank(&self) {
            let frame = self.frames.get() + 1;
            self.frames.set(frame);
            let stride = pipe_ctl(self.pipe, PRI_STRIDE);
            let fitter = [PF_CTRL, PF_WIN_POS, PF_WIN_SZ].map(|r| fitter_reg(self.fitter, r));
            let mut pending = self.pending.borrow_mut();
            let mut live = self.live.borrow_mut();
            pending.retain(|&reg, &mut value| {
                let ready = if reg == stride {
                    self.plane_armed.get()
                } else if fitter.contains(&reg) {
                    self.fitter_armed.get()
                } else {
                    true
                };
                if ready {
                    live.insert(reg, value);
                    self.latched.borrow_mut().push((frame, reg));
                }
                !ready
            });
            if self.errors != 0 {
                let e = live.entry(ERR_INT).or_insert(0);
                *e |= self.errors;
            }
            self.plane_armed.set(false);
            self.fitter_armed.set(false);
        }

        /// What the registers will hold once the pending writes take effect.
        fn settled(&self, reg: u32) -> u32 {
            self.pending
                .borrow()
                .get(&reg)
                .copied()
                .unwrap_or_else(|| self.value(reg))
        }
    }

    impl Regs for Fake {
        fn read(&self, offset: u32) -> u32 {
            assert!((offset as u64) < BAR0_SIZE, "outside BAR 0: {offset:#x}");
            if offset == pipe_ctl(self.pipe, PIPE_FRMCNT) {
                let running = match self.frames_left.get() {
                    Some(0) => false,
                    Some(n) => {
                        self.frames_left.set(Some(n - 1));
                        true
                    }
                    None => true,
                };
                if running {
                    self.vblank();
                }
                return self.frames.get();
            }
            self.value(offset)
        }

        fn write(&mut self, offset: u32, value: u32) {
            assert!((offset as u64) < BAR0_SIZE, "outside BAR 0: {offset:#x}");
            self.writes.push((offset, value));
            if self.ignored.contains(&offset) {
                return;
            }
            let (p, f) = (self.pipe, self.fitter);
            if offset == ERR_INT {
                let cleared = self.value(ERR_INT) & !value;
                self.set(ERR_INT, cleared);
            } else if offset == pipe_ctl(p, PRI_SURF) {
                self.set(offset, value);
                self.plane_armed.set(true);
            } else if offset == fitter_reg(f, PF_WIN_SZ) {
                self.pending.get_mut().insert(offset, value);
                self.fitter_armed.set(true);
            } else {
                self.pending.get_mut().insert(offset, value);
            }
        }
    }

    /// What the reference laptop's firmware left, read on 2026-11-05: an
    /// LVDS panel at 1366x768, an 800x600 source, on pipe A and fitter 0.
    fn laptop() -> Fake {
        on(0, 0)
    }

    fn on(pipe: u32, fitter: u32) -> Fake {
        let mut live = BTreeMap::from([
            (VGA_CONTROL, 0x8000_298E),
            (WM_PIPE[pipe as usize], 0x0078_3818),
            (pipe_timing(pipe, HTOTAL), 0x05B9_0555),
            (pipe_timing(pipe, VTOTAL), 0x0313_02FF),
            (pipe_timing(pipe, SRCSZ), 0x031F_0257),
            (fitter_reg(fitter, PF_WIN_SZ), 0x0556_0300),
            (
                fitter_reg(fitter, PF_CTRL),
                0x8080_0000 | pipe << PF_PIPE_SHIFT,
            ),
            (pipe_ctl(pipe, PIPE_CONF), 0xD800_0050),
            (pipe_ctl(pipe, PRI_CTL), 0x9800_0000),
            (pipe_ctl(pipe, PRI_STRIDE), 3200),
        ]);
        for e in 0..16384u32 {
            live.insert(GTT_BASE + e * 4, 0xDBE0_0001 + e * 0x1000);
        }
        Fake {
            pipe,
            fitter,
            live: RefCell::new(live),
            pending: RefCell::new(BTreeMap::new()),
            plane_armed: Cell::new(false),
            fitter_armed: Cell::new(false),
            frames: Cell::new(0),
            frames_left: Cell::new(None),
            errors: 0,
            ignored: Vec::new(),
            writes: Vec::new(),
            latched: RefCell::new(Vec::new()),
        }
    }

    fn request() -> Request {
        Request {
            device: 0x0166,
            panel: Some((1366, 768)),
            firmware: (800, 600),
            firmware_stride: 800,
            surface: 0,
            aperture_size: 256 << 20,
            max_bytes: 8 << 20,
            region: 64 << 20,
        }
    }

    #[test]
    fn the_laptop_gets_its_panel_size() {
        let mut hw = laptop();
        let plan = plan(&hw, &request()).unwrap().unwrap();
        assert_eq!((plan.width, plan.height), (1366, 768));
        assert_eq!(plan.stride, 1376, "1366 x 4 bytes, up to 64");
        assert_eq!(plan.bytes, 5504 * 768);
        assert!(hw.writes.is_empty(), "plan only reads");

        apply(&mut hw, &plan).unwrap();
        assert_eq!(hw.value(0x6001C), 1365 << 16 | 767);
        assert_eq!(hw.value(0x70188), 5504);
        assert_eq!(hw.value(0x68080), 0);
        assert_eq!(hw.value(0x68074), 0);
        assert_eq!(hw.value(0x70008), 0xD800_0050, "the pipe is not touched");
        assert_eq!(hw.value(0x60000), 0x05B9_0555, "nor its timing");
    }

    #[test]
    fn every_pipe_and_fitter_use_their_own_registers() {
        for (pipe, fitter) in [(1, 1), (2, 2), (1, 0)] {
            let mut hw = on(pipe, fitter);
            let plan = plan(&hw, &request()).unwrap().unwrap();
            apply(&mut hw, &plan).unwrap();
            assert_eq!(hw.value(pipe_timing(pipe, SRCSZ)), 1365 << 16 | 767);
            assert_eq!(hw.value(fitter_reg(fitter, PF_CTRL)), 0);
            assert_eq!(hw.value(pipe_timing(0, SRCSZ)), 0, "no other pipe's");
        }
    }

    #[test]
    fn a_loader_without_an_edid_still_gets_it_from_the_pipe() {
        let r = Request {
            panel: None,
            ..request()
        };
        let plan = plan(&laptop(), &r).unwrap().unwrap();
        assert_eq!((plan.width, plan.height), (1366, 768));
    }

    #[test]
    fn a_screen_at_the_pipes_size_needs_nothing() {
        let mut hw = laptop();
        hw.set(0x68080, 0);
        hw.set(0x6001C, 0x0555_02FF);
        let r = Request {
            firmware: (1366, 768),
            firmware_stride: 1376,
            ..request()
        };
        assert_eq!(plan(&hw, &r), Ok(None));
        assert!(hw.writes.is_empty());
    }

    #[test]
    fn writes_are_armed_in_the_manuals_order() {
        let mut hw = laptop();
        let plan = plan(&hw, &request()).unwrap().unwrap();
        apply(&mut hw, &plan).unwrap();
        let at = |reg: u32| hw.writes.iter().position(|w| w.0 == reg).unwrap();
        assert!(at(0x70188) < at(0x7019C), "PRI_SURF arms the stride");
        assert!(at(0x6001C) < at(0x68080));
        assert!(at(0x68080) < at(0x68074), "PF_WIN_SZ arms the fitter");
        assert!(at(0x68070) < at(0x68074), "PF_WIN_SZ arms the position");
    }

    #[test]
    fn the_fitter_leaves_a_frame_after_the_source_changes() {
        let mut hw = laptop();
        let plan = plan(&hw, &request()).unwrap().unwrap();
        apply(&mut hw, &plan).unwrap();
        let latched = hw.latched.borrow();
        let frame = |reg: u32| latched.iter().find(|l| l.1 == reg).unwrap().0;
        assert_eq!(frame(0x6001C), frame(0x70188), "source and stride together");
        assert!(frame(0x6001C) < frame(0x68080));
    }

    /// Each change to the laptop's state, with the refusal it must cause.
    #[test]
    fn any_other_state_is_left_alone() {
        type Change = fn(&mut Fake, &mut Request);
        let cases: &[(Change, Refusal)] = &[
            (|_, r| r.device = 0x1912, Refusal::NotThisFamily),
            (|h, _| h.set(VGA_CONTROL, 0), Refusal::VgaPlane),
            (|h, _| h.set(0x70008, 0), Refusal::PipeCount),
            (|h, _| h.set(0x71008, 0xC000_0000), Refusal::PipeCount),
            (|h, _| h.set(0x70008, 0xD860_0050), Refusal::Interlaced),
            (|_, r| r.panel = Some((1920, 1080)), Refusal::NotPanelTiming),
            (
                |_, r| r.firmware = (1024, 768),
                Refusal::SourceNotFirmwareMode,
            ),
            (|h, _| h.set(0x68080, 0), Refusal::NoFitter),
            (|h, _| h.set(0x68080, 0xA080_0000), Refusal::NoFitter),
            (|h, _| h.set(0x68880, 0x8080_0000), Refusal::NoFitter),
            (|h, _| h.set(0x68074, 0x031F_0257), Refusal::NoFitter),
            (|h, _| h.set(0x68070, 0x0010_0000), Refusal::NoFitter),
            (|h, _| h.set(FBC_CTL, FBC_ENABLE), Refusal::OtherFeatures),
            (|h, _| h.set(0x70280, SPR_ENABLE), Refusal::OtherFeatures),
            (|h, _| h.set(0x45100, 0x0040_3818), Refusal::Watermarks),
            (|h, _| h.set(0x45108, WM_LP_ENABLE), Refusal::Watermarks),
            (|h, _| h.set(0x45110, WM_LP_ENABLE), Refusal::Watermarks),
            (
                |h, _| h.set(0x70180, 0x1800_0000),
                Refusal::PlaneNotFramebuffer,
            ),
            (
                |h, _| h.set(0x70180, 0x9400_0000),
                Refusal::PlaneNotFramebuffer,
            ),
            (
                |h, _| h.set(0x70180, 0x9800_0400),
                Refusal::PlaneNotFramebuffer,
            ),
            (
                |h, _| h.set(0x70180, 0x9800_0200),
                Refusal::PlaneNotFramebuffer,
            ),
            (|h, _| h.set(0x70184, 64), Refusal::PlaneNotFramebuffer),
            (|h, _| h.set(0x70188, 3264), Refusal::PlaneNotFramebuffer),
            (|_, r| r.surface = 0x10_0000, Refusal::PlaneNotFramebuffer),
            (|_, r| r.max_bytes = 1 << 20, Refusal::TooLarge),
            (|_, r| r.region = 2 << 20, Refusal::TooLarge),
            (|_, r| r.aperture_size = 1 << 20, Refusal::TooLarge),
            (|h, _| h.set(GTT_BASE + 1000 * 4, 0), Refusal::NotMapped),
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
        hw.set(0x60000, 0x05B9_0554);
        hw.set(0x68074, 0x0555_0300);
        let r = Request {
            panel: None,
            ..request()
        };
        assert_eq!(plan(&hw, &r), Err(Refusal::OddWidth));
    }

    #[test]
    fn a_small_fitter_cannot_pass_a_wide_source() {
        // 2560x800, on the 3x3 fitter 1 and on the 7x5 fitter 0.
        let wide = |fitter| {
            let mut hw = on(0, fitter);
            hw.set(pipe_timing(0, HTOTAL), 0x0B00_09FF);
            hw.set(pipe_timing(0, VTOTAL), 0x0340_031F);
            hw.set(fitter_reg(fitter, PF_WIN_SZ), 2560 << 16 | 800);
            hw
        };
        let r = Request {
            panel: None,
            ..request()
        };
        assert_eq!(plan(&wide(1), &r), Err(Refusal::TooLarge));
        assert!(plan(&wide(0), &r).unwrap().is_some());
    }

    fn firmware_state(hw: &Fake) -> [u32; 4] {
        [0x6001C, 0x70188, 0x68080, 0x68074].map(|r| hw.settled(r))
    }

    /// A failure leaves the firmware's values on the way in, armed.
    fn assert_restored(hw: &Fake, before: [u32; 4]) {
        assert_eq!(firmware_state(hw), before);
        let last = |reg: u32| hw.writes.iter().rposition(|w| w.0 == reg).unwrap();
        assert!(last(0x68080) < last(0x68074), "PF_WIN_SZ arms the fitter");
        assert!(last(0x70188) < last(0x7019C), "PRI_SURF arms the stride");
    }

    #[test]
    fn a_pipe_that_stops_at_any_frame_restores_the_firmware_state() {
        // Each wait reads the counter once to start and once per try.
        for frames in 0..6 {
            let mut hw = laptop();
            let before = firmware_state(&hw);
            let plan = plan(&hw, &request()).unwrap().unwrap();
            hw.frames_left.set(Some(frames));
            assert_eq!(apply(&mut hw, &plan), Err(Refusal::NoFrames), "{frames}");
            assert_restored(&hw, before);
        }
    }

    #[test]
    fn a_register_that_ignores_its_value_restores_the_firmware_state() {
        let mut hw = laptop();
        let before = firmware_state(&hw);
        let plan = plan(&hw, &request()).unwrap().unwrap();
        hw.ignored.push(0x6001C);
        assert_eq!(apply(&mut hw, &plan), Err(Refusal::NotApplied));
        assert_eq!(hw.settled(0x70188), before[1]);
        assert_eq!(hw.settled(0x68080), before[2]);
    }

    #[test]
    fn an_underrun_or_a_plane_fault_restores_the_firmware_state() {
        for errors in [err_underrun(0), err_primary_fault(0)] {
            let mut hw = laptop();
            let before = firmware_state(&hw);
            let plan = plan(&hw, &request()).unwrap().unwrap();
            hw.errors = errors;
            assert_eq!(apply(&mut hw, &plan), Err(Refusal::Underrun));
            assert_restored(&hw, before);
        }
    }

    #[test]
    fn an_error_from_before_the_new_state_is_not_held_against_it() {
        let mut hw = laptop();
        hw.set(ERR_INT, err_underrun(0));
        let plan = plan(&hw, &request()).unwrap().unwrap();
        assert_eq!(apply(&mut hw, &plan), Ok(()));
        assert_eq!(hw.value(ERR_INT), 0, "cleared");
    }

    #[test]
    fn another_pipes_error_is_not_this_ones() {
        let mut hw = laptop();
        hw.errors = err_underrun(1);
        let plan = plan(&hw, &request()).unwrap().unwrap();
        assert_eq!(apply(&mut hw, &plan), Ok(()));
    }

    #[test]
    fn refusal_numbers_match_the_abi() {
        use aricode_abi::sysinfo::display_refusal;
        use Refusal::*;
        let all = [
            NotThisFamily,
            BadBars,
            VgaPlane,
            PipeCount,
            Interlaced,
            NotPanelTiming,
            SourceNotFirmwareMode,
            NoFitter,
            PlaneNotFramebuffer,
            OddWidth,
            TooLarge,
            NotMapped,
            NoFrames,
            NotApplied,
            Watermarks,
            Underrun,
            OtherFeatures,
        ];
        assert_eq!(display_refusal(0), None);
        for (i, r) in all.iter().enumerate() {
            assert_eq!(*r as usize, i + 1, "{r:?}");
            assert!(display_refusal(*r as u8).is_some(), "{r:?}");
        }
        assert_eq!(display_refusal(all.len() as u8 + 1), None);
    }
}
