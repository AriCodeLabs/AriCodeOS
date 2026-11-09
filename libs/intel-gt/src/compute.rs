// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Compute dispatches on the render engine's GPGPU pipe (ADR 0086). The
//! kernel writes these commands into the render ring from a client's
//! [`Dispatch`]: every pointer in them is a PPGTT address or an offset from
//! one (PRM Vol 1 Part 2, 2.2: "all rendering operations ... target
//! per-process virtual memory"), and what could reach registers, the global
//! GTT or MMIO is forced off here: the VFE gateway's MMIO access, the
//! walker's indirect parameters and predicate. The flushes and their order
//! follow Linux `gen7_renderclear.c`; the commands follow Intel's PRM (Vol 2
//! Part 2, 1.8) and igt's `gen7_gpgpu_fillfunc` (`lib/gpu_cmds.c`), both MIT.

use aricode_abi::gpu::Dispatch;

use crate::render::{
    PC_CS_STALL, PC_DC_FLUSH, PC_DEPTH_FLUSH, PC_RENDER_TARGET_FLUSH, PC_STALL_AT_SCOREBOARD,
    PC_STATE_INVALIDATE, PIPE_CONTROL,
};

/// `PIPELINE_SELECT` of the GPGPU pipe.
pub const PIPELINE_SELECT_GPGPU: u32 = 0x3 << 29 | 0x1 << 27 | 0x1 << 24 | 0x4 << 16 | 2;
/// `STATE_BASE_ADDRESS`, ten dwords.
pub const STATE_BASE_ADDRESS: u32 = 0x3 << 29 | 0x1 << 24 | 0x1 << 16 | (10 - 2);
const BASE_MODIFY: u32 = 1;
const MEDIA: u32 = 0x3 << 29 | 0x2 << 27;
/// `MEDIA_VFE_STATE`, eight dwords.
pub const MEDIA_VFE_STATE: u32 = MEDIA | (8 - 2);
/// `MEDIA_CURBE_LOAD`, four dwords.
pub const MEDIA_CURBE_LOAD: u32 = MEDIA | 0x1 << 16 | (4 - 2);
/// `MEDIA_INTERFACE_DESCRIPTOR_LOAD`, four dwords.
pub const MEDIA_INTERFACE_DESCRIPTOR_LOAD: u32 = MEDIA | 0x2 << 16 | (4 - 2);
/// `MEDIA_STATE_FLUSH`, two dwords.
pub const MEDIA_STATE_FLUSH: u32 = MEDIA | 0x4 << 16;
/// `GPGPU_WALKER`, eleven dwords; bits 10 (indirect) and 8 (predicate) clear.
pub const GPGPU_WALKER: u32 = MEDIA | 0x1 << 24 | 0x5 << 16 | (11 - 2);
/// `MEDIA_VFE_STATE` dword 2: the GPGPU mode (Mesa `genxml/gen70.xml`).
const VFE_GPGPU_MODE: u32 = 1 << 2;
/// One interface descriptor's bytes.
pub const DESCRIPTOR_LEN: u32 = 32;
/// The URB's 256-bit entries for the media pipe, 32 of them holding
/// interface descriptors (PRM Vol 2 Part 2, 1.8.1 and 1.8.4).
const URB_ENTRIES: u32 = 2048;
const URB_DESCRIPTORS: u32 = 32;

/// Dwords [`emit`] writes at most.
pub const MAX_DWORDS: usize = 8 + 2 + 8 + 4 + 10 + 8 + 8 + 4 + 4 + 11 + 2;

/// Why a [`Dispatch`] cannot run: each is the client's mistake, which the
/// hardware would not take or would run wrongly. None of them is what keeps
/// the kernel safe: no field can name anything outside the client's PPGTT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    /// A base not 4 KiB-aligned.
    Base,
    /// A scratch pointer not 1 KiB-aligned, or a size code above 11.
    Scratch,
    /// No threads, or more than the GPU has.
    Threads,
    /// The URB entries, their size and the CURBE space do not fit the URB.
    Urb,
    /// CURBE data not 32-byte aligned, or longer than its space.
    Curbe,
    /// Descriptors not 32-byte aligned, none or more than 32, or the walker
    /// naming one past them.
    Descriptor,
    /// A SIMD width other than 8, 16 or 32.
    Simd,
    /// A group with an axis of no thread or more than 64, or too many
    /// threads in all.
    Group,
    /// An axis with no group, a start past its groups, or an empty mask.
    Groups,
}

/// The hardware threads of an Ivy Bridge GPU, by device ID: 6 EUs of 6
/// threads on GT1, 16 of 8 on GT2 (Linux `gen7_renderclear.c`,
/// `batch_get_defaults`; the devices of `i915_pciids.h`).
pub fn threads(device: u16) -> Option<u16> {
    match device {
        0x0152 | 0x0156 | 0x015A => Some(36),
        0x0162 | 0x0166 | 0x016A => Some(128),
        _ => None,
    }
}

/// Checks `d` for a GPU of `threads` hardware threads.
pub fn validate(d: &Dispatch, threads: u16) -> Result<(), Bad> {
    let bases = [
        d.general_base,
        d.surface_base,
        d.dynamic_base,
        d.instruction_base,
    ];
    if bases.iter().any(|b| b & 0xFFF != 0) {
        return Err(Bad::Base);
    }
    if d.scratch & 0x3F0 != 0 || d.scratch & 0xF > 11 {
        return Err(Bad::Scratch);
    }
    if d.max_threads == 0 || d.max_threads > threads {
        return Err(Bad::Threads);
    }
    let entries = d.urb_entries as u32;
    let entry = d.urb_entry_regs as u32;
    if entries > 64
        || (entries > 0 && entry == 0)
        || entry > 2048
        || entries * entry + d.curbe_regs as u32 + URB_DESCRIPTORS > URB_ENTRIES
    {
        return Err(Bad::Urb);
    }
    if !d.curbe_offset.is_multiple_of(32)
        || !d.curbe_len.is_multiple_of(32)
        || d.curbe_len > d.curbe_regs as u32 * 32
    {
        return Err(Bad::Curbe);
    }
    if !d.descriptor_offset.is_multiple_of(DESCRIPTOR_LEN)
        || d.descriptors == 0
        || d.descriptors as u32 > URB_DESCRIPTORS
        || d.descriptor >= d.descriptors
    {
        return Err(Bad::Descriptor);
    }
    let simd_code = match d.simd {
        8 => 0,
        16 => 1,
        32 => 2,
        _ => return Err(Bad::Simd),
    };
    let per_group: u32 = d.group.iter().map(|&n| n as u32).product();
    let most = if simd_code == 2 { 32 } else { 64 };
    if d.group.iter().any(|&n| n == 0 || n > 64) || per_group > most {
        return Err(Bad::Group);
    }
    if (0..3).any(|i| d.groups[i] == 0 || d.start[i] >= d.groups[i])
        || d.right_mask == 0
        || d.bottom_mask == 0
    {
        return Err(Bad::Groups);
    }
    Ok(())
}

/// The commands of `d` for a GPU of `threads` hardware threads, and how
/// many of the dwords they fill. The render request around them
/// ([`crate::render::request`]) flushes before and stores its number after
/// every thread has ended and its writes are in memory.
pub fn emit(d: &Dispatch, threads: u16) -> Result<([u32; MAX_DWORDS], usize), Bad> {
    validate(d, threads)?;
    let simd_code: u32 = match d.simd {
        8 => 0,
        16 => 1,
        _ => 2,
    };
    // Linux `gen7_emit_pipeline_invalidate`: Ivy Bridge stalls before a
    // state cache invalidation.
    const INVALIDATE: [u32; 8] = [
        PIPE_CONTROL,
        PC_STALL_AT_SCOREBOARD | PC_CS_STALL,
        0,
        0,
        PIPE_CONTROL,
        PC_STATE_INVALIDATE,
        0,
        0,
    ];
    // Linux `gen7_emit_pipeline_flush`.
    const FLUSH: [u32; 4] = [
        PIPE_CONTROL,
        PC_RENDER_TARGET_FLUSH | PC_DEPTH_FLUSH | PC_DC_FLUSH | PC_CS_STALL,
        0,
        0,
    ];
    let mut w = [0u32; MAX_DWORDS];
    let mut n = 0;
    let mut put = |words: &[u32]| {
        w[n..n + words.len()].copy_from_slice(words);
        n += words.len();
    };
    put(&INVALIDATE);
    put(&[PIPELINE_SELECT_GPGPU, crate::cmd::NOOP]);
    put(&INVALIDATE);
    put(&FLUSH);
    // MOCS 0 (igt's); the bounds as igt and Linux set them: 0 with the
    // modify bit for the dynamic and instruction ones, which the hardware
    // takes as no bound.
    put(&[
        STATE_BASE_ADDRESS,
        d.general_base | BASE_MODIFY,
        d.surface_base | BASE_MODIFY,
        d.dynamic_base | BASE_MODIFY,
        0,
        d.instruction_base | BASE_MODIFY,
        0,
        BASE_MODIFY,
        0,
        BASE_MODIFY,
    ]);
    put(&INVALIDATE);
    // Gateway MMIO access (bits 4:3) stays 0: no EU message reaches a
    // register.
    put(&[
        MEDIA_VFE_STATE,
        d.scratch,
        (d.max_threads as u32 - 1) << 16 | (d.urb_entries as u32) << 8 | VFE_GPGPU_MODE,
        0,
        (d.urb_entry_regs as u32).saturating_sub(1) << 16 | d.curbe_regs as u32,
        0,
        0,
        0,
    ]);
    if d.curbe_len > 0 {
        put(&[MEDIA_CURBE_LOAD, 0, d.curbe_len, d.curbe_offset]);
    }
    put(&[
        MEDIA_INTERFACE_DESCRIPTOR_LOAD,
        0,
        d.descriptors as u32 * DESCRIPTOR_LEN,
        d.descriptor_offset,
    ]);
    put(&[
        GPGPU_WALKER,
        d.descriptor as u32,
        simd_code << 30
            | (d.group[2] as u32 - 1) << 16
            | (d.group[1] as u32 - 1) << 8
            | (d.group[0] as u32 - 1),
        d.start[0],
        d.groups[0],
        d.start[1],
        d.groups[1],
        d.start[2],
        d.groups[2],
        d.right_mask,
        d.bottom_mask,
    ]);
    put(&[MEDIA_STATE_FLUSH, 0]);
    Ok((w, n))
}

/// An interface descriptor (PRM Vol 2 Part 2, 1.8.4), as a client writes it
/// in its memory: the EU code's offset from the instruction base (64-byte
/// aligned), the binding table's from the surface base (32-byte aligned,
/// below 64 KiB) and its entries to prefetch (at most 31), the CURBE
/// registers each thread reads, and the shared local memory (in 4 KiB
/// blocks: 0, 1, 2, 4, 8 or 16) and barrier a group needs. Single program
/// flow, IEEE floats, no samplers: what a compute kernel of this family runs
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Descriptor {
    pub kernel: u32,
    pub binding_table: u32,
    pub binding_entries: u8,
    pub curbe_regs: u16,
    pub slm_blocks: u8,
    pub barrier: bool,
}

impl Descriptor {
    /// `None` for a field the hardware cannot hold.
    pub fn encode(&self) -> Option<[u32; 8]> {
        if self.kernel & 0x3F != 0
            || self.binding_table & 0x1F != 0
            || self.binding_table > 0xFFE0
            || self.binding_entries > 31
            || self.curbe_regs > 63
            || !matches!(self.slm_blocks, 0 | 1 | 2 | 4 | 8 | 16)
        {
            return None;
        }
        Some([
            self.kernel,
            1 << 18,
            0,
            self.binding_table | self.binding_entries as u32,
            (self.curbe_regs as u32) << 16,
            (self.barrier as u32) << 21 | (self.slm_blocks as u32) << 16,
            0,
            0,
        ])
    }
}

/// `RENDER_SURFACE_STATE` of a linear 2D surface (Mesa `genxml/gen70.xml`):
/// the shape igt's fill writes through, with media block messages. `None`
/// for a size the fields cannot hold.
pub fn surface_2d(
    format: u32,
    address: u32,
    width: u32,
    height: u32,
    pitch: u32,
) -> Option<[u32; 8]> {
    const SURFTYPE_2D: u32 = 1;
    const RENDER_CACHE_READ_WRITE: u32 = 1 << 8;
    if !(1..=1 << 14).contains(&width)
        || !(1..=1 << 14).contains(&height)
        || !(1..=1 << 18).contains(&pitch)
        || format > 0x1FF
    {
        return None;
    }
    Some([
        SURFTYPE_2D << 29 | format << 18 | RENDER_CACHE_READ_WRITE,
        address,
        (height - 1) << 16 | (width - 1),
        pitch - 1,
        0,
        0,
        0,
        0,
    ])
}

/// Surface formats (Mesa `isl.h`).
pub const FORMAT_R8_UNORM: u32 = 0x140;
pub const FORMAT_R32_UINT: u32 = 0xD7;
