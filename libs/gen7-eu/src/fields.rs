// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Where each field sits in the 128-bit native instruction of gen7.
//!
//! Positions are bit numbers of the whole instruction: dword `n` holds bits
//! `32n + 31` to `32n`. Each one is taken from the field tables of Intel's
//! Ivy Bridge PRM, Vol 4 Part 3 ("PRM" below), section 5.2.1 (one and two
//! sources, three sources, send and branches) and the dword sections
//! 5.2.3 to 5.2.6, and checked against Mesa's `elk_inst.h` (MIT), whose
//! line is given as `elk_inst.h:N`. Where the two disagree the comment
//! says so.

/// A field: bits `hi` down to `lo`, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Field {
    hi: u8,
    lo: u8,
}

impl Field {
    const fn new(hi: u8, lo: u8) -> Self {
        Field { hi, lo }
    }

    const fn bit(at: u8) -> Self {
        Field { hi: at, lo: at }
    }

    /// The largest value the field holds.
    pub(crate) const fn max(self) -> u32 {
        let width = (self.hi - self.lo + 1) as u32;
        if width == 32 {
            u32::MAX
        } else {
            (1 << width) - 1
        }
    }
}

/// An instruction under construction: the 128 bits as one number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Bits(u128);

impl Bits {
    /// Writes `value` into `field`. The encoder checks every value against
    /// its field before writing it; a value that does not fit is a bug in
    /// the encoder, never in the program being assembled.
    pub(crate) fn set(&mut self, field: Field, value: u32) {
        debug_assert!(value <= field.max(), "{value:#x} does not fit {field:?}");
        let mask = (field.max() as u128) << field.lo;
        self.0 = (self.0 & !mask) | (((value as u128) << field.lo) & mask);
    }

    pub(crate) fn set_flag(&mut self, field: Field, on: bool) {
        self.set(field, on as u32);
    }

    /// The four dwords in the order the hardware fetches them.
    pub(crate) const fn words(self) -> [u32; 4] {
        [
            self.0 as u32,
            (self.0 >> 32) as u32,
            (self.0 >> 64) as u32,
            (self.0 >> 96) as u32,
        ]
    }
}

// --- Dword 0, the same for every format (PRM 5.2.3). ---

/// elk_inst.h:182.
pub(crate) const OPCODE: Field = Field::new(6, 0);
/// 0 is Align1, 1 is Align16. elk_inst.h:180.
pub(crate) const ACCESS_MODE: Field = Field::bit(8);
/// MaskCtrl: 1 ("NoMask", `WE_all`) writes channels the execution mask has
/// off. elk_inst.h:153.
pub(crate) const MASK_CONTROL: Field = Field::bit(9);
/// elk_inst.h:173.
pub(crate) const QTR_CONTROL: Field = Field::new(13, 12);
/// elk_inst.h:170.
pub(crate) const PRED_CONTROL: Field = Field::new(19, 16);
/// elk_inst.h:169.
pub(crate) const PRED_INV: Field = Field::bit(20);
/// elk_inst.h:168.
pub(crate) const EXEC_SIZE: Field = Field::new(23, 21);
/// CondModifier, or for `send` the target's shared function ID.
/// elk_inst.h:166 and 443 (`sfid`, 27:24 on gen6 to gen8).
pub(crate) const COND_MODIFIER: Field = Field::new(27, 24);
pub(crate) const SFID: Field = COND_MODIFIER;
/// elk_inst.h:159.
pub(crate) const SATURATE: Field = Field::bit(31);

// --- Dword 1 of one- and two-source instructions (PRM 5.2.4.1). ---

/// elk_inst.h:152.
pub(crate) const DST_FILE: Field = Field::new(33, 32);
/// elk_inst.h:151.
pub(crate) const DST_TYPE: Field = Field::new(36, 34);
/// Byte offset in Align1. elk_inst.h:146.
pub(crate) const DST_DA1_SUBREG: Field = Field::new(52, 48);
/// Bit 4 of the byte offset in Align16. elk_inst.h:145.
pub(crate) const DST_DA16_SUBREG: Field = Field::bit(52);
/// Dst.ChanEn in Align16. elk_inst.h:147.
pub(crate) const DST_WRITEMASK: Field = Field::new(51, 48);
/// elk_inst.h:144.
pub(crate) const DST_REG: Field = Field::new(60, 53);
/// elk_inst.h:142.
pub(crate) const DST_HSTRIDE: Field = Field::new(62, 61);

/// The fields of one register source of a one- or two-source instruction:
/// src0 in dword 2 (PRM 5.2.5.1), src1 in dword 3 (PRM 5.2.6), with their
/// file and type in dword 1.
pub(crate) struct SrcFields {
    pub file: Field,
    pub ty: Field,
    /// Byte offset in Align1.
    pub da1_subreg: Field,
    /// Bit 4 of the byte offset in Align16.
    pub da16_subreg: Field,
    /// ChanSel[3:0], channels x and y, in Align16.
    pub swizzle_lo: Field,
    pub reg: Field,
    /// SrcMod: bit 0 absolute value, bit 1 negate (PRM 5.2.2).
    pub modifier: Field,
    pub hstride: Field,
    /// ChanSel[7:4], channels z and w, in Align16; over HorzStride and the
    /// low bit of Width.
    pub swizzle_hi: Field,
    pub width: Field,
    pub vstride: Field,
}

/// elk_inst.h:125 to 149.
pub(crate) const SRC0: SrcFields = SrcFields {
    file: Field::new(38, 37),
    ty: Field::new(41, 39),
    da1_subreg: Field::new(68, 64),
    da16_subreg: Field::bit(68),
    swizzle_lo: Field::new(67, 64),
    reg: Field::new(76, 69),
    modifier: Field::new(78, 77),
    hstride: Field::new(81, 80),
    swizzle_hi: Field::new(83, 80),
    width: Field::new(84, 82),
    vstride: Field::new(88, 85),
};

/// elk_inst.h:107 to 124.
pub(crate) const SRC1: SrcFields = SrcFields {
    file: Field::new(43, 42),
    ty: Field::new(46, 44),
    da1_subreg: Field::new(100, 96),
    da16_subreg: Field::bit(100),
    swizzle_lo: Field::new(99, 96),
    reg: Field::new(108, 101),
    modifier: Field::new(110, 109),
    hstride: Field::new(113, 112),
    swizzle_hi: Field::new(115, 112),
    width: Field::new(116, 114),
    vstride: Field::new(120, 117),
};

/// FlagSubRegNum and, new on gen7, FlagRegNum: the flag register that both
/// the predicate and the conditional modifier use. elk_inst.h:154 to 158.
pub(crate) const FLAG_SUBREG: Field = Field::bit(89);
pub(crate) const FLAG_REG: Field = Field::bit(90);

/// The 32-bit immediate, which is always the last source. elk_inst.h, the
/// `imm_ud` accessors (bits 127:96).
pub(crate) const IMM32: Field = Field::new(127, 96);

// --- send (PRM 5.2.1, send column; PRM 6.56). ---

/// The message descriptor: 29 bits from gen5 on (elk_inst.h:367 to 377).
pub(crate) const SEND_DESC: Field = Field::new(124, 96);
/// End of thread. elk_inst.h:407.
pub(crate) const SEND_EOT: Field = Field::bit(127);

// --- Three-source instructions, Align16 only on gen7 (PRM 5.2.1). ---
//
// PRM 5.2.4.2 places the two type fields at dword 1 bits 16:15 and 14:13;
// its own table in 5.2.1 and Mesa put them at 45:44 and 43:42. Single
// precision float, the only type taken here, is 0 in both encodings, so the
// disagreement cannot change an encoded word.

/// elk_inst.h:215.
pub(crate) const T_FLAG_SUBREG: Field = Field::bit(33);
/// elk_inst.h:214.
pub(crate) const T_FLAG_REG: Field = Field::bit(34);
/// elk_inst.h:205.
pub(crate) const T_SRC_TYPE: Field = Field::new(43, 42);
/// elk_inst.h:204.
pub(crate) const T_DST_TYPE: Field = Field::new(45, 44);
/// elk_inst.h:202.
pub(crate) const T_DST_WRITEMASK: Field = Field::new(52, 49);
/// In dwords. elk_inst.h:201.
pub(crate) const T_DST_SUBREG: Field = Field::new(55, 53);
/// elk_inst.h:200.
pub(crate) const T_DST_REG: Field = Field::new(63, 56);

/// The fields of one source of a three-source instruction.
pub(crate) struct TernaryFields {
    /// Same encoding as SrcMod.
    pub modifier: Field,
    /// RepCtrl: 1 repeats the first channel into all of them.
    pub rep: Field,
    pub swizzle: Field,
    /// In dwords.
    pub subreg: Field,
    pub reg: Field,
}

/// elk_inst.h:188 to 211.
pub(crate) const T_SRC: [TernaryFields; 3] = [
    TernaryFields {
        modifier: Field::new(37, 36),
        rep: Field::bit(64),
        swizzle: Field::new(72, 65),
        subreg: Field::new(75, 73),
        reg: Field::new(83, 76),
    },
    TernaryFields {
        modifier: Field::new(39, 38),
        rep: Field::bit(85),
        swizzle: Field::new(93, 86),
        subreg: Field::new(96, 94),
        reg: Field::new(104, 97),
    },
    TernaryFields {
        modifier: Field::new(41, 40),
        rep: Field::bit(106),
        swizzle: Field::new(114, 107),
        subreg: Field::new(117, 115),
        reg: Field::new(125, 118),
    },
];
