// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Operands: registers of the general file (GRF) and of the architecture
//! file (ARF), and immediates.
//!
//! A register is written the way Mesa's disassembler prints it: number,
//! subregister in elements of its type, region `<vstride,width,hstride>`
//! in elements (PRM Vol 4 Part 3, 3.3.5), and in Align16 a swizzle or a
//! write mask. The encoder turns elements into bytes and checks every
//! value; nothing here can fail.

use crate::Error;

/// GRF registers a gen7 thread has: g0 to g127, 32 bytes each (PRM Vol 4
/// Part 3, 3.3.2).
pub const GRF_COUNT: u8 = 128;
/// Bytes in one register (PRM Vol 4 Part 3, 3.3.1).
pub const REG_BYTES: u32 = 32;

/// Register file codes of DW1 (PRM Vol 4 Part 3, 5.2.4.1). Gen7 has no
/// message register file; `send` reads its payload from the GRF.
const FILE_ARF: u32 = 0;
const FILE_GRF: u32 = 1;
pub(crate) const FILE_IMM: u32 = 3;

/// Architecture registers: RegNum[7:4] is the kind, RegNum[3:0] the number
/// (PRM Vol 4 Part 3, 3.3.3.1; Mesa `elk_eu_defines.h`, `ELK_ARF_*`).
const ARF_NULL: u8 = 0x00;
const ARF_ADDRESS: u8 = 0x10;
const ARF_ACCUMULATOR: u8 = 0x20;
const ARF_FLAG: u8 = 0x30;
const ARF_STATE: u8 = 0x70;
const ARF_IP: u8 = 0xA0;

/// The data type of a register operand (PRM Vol 4 Part 3, 2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    UD,
    D,
    UW,
    W,
    UB,
    B,
    F,
}

impl Type {
    /// Bytes per element.
    pub const fn size(self) -> u32 {
        match self {
            Type::UD | Type::D | Type::F => 4,
            Type::UW | Type::W => 2,
            Type::UB | Type::B => 1,
        }
    }

    /// The "Source Register Type Encoding", which destinations share (PRM
    /// Vol 4 Part 3, 5.2.4.1).
    pub(crate) const fn code(self) -> u32 {
        match self {
            Type::UD => 0,
            Type::D => 1,
            Type::UW => 2,
            Type::W => 3,
            Type::UB => 4,
            Type::B => 5,
            Type::F => 7,
        }
    }

    pub(crate) const fn is_integer(self) -> bool {
        !matches!(self, Type::F)
    }
}

/// The type of an immediate. Bytes have no immediate form; the packed
/// vector types (V, UV, VF) are not offered here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImmType {
    UD,
    D,
    UW,
    W,
    F,
}

impl ImmType {
    /// The "Source Immediate Type Encoding" (PRM Vol 4 Part 3, 5.2.4.1).
    pub(crate) const fn code(self) -> u32 {
        match self {
            ImmType::UD => 0,
            ImmType::D => 1,
            ImmType::UW => 2,
            ImmType::W => 3,
            ImmType::F => 7,
        }
    }

    pub(crate) const fn is_integer(self) -> bool {
        !matches!(self, ImmType::F)
    }
}

/// A 32-bit immediate source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Imm {
    pub(crate) bits: u32,
    pub(crate) ty: ImmType,
}

impl Imm {
    pub const fn ud(value: u32) -> Imm {
        Imm {
            bits: value,
            ty: ImmType::UD,
        }
    }

    pub const fn d(value: i32) -> Imm {
        Imm {
            bits: value as u32,
            ty: ImmType::D,
        }
    }

    /// Word immediates are written twice, in both halves: PRM Vol 4 Part 3,
    /// 5.2.2 (Imm32) asks the programmer to replicate the low word.
    pub const fn uw(value: u16) -> Imm {
        Imm {
            bits: value as u32 * 0x1_0001,
            ty: ImmType::UW,
        }
    }

    pub const fn w(value: i16) -> Imm {
        Imm {
            bits: value as u16 as u32 * 0x1_0001,
            ty: ImmType::W,
        }
    }

    pub const fn f(value: f32) -> Imm {
        Imm {
            bits: value.to_bits(),
            ty: ImmType::F,
        }
    }
}

/// A channel of a group of four, for Align16 swizzles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chan {
    X = 0,
    Y = 1,
    Z = 2,
    W = 3,
}

/// ChanSel of an Align16 source: which channel of the group of four feeds
/// each channel, two bits each, x lowest (PRM Vol 4 Part 3, 5.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swizzle(pub(crate) u8);

impl Swizzle {
    pub const XYZW: Swizzle = Swizzle::new(Chan::X, Chan::Y, Chan::Z, Chan::W);
    pub const XXXX: Swizzle = Swizzle::new(Chan::X, Chan::X, Chan::X, Chan::X);

    pub const fn new(x: Chan, y: Chan, z: Chan, w: Chan) -> Swizzle {
        Swizzle(x as u8 | (y as u8) << 2 | (z as u8) << 4 | (w as u8) << 6)
    }
}

/// ChanEn of an Align16 destination: which channels of each group of four
/// are written (PRM Vol 4 Part 3, 5.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteMask(pub(crate) u8);

impl WriteMask {
    pub const X: WriteMask = WriteMask(1);
    pub const Y: WriteMask = WriteMask(2);
    pub const Z: WriteMask = WriteMask(4);
    pub const W: WriteMask = WriteMask(8);
    pub const XYZW: WriteMask = WriteMask(0xF);
    pub const NONE: WriteMask = WriteMask(0);
}

impl core::ops::BitOr for WriteMask {
    type Output = WriteMask;
    fn bitor(self, other: WriteMask) -> WriteMask {
        WriteMask(self.0 | other.0)
    }
}

/// One of the four 16-bit flag registers: f0.0, f0.1, f1.0, f1.1 (PRM Vol 4
/// Part 3, 3.3.3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    F0_0,
    F0_1,
    F1_0,
    F1_1,
}

impl Flag {
    pub(crate) const fn reg(self) -> u32 {
        matches!(self, Flag::F1_0 | Flag::F1_1) as u32
    }

    pub(crate) const fn subreg(self) -> u32 {
        matches!(self, Flag::F0_1 | Flag::F1_1) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum File {
    Arf,
    Grf,
}

impl File {
    pub(crate) const fn code(self) -> u32 {
        match self {
            File::Arf => FILE_ARF,
            File::Grf => FILE_GRF,
        }
    }
}

/// A register operand, direct addressing only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reg {
    pub(crate) file: File,
    pub(crate) nr: u8,
    /// Subregister in elements of `ty`.
    pub(crate) sub: u8,
    pub(crate) ty: Type,
    pub(crate) vstride: u8,
    pub(crate) width: u8,
    pub(crate) hstride: u8,
    pub(crate) swizzle: Swizzle,
    pub(crate) writemask: WriteMask,
    pub(crate) negate: bool,
    pub(crate) abs: bool,
}

/// GRF register `nr`: type F, region `<8,8,1>`, swizzle `xyzw`, all four
/// channels written, as Mesa's `elk_vec8_grf` starts a register.
pub const fn g(nr: u8) -> Reg {
    Reg::new(File::Grf, nr)
}

impl Reg {
    const fn new(file: File, nr: u8) -> Reg {
        Reg {
            file,
            nr,
            sub: 0,
            ty: Type::F,
            vstride: 8,
            width: 8,
            hstride: 1,
            swizzle: Swizzle::XYZW,
            writemask: WriteMask::XYZW,
            negate: false,
            abs: false,
        }
    }

    /// The null register: writes to it are dropped.
    pub const fn null() -> Reg {
        Reg::new(File::Arf, ARF_NULL)
    }

    /// The address register a0.
    pub const fn a0() -> Reg {
        Reg::new(File::Arf, ARF_ADDRESS)
    }

    /// The accumulator acc0.
    pub const fn acc0() -> Reg {
        Reg::new(File::Arf, ARF_ACCUMULATOR)
    }

    /// Flag register f0 as a whole; `.uw().sub(1)` is f0.1.
    pub const fn f0() -> Reg {
        Reg::new(File::Arf, ARF_FLAG)
    }

    /// Flag register f1.
    pub const fn f1() -> Reg {
        Reg::new(File::Arf, ARF_FLAG + 1)
    }

    /// The state register sr0 (EU and thread numbers, PRM Vol 4 Part 3,
    /// 3.3.3.7).
    pub const fn sr0() -> Reg {
        Reg::new(File::Arf, ARF_STATE)
    }

    /// The instruction pointer.
    pub const fn ip() -> Reg {
        Reg::new(File::Arf, ARF_IP)
    }

    pub const fn ty(mut self, ty: Type) -> Reg {
        self.ty = ty;
        self
    }

    pub const fn ud(self) -> Reg {
        self.ty(Type::UD)
    }

    pub const fn d(self) -> Reg {
        self.ty(Type::D)
    }

    pub const fn uw(self) -> Reg {
        self.ty(Type::UW)
    }

    pub const fn w(self) -> Reg {
        self.ty(Type::W)
    }

    pub const fn ub(self) -> Reg {
        self.ty(Type::UB)
    }

    pub const fn b(self) -> Reg {
        self.ty(Type::B)
    }

    pub const fn f(self) -> Reg {
        self.ty(Type::F)
    }

    /// Subregister, in elements of the operand's type.
    pub const fn sub(mut self, elements: u8) -> Reg {
        self.sub = elements;
        self
    }

    /// Source region `<vstride,width,hstride>`, in elements.
    pub const fn region(mut self, vstride: u8, width: u8, hstride: u8) -> Reg {
        self.vstride = vstride;
        self.width = width;
        self.hstride = hstride;
        self
    }

    /// One element for every channel: region `<0,1,0>` in Align1; swizzle
    /// `xxxx`, so replicated in Align16 and in three-source instructions.
    pub const fn scalar(self) -> Reg {
        self.region(0, 1, 0).swizzle(Swizzle::XXXX)
    }

    /// The Align16 source region `<4,4,1>`: four channels a group.
    pub const fn vec4(self) -> Reg {
        self.region(4, 4, 1)
    }

    /// Only the vertical stride; the one region field an Align16 source has.
    pub const fn vstride(mut self, vstride: u8) -> Reg {
        self.vstride = vstride;
        self
    }

    /// Destination horizontal stride, in elements.
    pub const fn stride(mut self, hstride: u8) -> Reg {
        self.hstride = hstride;
        self
    }

    pub const fn swizzle(mut self, swizzle: Swizzle) -> Reg {
        self.swizzle = swizzle;
        self
    }

    pub const fn writemask(mut self, mask: WriteMask) -> Reg {
        self.writemask = mask;
        self
    }

    /// Source modifier: negate.
    pub const fn negate(mut self) -> Reg {
        self.negate = true;
        self
    }

    /// Source modifier: absolute value, taken before any negation.
    pub const fn abs(mut self) -> Reg {
        self.abs = true;
        self
    }

    pub(crate) const fn is_null(&self) -> bool {
        matches!(self.file, File::Arf) && self.nr == ARF_NULL
    }

    pub(crate) const fn is_accumulator(&self) -> bool {
        matches!(self.file, File::Arf) && self.nr == ARF_ACCUMULATOR
    }

    pub(crate) const fn has_modifier(&self) -> bool {
        self.negate || self.abs
    }

    /// SrcMod: bit 0 absolute, bit 1 negate (PRM Vol 4 Part 3, 5.2.2).
    pub(crate) const fn modifier(&self) -> u32 {
        self.abs as u32 | (self.negate as u32) << 1
    }

    /// The subregister offset in bytes, checked to start inside the
    /// register.
    pub(crate) fn sub_bytes(&self) -> Result<u32, Error> {
        let bytes = self.sub as u32 * self.ty.size();
        if bytes >= REG_BYTES {
            return Err(Error::SubregOutOfRange);
        }
        Ok(bytes)
    }

    /// The register number, checked to exist.
    pub(crate) fn number(&self) -> Result<u32, Error> {
        if matches!(self.file, File::Grf) && self.nr >= GRF_COUNT {
            return Err(Error::RegisterOutOfRange);
        }
        Ok(self.nr as u32)
    }
}

/// A source operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    Reg(Reg),
    Imm(Imm),
}

impl From<Reg> for Src {
    fn from(reg: Reg) -> Src {
        Src::Reg(reg)
    }
}

impl From<Imm> for Src {
    fn from(imm: Imm) -> Src {
        Src::Imm(imm)
    }
}
