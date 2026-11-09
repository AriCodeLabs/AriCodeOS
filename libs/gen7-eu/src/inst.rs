// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Instructions and their encoding into the 128-bit native format.
//!
//! An [`Inst`] is built with a constructor per opcode and the modifiers
//! (predicate, conditional modifier, saturation, quarter, mask, access
//! mode); [`Inst::encode`] checks it and writes the four dwords. "PRM" is
//! Intel's Ivy Bridge PRM, Vol 4 Part 3.

use crate::fields::{self, Bits, Field, SrcFields};
use crate::reg::{File, Imm, ImmType, Reg, Src, Type, FILE_IMM, GRF_COUNT, REG_BYTES};
use crate::send::Message;
use crate::Error;

/// Opcodes (PRM 5.4; Mesa `elk_eu.c`, the gen7 opcode table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opcode {
    Mov = 0x01,
    And = 0x05,
    Or = 0x06,
    Shr = 0x08,
    Shl = 0x09,
    Cmp = 0x10,
    Jmpi = 0x20,
    While = 0x27,
    Send = 0x31,
    Sendc = 0x32,
    Add = 0x40,
    Mul = 0x41,
    Mad = 0x5B,
    Nop = 0x7E,
}

/// What an opcode takes, from the "Pred Sat Cond Mod Src Mod" table of each
/// instruction in PRM 6 (6.4 add, 6.6 and, 6.17 cmp, 6.34 jmpi, 6.40 mad,
/// 6.41.10 mov, 6.43 mul, 6.46 or, 6.56 send, 6.57 sendc, 6.58 shl, 6.59
/// shr, 6.62 while).
struct Caps {
    saturate: bool,
    cond_modifier: bool,
    /// Logic and shift instructions work on integers only.
    integer_only: bool,
}

impl Opcode {
    const fn caps(self) -> Caps {
        let (saturate, cond_modifier, integer_only) = match self {
            Opcode::Mov | Opcode::Add | Opcode::Mul | Opcode::Mad => (true, true, false),
            Opcode::Shl | Opcode::Shr => (true, true, true),
            Opcode::And | Opcode::Or => (false, true, true),
            Opcode::Cmp => (false, true, false),
            Opcode::Jmpi | Opcode::While | Opcode::Send | Opcode::Sendc | Opcode::Nop => {
                (false, false, false)
            }
        };
        Caps {
            saturate,
            cond_modifier,
            integer_only,
        }
    }
}

/// Channels the instruction runs (PRM 5.2.3, ExecSize).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecSize {
    E1,
    E2,
    E4,
    E8,
    E16,
}

impl ExecSize {
    const fn code(self) -> u32 {
        match self {
            ExecSize::E1 => 0,
            ExecSize::E2 => 1,
            ExecSize::E4 => 2,
            ExecSize::E8 => 3,
            ExecSize::E16 => 4,
        }
    }

    pub const fn channels(self) -> u32 {
        1 << self.code()
    }
}

/// Quarter control: which part of the execution mask the instruction uses
/// (PRM 5.2.3, QtrCtrl). `Q1` to `Q4` are the four groups of 8 channels,
/// for up to 8 channels; `H1` and `H2` the two halves of 16 channels, for
/// SIMD16 instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qtr {
    Q1,
    Q2,
    Q3,
    Q4,
    H1,
    H2,
}

impl Qtr {
    const fn code(self) -> u32 {
        match self {
            Qtr::Q1 | Qtr::H1 => 0,
            Qtr::Q2 => 1,
            Qtr::Q3 | Qtr::H2 => 2,
            Qtr::Q4 => 3,
        }
    }

    const fn fits(self, exec: ExecSize) -> bool {
        match self {
            Qtr::Q1 | Qtr::H1 => true,
            Qtr::Q2 | Qtr::Q3 | Qtr::Q4 => exec.channels() <= 8,
            Qtr::H2 => exec.channels() == 16,
        }
    }
}

/// Conditional modifiers (PRM 5.2.3; Mesa `elk_eu_defines.h`,
/// `ELK_CONDITIONAL_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondMod {
    /// Zero, or equal for `cmp`.
    Z = 1,
    /// Not zero, or not equal.
    NZ = 2,
    G = 3,
    GE = 4,
    L = 5,
    LE = 6,
    /// Overflow.
    O = 8,
    /// Unordered (NaN).
    U = 9,
}

/// How the predicate combines flag bits (PRM 5.2.3, PredCtrl). `Normal`
/// uses each channel's own bit; the others, Align1 only, combine
/// neighbouring channels' bits, as SIMD branches need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredCtrl {
    Normal = 1,
    AnyV = 2,
    AllV = 3,
    Any2H = 4,
    All2H = 5,
    Any4H = 6,
    All4H = 7,
    Any8H = 8,
    All8H = 9,
    Any16H = 10,
    All16H = 11,
}

/// A predicate: run only the channels whose flag bit says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pred {
    flag: crate::Flag,
    ctrl: PredCtrl,
    inverse: bool,
}

impl Pred {
    /// `(+f)`: channels whose bit in `flag` is set.
    pub const fn new(flag: crate::Flag) -> Pred {
        Pred {
            flag,
            ctrl: PredCtrl::Normal,
            inverse: false,
        }
    }

    /// `(-f)`: channels whose bit is clear.
    pub const fn inverse(mut self) -> Pred {
        self.inverse = true;
        self
    }

    pub const fn ctrl(mut self, ctrl: PredCtrl) -> Pred {
        self.ctrl = ctrl;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Form {
    Unary {
        dst: Reg,
        src: Src,
    },
    Binary {
        dst: Reg,
        src0: Reg,
        src1: Src,
    },
    Ternary {
        dst: Reg,
        src: [Reg; 3],
    },
    Send {
        dst: Reg,
        payload: Reg,
        msg: Message,
    },
    Nop,
}

/// One instruction, not yet encoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inst {
    op: Opcode,
    exec: ExecSize,
    align16: bool,
    no_mask: bool,
    qtr: Qtr,
    pred: Option<Pred>,
    cmod: Option<(CondMod, crate::Flag)>,
    sat: bool,
    form: Form,
}

/// Branch distances are counted in 64-bit units, two per native
/// instruction (PRM 6.34, 6.62; Mesa `elk_jump_scale` gives 2 from gen5 to
/// gen7).
pub(crate) const JUMP_UNITS_PER_INST: i32 = 2;

impl Inst {
    const fn new(op: Opcode, exec: ExecSize, form: Form) -> Inst {
        Inst {
            op,
            exec,
            align16: false,
            no_mask: false,
            qtr: Qtr::Q1,
            pred: None,
            cmod: None,
            sat: false,
            form,
        }
    }

    const fn unary(op: Opcode, exec: ExecSize, dst: Reg, src: Src) -> Inst {
        Inst::new(op, exec, Form::Unary { dst, src })
    }

    const fn binary(op: Opcode, exec: ExecSize, dst: Reg, src0: Reg, src1: Src) -> Inst {
        Inst::new(op, exec, Form::Binary { dst, src0, src1 })
    }

    pub fn mov(exec: ExecSize, dst: Reg, src: impl Into<Src>) -> Inst {
        Inst::unary(Opcode::Mov, exec, dst, src.into())
    }

    pub fn add(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::Add, exec, dst, src0, src1.into())
    }

    /// With two dword integer sources only the low 16 bits of `src1` count
    /// (PRM 6.43).
    pub fn mul(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::Mul, exec, dst, src0, src1.into())
    }

    pub fn and(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::And, exec, dst, src0, src1.into())
    }

    pub fn or(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::Or, exec, dst, src0, src1.into())
    }

    pub fn shl(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::Shl, exec, dst, src0, src1.into())
    }

    pub fn shr(exec: ExecSize, dst: Reg, src0: Reg, src1: impl Into<Src>) -> Inst {
        Inst::binary(Opcode::Shr, exec, dst, src0, src1.into())
    }

    /// Compares `src0` with `src1` and writes the result into `flag`.
    pub fn cmp(
        exec: ExecSize,
        cmod: CondMod,
        flag: crate::Flag,
        dst: Reg,
        src0: Reg,
        src1: impl Into<Src>,
    ) -> Inst {
        Inst::binary(Opcode::Cmp, exec, dst, src0, src1.into()).cmod(cmod, flag)
    }

    /// `dst = src0 + src1 * src2`, in float. A three-source instruction:
    /// Align16 only on gen7, GRF operands, and at most 8 channels of 32-bit
    /// data (PRM 3.3.9, 7A). A source given as [`Reg::scalar`] is
    /// replicated to every channel.
    pub fn mad(exec: ExecSize, dst: Reg, src0: Reg, src1: Reg, src2: Reg) -> Inst {
        let src = [src0, src1, src2];
        Inst::new(Opcode::Mad, exec, Form::Ternary { dst, src }).align16()
    }

    /// Sends `msg` with its payload starting at `payload`; a response lands
    /// at `dst`.
    pub fn send(exec: ExecSize, dst: Reg, payload: Reg, msg: Message) -> Inst {
        Inst::new(Opcode::Send, exec, Form::Send { dst, payload, msg })
    }

    /// `send` that first waits for the thread dependencies in `tdr` to
    /// clear (PRM 6.57).
    pub fn sendc(exec: ExecSize, dst: Reg, payload: Reg, msg: Message) -> Inst {
        Inst::new(Opcode::Sendc, exec, Form::Send { dst, payload, msg })
    }

    pub(crate) const fn nop() -> Inst {
        Inst::new(Opcode::Nop, ExecSize::E1, Form::Nop)
    }

    /// `jmpi` by `units` past the next instruction (PRM 6.34): one channel,
    /// NoMask, `ip` as destination and first source, the distance as a D
    /// immediate.
    pub(crate) const fn jmpi(pred: Option<Pred>, units: i16) -> Inst {
        let ip = Reg::ip().ud();
        let mut inst = Inst::binary(
            Opcode::Jmpi,
            ExecSize::E1,
            ip,
            ip.scalar(),
            Src::Imm(jmpi_index(units)),
        );
        inst.no_mask = true;
        inst.pred = pred;
        inst
    }

    /// `while` back by `units` from itself (PRM 6.62), laid out as Mesa's
    /// `elk_WHILE` does for gen7: null D destination and first source, and
    /// the JIP in the low word of a W immediate whose high word, UIP,
    /// stays 0 (JIP is bits 111:96, UIP 127:112: PRM 5.2.1, branch column;
    /// elk_inst.h:274 to 313).
    pub(crate) const fn while_(exec: ExecSize, pred: Option<Pred>, units: i16) -> Inst {
        let null = Reg::null().d();
        // The null source keeps a region valid for the execution size.
        let src0 = if exec.channels() >= 8 {
            null
        } else {
            null.scalar()
        };
        let jip = Imm {
            bits: units as u16 as u32,
            ty: ImmType::W,
        };
        let mut inst = Inst::binary(Opcode::While, exec, null, src0, Src::Imm(jip));
        inst.pred = pred;
        inst
    }

    pub const fn pred(mut self, pred: Pred) -> Inst {
        self.pred = Some(pred);
        self
    }

    /// Sets `flag` from the result.
    pub const fn cmod(mut self, cmod: CondMod, flag: crate::Flag) -> Inst {
        self.cmod = Some((cmod, flag));
        self
    }

    pub const fn sat(mut self) -> Inst {
        self.sat = true;
        self
    }

    pub const fn qtr(mut self, qtr: Qtr) -> Inst {
        self.qtr = qtr;
        self
    }

    /// Writes every channel, whatever the execution mask (`WE_all`).
    pub const fn no_mask(mut self) -> Inst {
        self.no_mask = true;
        self
    }

    /// 16-byte aligned operands with swizzles and write masks.
    pub const fn align16(mut self) -> Inst {
        self.align16 = true;
        self
    }

    /// Checks the instruction and encodes it.
    pub fn encode(&self) -> Result<[u32; 4], Error> {
        let mut bits = Bits::default();
        self.encode_controls(&mut bits)?;
        match self.form {
            Form::Unary { dst, src } => {
                self.encode_dst(&mut bits, &dst)?;
                match src {
                    Src::Reg(reg) => self.encode_src(&mut bits, &fields::SRC0, &reg)?,
                    Src::Imm(imm) => {
                        self.encode_imm(&mut bits, &fields::SRC0, imm)?;
                        // As Mesa's `elk_set_src0` does, the absent src1
                        // repeats the immediate's type, with the ARF file.
                        bits.set(fields::SRC1.ty, imm.ty.code());
                    }
                }
                Ok(())
            }
            Form::Binary { dst, src0, src1 } => {
                self.encode_dst(&mut bits, &dst)?;
                self.encode_src(&mut bits, &fields::SRC0, &src0)?;
                match src1 {
                    Src::Reg(reg) if reg.is_accumulator() => return Err(Error::AccumulatorNotSrc0),
                    Src::Reg(reg) => self.encode_src(&mut bits, &fields::SRC1, &reg)?,
                    Src::Imm(imm) => self.encode_imm(&mut bits, &fields::SRC1, imm)?,
                }
                Ok(())
            }
            Form::Ternary { dst, src } => self.encode_ternary(&mut bits, &dst, &src),
            Form::Send { dst, payload, msg } => self.encode_send(&mut bits, &dst, &payload, &msg),
            Form::Nop => Ok(()),
        }?;
        // One flag register serves the predicate and the conditional
        // modifier; three-source instructions keep it in dword 1.
        if matches!(self.form, Form::Ternary { .. }) {
            self.flag_field(&mut bits, fields::T_FLAG_REG, fields::T_FLAG_SUBREG);
        } else {
            self.flag_field(&mut bits, fields::FLAG_REG, fields::FLAG_SUBREG);
        }
        Ok(bits.words())
    }

    /// Dword 0 (PRM 5.2.3), and the checks that belong to it.
    fn encode_controls(&self, bits: &mut Bits) -> Result<(), Error> {
        let caps = self.op.caps();
        if self.sat && !caps.saturate {
            return Err(Error::SaturateNotAllowed);
        }
        if self.cmod.is_some() && !caps.cond_modifier {
            return Err(Error::CondModNotAllowed);
        }
        if let (Some(pred), Some((_, flag))) = (self.pred, self.cmod) {
            if pred.flag != flag {
                return Err(Error::FlagConflict);
            }
        }
        if !self.qtr.fits(self.exec) {
            return Err(Error::QuarterControl);
        }
        bits.set(fields::OPCODE, self.op as u32);
        bits.set_flag(fields::ACCESS_MODE, self.align16);
        bits.set_flag(fields::MASK_CONTROL, self.no_mask);
        bits.set(fields::QTR_CONTROL, self.qtr.code());
        if let Some(pred) = self.pred {
            if self.align16 && pred.ctrl != PredCtrl::Normal {
                return Err(Error::PredicateMode);
            }
            bits.set(fields::PRED_CONTROL, pred.ctrl as u32);
            bits.set_flag(fields::PRED_INV, pred.inverse);
        }
        bits.set(fields::EXEC_SIZE, self.exec.code());
        if let Some((cmod, _)) = self.cmod {
            bits.set(fields::COND_MODIFIER, cmod as u32);
        }
        bits.set_flag(fields::SATURATE, self.sat);
        Ok(())
    }

    /// The flag register of the predicate or of the conditional modifier.
    fn flag_field(&self, bits: &mut Bits, reg: Field, subreg: Field) {
        let flag = match (self.pred, self.cmod) {
            (Some(pred), _) => pred.flag,
            (None, Some((_, flag))) => flag,
            (None, None) => return,
        };
        bits.set(reg, flag.reg());
        bits.set(subreg, flag.subreg());
    }

    /// Operand type checks of PRM 6: logic and shifts take integers.
    fn check_type(&self, integer: bool) -> Result<(), Error> {
        if self.op.caps().integer_only && !integer {
            return Err(Error::Type);
        }
        Ok(())
    }

    /// Align16 forbids SIMD16 on 32-bit data (PRM 3.3.9, 7A).
    fn check_align16_width(&self, size: u32) -> Result<(), Error> {
        if self.align16 && self.exec == ExecSize::E16 && size == 4 {
            return Err(Error::Align16Simd16);
        }
        Ok(())
    }

    /// The destination of a one- or two-source instruction (PRM 5.2.4.1).
    fn encode_dst(&self, bits: &mut Bits, dst: &Reg) -> Result<(), Error> {
        if dst.has_modifier() {
            return Err(Error::SourceModifier);
        }
        self.check_type(dst.ty.is_integer())?;
        self.check_align16_width(dst.ty.size())?;
        let sub = dst.sub_bytes()?;
        bits.set(fields::DST_FILE, dst.file.code());
        bits.set(fields::DST_TYPE, dst.ty.code());
        bits.set(fields::DST_REG, dst.number()?);
        if self.align16 {
            bits.set(fields::DST_DA16_SUBREG, align16_offset(sub)?);
            if matches!(dst.file, File::Grf) && dst.writemask.0 == 0 {
                return Err(Error::EmptyWriteMask);
            }
            bits.set(fields::DST_WRITEMASK, dst.writemask.0 as u32);
            // "HW needs this to be programmed as 01" (PRM 5.2.4.1).
            bits.set(fields::DST_HSTRIDE, 1);
            return Ok(());
        }
        let stride = match hstride_code(dst.hstride) {
            Some(code) if dst.hstride != 0 => code,
            _ => return Err(Error::DstStride),
        };
        // A single byte is no packed region: Mesa's gen7 code writes
        // `or(1) g113.21<1>UB`.
        let packed_bytes = dst.ty.size() == 1 && dst.hstride == 1 && self.exec != ExecSize::E1;
        if packed_bytes && self.op != Opcode::Mov {
            return Err(Error::PackedByteDst);
        }
        let last = sub + ((self.exec.channels() - 1) * dst.hstride as u32 + 1) * dst.ty.size();
        if last > 2 * REG_BYTES {
            return Err(Error::RegionSpan);
        }
        bits.set(fields::DST_DA1_SUBREG, sub);
        bits.set(fields::DST_HSTRIDE, stride);
        Ok(())
    }

    /// A register source of a one- or two-source instruction (PRM 5.2.5.1,
    /// 5.2.6).
    fn encode_src(&self, bits: &mut Bits, f: &SrcFields, src: &Reg) -> Result<(), Error> {
        self.check_type(src.ty.is_integer())?;
        self.check_align16_width(src.ty.size())?;
        let sub = src.sub_bytes()?;
        bits.set(f.file, src.file.code());
        bits.set(f.ty, src.ty.code());
        bits.set(f.reg, src.number()?);
        bits.set(f.modifier, src.modifier());
        let vstride = vstride_code(src.vstride).ok_or(Error::RegionEncoding)?;
        if self.align16 {
            if src.vstride != 0 && src.vstride != ALIGN16_VSTRIDE {
                return Err(Error::Align16Region);
            }
            bits.set(f.da16_subreg, align16_offset(sub)?);
            bits.set(f.swizzle_lo, (src.swizzle.0 & 0xF) as u32);
            bits.set(f.swizzle_hi, (src.swizzle.0 >> 4) as u32);
            bits.set(f.vstride, vstride);
            return Ok(());
        }
        let width = width_code(src.width).ok_or(Error::RegionEncoding)?;
        let hstride = hstride_code(src.hstride).ok_or(Error::RegionEncoding)?;
        check_region(self.exec.channels(), sub, src)?;
        bits.set(f.da1_subreg, sub);
        bits.set(f.vstride, vstride);
        bits.set(f.width, width);
        bits.set(f.hstride, hstride);
        Ok(())
    }

    /// An immediate, which fills dword 3 (PRM 5.2.2, Imm32).
    fn encode_imm(&self, bits: &mut Bits, f: &SrcFields, imm: Imm) -> Result<(), Error> {
        self.check_type(imm.ty.is_integer())?;
        bits.set(f.file, FILE_IMM);
        bits.set(f.ty, imm.ty.code());
        bits.set(fields::IMM32, imm.bits);
        Ok(())
    }

    /// The three-source format (PRM 5.2.1 and 5.2.4.2).
    fn encode_ternary(&self, bits: &mut Bits, dst: &Reg, src: &[Reg; 3]) -> Result<(), Error> {
        // Every operand is a float in the GRF.
        for reg in core::iter::once(dst).chain(src) {
            if reg.file != File::Grf {
                return Err(Error::File);
            }
            if reg.ty != Type::F {
                return Err(Error::Type);
            }
        }
        self.check_align16_width(Type::F.size())?;
        if dst.has_modifier() {
            return Err(Error::SourceModifier);
        }
        if dst.writemask.0 == 0 {
            return Err(Error::EmptyWriteMask);
        }
        bits.set(fields::T_DST_REG, dst.number()?);
        bits.set(fields::T_DST_SUBREG, dst.sub_bytes()? / DWORD);
        bits.set(fields::T_DST_WRITEMASK, dst.writemask.0 as u32);
        bits.set(fields::T_DST_TYPE, THREE_SRC_TYPE_F);
        bits.set(fields::T_SRC_TYPE, THREE_SRC_TYPE_F);
        for (reg, f) in src.iter().zip(&fields::T_SRC) {
            // No region: a vertical stride of 0 replicates the first
            // channel, 4 is a plain vec4 (Mesa `elk_alu3`).
            let rep = match reg.vstride {
                0 => true,
                ALIGN16_VSTRIDE => false,
                _ => return Err(Error::Align16Region),
            };
            bits.set(f.reg, reg.number()?);
            bits.set(f.subreg, reg.sub_bytes()? / DWORD);
            bits.set(f.swizzle, reg.swizzle.0 as u32);
            bits.set_flag(f.rep, rep);
            bits.set(f.modifier, reg.modifier());
        }
        Ok(())
    }

    /// `send` and `sendc` (PRM 5.2.1 send column, 6.56).
    fn encode_send(
        &self,
        bits: &mut Bits,
        dst: &Reg,
        payload: &Reg,
        msg: &Message,
    ) -> Result<(), Error> {
        if payload.file != File::Grf {
            return Err(Error::File);
        }
        if payload.has_modifier() {
            return Err(Error::SourceModifier);
        }
        if payload.number()? + msg.mlen as u32 > GRF_COUNT as u32 {
            return Err(Error::MessageLength);
        }
        if msg.eot {
            // The next thread may load into g0 to g111 while this message
            // is still pending; and nothing may answer a thread that is gone.
            if payload.nr < EOT_PAYLOAD_FIRST {
                return Err(Error::EotPayload);
            }
            if !dst.is_null() || msg.rlen != 0 {
                return Err(Error::EotResponse);
            }
        }
        if msg.rlen > 0
            && (dst.file != File::Grf || dst.number()? + msg.rlen as u32 > GRF_COUNT as u32)
        {
            return Err(Error::SendResponse);
        }
        // Without a response the destination only shapes the channel
        // enables (PRM 6.56), so any register is accepted: igt's gen7 fill
        // kernel names acc0 and Linux's clear kernel names g5 there.
        self.encode_dst(bits, dst)?;
        self.encode_src(bits, &fields::SRC0, payload)?;
        bits.set(fields::SFID, msg.sfid as u32);
        bits.set(fields::SRC1.file, FILE_IMM);
        let desc_type = if msg.desc_d { ImmType::D } else { ImmType::UD };
        bits.set(fields::SRC1.ty, desc_type.code());
        bits.set(fields::SEND_DESC, msg.descriptor());
        bits.set_flag(fields::SEND_EOT, msg.eot);
        Ok(())
    }
}

/// The jump distance of a branch to `target` from `at`, checked against the
/// 16-bit field (PRM 6.34: "Format = S15" in bits 15:0).
pub(crate) fn jump_units(at: usize, target: usize) -> Result<i16, Error> {
    let delta = target as i64 - at as i64;
    i16::try_from(delta * JUMP_UNITS_PER_INST as i64).map_err(|_| Error::JumpOutOfRange)
}

/// Writes a branch distance into an encoded `jmpi`, as [`Inst::jmpi`]
/// writes its immediate (dword 3).
pub(crate) fn patch_jmpi(words: &mut [u32; 4], units: i16) {
    words[3] = jmpi_index(units).bits;
}

/// The index of `jmpi`, a D immediate. PRM 6.34 marks bits 31:16 reserved,
/// but Linux's clear kernel sign-extends a backward distance through them,
/// as a D immediate is; so does this.
const fn jmpi_index(units: i16) -> Imm {
    Imm::d(units as i32)
}

/// Three-source type code of single precision float (PRM 5.2.4.2).
const THREE_SRC_TYPE_F: u32 = 0;
/// Three-source subregisters count dwords.
const DWORD: u32 = 4;
/// The vertical stride of an Align16 vec4 source.
const ALIGN16_VSTRIDE: u8 = 4;
/// Bytes per Align16 subregister step: the field is bit 4 of the offset.
const ALIGN16_STEP: u32 = 16;
/// "The send with EOT should use register space R112-R127 for <src>"
/// (PRM 6.56).
const EOT_PAYLOAD_FIRST: u8 = 112;

fn align16_offset(sub_bytes: u32) -> Result<u32, Error> {
    if !sub_bytes.is_multiple_of(ALIGN16_STEP) {
        return Err(Error::Align16Subreg);
    }
    Ok(sub_bytes / ALIGN16_STEP)
}

/// VertStride encoding (PRM 5.2.2): 0 and powers of two up to 32.
const fn vstride_code(v: u8) -> Option<u32> {
    match v {
        0 => Some(0),
        1 => Some(1),
        2 => Some(2),
        4 => Some(3),
        8 => Some(4),
        16 => Some(5),
        32 => Some(6),
        _ => None,
    }
}

/// Width encoding (PRM 5.2.2): 1 to 16.
const fn width_code(w: u8) -> Option<u32> {
    match w {
        1 => Some(0),
        2 => Some(1),
        4 => Some(2),
        8 => Some(3),
        16 => Some(4),
        _ => None,
    }
}

/// HorzStride encoding (PRM 5.2.2): 0, 1, 2 or 4.
const fn hstride_code(h: u8) -> Option<u32> {
    match h {
        0 => Some(0),
        1 => Some(1),
        2 => Some(2),
        4 => Some(3),
        _ => None,
    }
}

/// The regioning rules of PRM 3.3.9 part 2 for an Align1 source, and rule
/// 3A on how far it reaches.
fn check_region(exec: u32, sub: u32, src: &Reg) -> Result<(), Error> {
    let (v, w, h) = (src.vstride as u32, src.width as u32, src.hstride as u32);
    // A: the width never exceeds the execution size.
    if w > exec {
        return Err(Error::RegionRule);
    }
    // B: one full-width row with a stride: the next row follows on.
    if exec == w && h != 0 && v != w * h {
        return Err(Error::RegionRule);
    }
    // D, and E as its case: a width of 1 has no horizontal stride.
    if w == 1 && h != 0 {
        return Err(Error::RegionRule);
    }
    if exec == 1 && v != 0 {
        return Err(Error::RegionRule);
    }
    // F: with no stride at all there is only one column.
    if v == 0 && h == 0 && w != 1 {
        return Err(Error::RegionRule);
    }
    // H: a row stays inside one register; 3A: the whole region inside two.
    let size = src.ty.size();
    for row in 0..exec / w {
        let first = sub + row * v * size;
        let last = first + (w - 1) * h * size + size - 1;
        if first / REG_BYTES != last / REG_BYTES {
            return Err(Error::RegionRule);
        }
        if last >= 2 * REG_BYTES {
            return Err(Error::RegionSpan);
        }
    }
    Ok(())
}
