// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Messages for `send`: the shared function they go to and their
//! descriptor.
//!
//! The descriptor layout common to all messages is in PRM Vol 4 Part 3,
//! 6.56; the data port messages in Vol 4 Part 1, 3.9; the thread spawner
//! in Vol 2 Part 2, 1.7.2. Mesa's `elk_eu_defines.h` (MIT) gives the same
//! numbers, cited by name.

use crate::Error;

/// Shared function IDs (Mesa `elk_eu_defines.h`: `GFX6_SFID_*`,
/// `ELK_SFID_THREAD_SPAWNER`, `GFX7_SFID_DATAPORT_DATA_CACHE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sfid {
    /// The data port through the render cache: media block messages.
    RenderCache = 5,
    /// The thread spawner, where a GPGPU thread ends.
    ThreadSpawner = 7,
    /// The data port through the data cache: OWord block and untyped
    /// surface messages.
    DataCache = 10,
}

// Message descriptor (PRM Vol 4 Part 3, 6.56).
const MLEN_SHIFT: u32 = 25;
const RLEN_SHIFT: u32 = 20;
const HEADER_PRESENT: u32 = 1 << 19;
/// Message length: 1 to 15 registers.
const MLEN_MAX: u8 = 15;
/// Response length: 0 to 16 registers.
const RLEN_MAX: u8 = 16;

// Data port function control (PRM Vol 4 Part 1, 3.9.2.1.1). Bit 18 of the
// data cache's descriptor picks the scratch messages; it stays 0 here.
const DP_MSG_TYPE_SHIFT: u32 = 14;
const DP_MSG_CONTROL_SHIFT: u32 = 8;

// Data cache message types (PRM Vol 4 Part 1, 3.9.2.1.1;
// `GFX7_DATAPORT_DC_*`).
const DC_OWORD_BLOCK_READ: u32 = 0;
const DC_UNTYPED_SURFACE_READ: u32 = 5;
const DC_OWORD_BLOCK_WRITE: u32 = 8;
const DC_UNTYPED_SURFACE_WRITE: u32 = 13;

// Render cache message types (same section; `GFX7_DATAPORT_RC_*`).
const RC_MEDIA_BLOCK_READ: u32 = 4;
const RC_MEDIA_BLOCK_WRITE: u32 = 10;

/// Binding table index 254 is shared local memory, which the OWord block
/// messages cannot reach (PRM Vol 4 Part 1, 3.9.2.1.1). 255, stateless,
/// they can.
const BTI_SLM: u8 = 254;

/// Thread spawner function control (PRM Vol 2 Part 2, 1.7.2.1): opcode 0
/// "dereference resource" ends the thread, requester 0 is a root thread,
/// and resource select 1 leaves the URB handle alone, as a GPGPU thread
/// has none.
const TS_RESOURCE_SELECT_NO_URB: u32 = 1 << 4;

/// How many OWords (16 bytes) an OWord block message moves, and the
/// "Block Size" code for it (PRM Vol 4 Part 1, 3.9.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OWords {
    /// One OWord, in the low half of the register.
    OneLow,
    /// One OWord, in the high half of the register.
    OneHigh,
    Two,
    Four,
    Eight,
}

impl OWords {
    const fn code(self) -> u32 {
        match self {
            OWords::OneLow => 0,
            OWords::OneHigh => 1,
            OWords::Two => 2,
            OWords::Four => 3,
            OWords::Eight => 4,
        }
    }

    /// Registers the data fills: two OWords a register.
    const fn regs(self) -> u8 {
        match self {
            OWords::OneLow | OWords::OneHigh | OWords::Two => 1,
            OWords::Four => 2,
            OWords::Eight => 4,
        }
    }
}

/// SIMD mode of an untyped surface message (PRM Vol 4 Part 1, 3.9.9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Simd {
    Simd8,
    Simd16,
}

impl Simd {
    const fn code(self) -> u32 {
        match self {
            Simd::Simd16 => 1,
            Simd::Simd8 => 2,
        }
    }

    /// Registers one 32-bit value per channel takes.
    const fn regs(self) -> u8 {
        match self {
            Simd::Simd8 => 1,
            Simd::Simd16 => 2,
        }
    }
}

/// Channels (dwords) an untyped surface message moves at each address:
/// red, then green, blue and alpha.
const UNTYPED_CHANNELS_MAX: u8 = 4;
/// The four channel mask bits, 1 meaning "not included" (PRM Vol 4 Part 1,
/// 3.9.9.2).
const UNTYPED_MASK_ALL: u32 = 0xF;

/// A message for `send`: where it goes and its descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub(crate) sfid: Sfid,
    pub(crate) mlen: u8,
    pub(crate) rlen: u8,
    header: bool,
    /// Function control, bits 18:0 of the descriptor.
    function: u32,
    pub(crate) eot: bool,
    pub(crate) desc_d: bool,
}

impl Message {
    const fn new(sfid: Sfid, mlen: u8, rlen: u8, header: bool, function: u32) -> Message {
        Message {
            sfid,
            mlen,
            rlen,
            header,
            function,
            eot: false,
            desc_d: false,
        }
    }

    const fn data_port(msg_type: u32, control: u32, bti: u8) -> u32 {
        msg_type << DP_MSG_TYPE_SHIFT | control << DP_MSG_CONTROL_SHIFT | bti as u32
    }

    /// Reads `size` OWords of a buffer through the data cache, at the
    /// OWord offset in dword 2 of the header (PRM Vol 4 Part 1, 3.9.3). The
    /// payload is the header alone.
    pub fn oword_block_read(bti: u8, size: OWords) -> Result<Message, Error> {
        if bti == BTI_SLM {
            return Err(Error::MessageParameter);
        }
        let function = Message::data_port(DC_OWORD_BLOCK_READ, size.code(), bti);
        Ok(Message::new(
            Sfid::DataCache,
            1,
            size.regs(),
            true,
            function,
        ))
    }

    /// Writes `size` OWords: the header, then the data.
    pub fn oword_block_write(bti: u8, size: OWords) -> Result<Message, Error> {
        if bti == BTI_SLM {
            return Err(Error::MessageParameter);
        }
        let function = Message::data_port(DC_OWORD_BLOCK_WRITE, size.code(), bti);
        Ok(Message::new(
            Sfid::DataCache,
            1 + size.regs(),
            0,
            true,
            function,
        ))
    }

    /// The untyped mask: the low `channels` channels included, the rest
    /// masked. Writes take only such prefixes (PRM Vol 4 Part 1, 3.9.9.2);
    /// reads are given the same, as they are what a kernel uses.
    fn untyped_control(simd: Simd, channels: u8) -> Result<u32, Error> {
        if channels == 0 || channels > UNTYPED_CHANNELS_MAX {
            return Err(Error::MessageParameter);
        }
        let masked = (UNTYPED_MASK_ALL << channels) & UNTYPED_MASK_ALL;
        Ok(simd.code() << 4 | masked)
    }

    /// Reads `channels` dwords at each channel's byte address, from a raw
    /// buffer (PRM Vol 4 Part 1, 3.9.9). The payload is the addresses, no
    /// header; the response is one register (SIMD8) or two (SIMD16) a
    /// channel.
    pub fn untyped_surface_read(bti: u8, simd: Simd, channels: u8) -> Result<Message, Error> {
        let control = Message::untyped_control(simd, channels)?;
        let function = Message::data_port(DC_UNTYPED_SURFACE_READ, control, bti);
        let rlen = channels * simd.regs();
        Ok(Message::new(
            Sfid::DataCache,
            simd.regs(),
            rlen,
            false,
            function,
        ))
    }

    /// Writes `channels` dwords at each channel's byte address. The payload
    /// is the addresses, then the data of each channel.
    pub fn untyped_surface_write(bti: u8, simd: Simd, channels: u8) -> Result<Message, Error> {
        let control = Message::untyped_control(simd, channels)?;
        let function = Message::data_port(DC_UNTYPED_SURFACE_WRITE, control, bti);
        let mlen = (1 + channels) * simd.regs();
        Ok(Message::new(Sfid::DataCache, mlen, 0, false, function))
    }

    /// Reads a block of a 2D surface through the render cache into `rlen`
    /// registers; the header gives its place and size (PRM Vol 4 Part 1,
    /// 3.9.6).
    pub fn media_block_read(bti: u8, rlen: u8) -> Result<Message, Error> {
        if rlen == 0 || rlen > RLEN_MAX {
            return Err(Error::MessageLength);
        }
        let function = Message::data_port(RC_MEDIA_BLOCK_READ, 0, bti);
        Ok(Message::new(Sfid::RenderCache, 1, rlen, true, function))
    }

    /// Writes a block of a 2D surface: the header, then `data_regs`
    /// registers of data.
    pub fn media_block_write(bti: u8, data_regs: u8) -> Result<Message, Error> {
        if data_regs == 0 || data_regs >= MLEN_MAX {
            return Err(Error::MessageLength);
        }
        let function = Message::data_port(RC_MEDIA_BLOCK_WRITE, 0, bti);
        Ok(Message::new(
            Sfid::RenderCache,
            1 + data_regs,
            0,
            true,
            function,
        ))
    }

    /// Ends a GPGPU root thread: a "dereference resource" message to the
    /// thread spawner with end of thread set. The payload is one register,
    /// a copy of g0 (PRM Vol 2 Part 2, 1.7.2).
    pub const fn thread_spawner_eot() -> Message {
        let mut msg = Message::new(Sfid::ThreadSpawner, 1, 0, false, TS_RESOURCE_SELECT_NO_URB);
        msg.eot = true;
        msg
    }

    /// Gives the descriptor immediate type D instead of UD, as
    /// intel-gen4asm wrote igt's kernels; the hardware takes the bits
    /// either way.
    pub const fn desc_as_d(mut self) -> Message {
        self.desc_d = true;
        self
    }

    /// The 29-bit descriptor (PRM Vol 4 Part 3, 6.56). The constructors
    /// keep every part in range.
    pub(crate) const fn descriptor(&self) -> u32 {
        (self.mlen as u32) << MLEN_SHIFT
            | (self.rlen as u32) << RLEN_SHIFT
            | if self.header { HEADER_PRESENT } else { 0 }
            | self.function
    }
}
