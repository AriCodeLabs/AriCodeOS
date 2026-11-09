// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Why an instruction was refused. "PRM" is Intel's Ivy Bridge PRM, Vol 4
//! Part 3 unless another part is named.

/// An instruction the encoder will not write, and the rule it breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The caller's buffer has no room for another instruction.
    BufferFull,
    /// A GRF register past g127 (PRM 3.3.2).
    RegisterOutOfRange,
    /// A subregister that starts past the 32 bytes of its register.
    SubregOutOfRange,
    /// A vertical stride, width or horizontal stride with no encoding
    /// (PRM 5.2.2).
    RegionEncoding,
    /// A source region that breaks the regioning rules of PRM 3.3.9,
    /// part 2 (A to F, H).
    RegionRule,
    /// An operand that reaches past the two registers it may span (PRM
    /// 3.3.9, 3A and 3B).
    RegionSpan,
    /// A destination horizontal stride of 0, or one with no encoding (PRM
    /// 3.3.9, 2G).
    DstStride,
    /// A packed byte destination outside `mov` (PRM 3.3.9, 4A).
    PackedByteDst,
    /// An Align16 operand that does not start at byte 0 or 16 of its
    /// register: the field holds only bit 4 (PRM 5.2.4.1, 5.2.5.1).
    Align16Subreg,
    /// An Align16 source with a vertical stride other than 0 or 4, the
    /// only region it has (PRM 5.2.1, three-source table).
    Align16Region,
    /// Align16 with 16 channels of 32-bit data (PRM 3.3.9, 7A).
    Align16Simd16,
    /// An Align16 GRF destination with no channel enabled.
    EmptyWriteMask,
    /// A predicate control other than the plain one in Align16, whose
    /// encodings differ from Align1's (PRM 5.2.3).
    PredicateMode,
    /// A quarter control that does not fit the execution size (PRM 5.2.3,
    /// QtrCtrl).
    QuarterControl,
    /// A predicate and a conditional modifier on different flag
    /// registers; the instruction has one flag field (PRM 5.2.5.1).
    FlagConflict,
    /// A conditional modifier on an instruction without one (PRM 6, the
    /// table of each instruction; for `send` the field holds the SFID).
    CondModNotAllowed,
    /// Saturation on an instruction without it (PRM 6).
    SaturateNotAllowed,
    /// A source modifier where none is taken: on a destination or on the
    /// payload of `send` (PRM 6.56).
    SourceModifier,
    /// The accumulator as the second source: it is a src0 operand only
    /// (PRM 3.3.3.5).
    AccumulatorNotSrc0,
    /// An operand in a register file the instruction cannot use: `send`
    /// and three-source instructions take GRF registers only (PRM 5.2,
    /// 6.56).
    File,
    /// An operand type the instruction does not take (PRM 6): logic and
    /// shift instructions take integers, `mad` takes float.
    Type,
    /// `send` with end of thread whose payload is below g112 (PRM 6.56).
    EotPayload,
    /// `send` with end of thread that asks for a response (PRM 6.56).
    EotResponse,
    /// A response with nowhere to land: its destination is not a GRF
    /// register, or the response runs past g127 (PRM 6.56).
    SendResponse,
    /// A message length out of 1 to 15 or a response length over 16, or a
    /// payload that runs past g127 (PRM 6.56).
    MessageLength,
    /// A message parameter the data port does not take (PRM Vol 4 Part 1,
    /// 3.9).
    MessageParameter,
    /// A jump farther than the 16-bit jump field holds (PRM 6.34, 6.62).
    JumpOutOfRange,
}
