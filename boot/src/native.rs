// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The panel's own size when the firmware does not offer it (ADR 0079): the
//! hardware's side of `aricode_intel_display`, which decides what is done.
//!
//! The firmware's page tables map BAR 0, since its display driver uses it. The
//! registers are written after ExitBootServices, when no firmware console is
//! left to draw with the old geometry.

use aricode_intel_display::{self as intel, Plan, Refusal, Regs, Request};
use boot_protocol::FramebufferInfo;
use core::arch::asm;

const PCI_ADDRESS: u16 = 0xCF8;
const PCI_DATA: u16 = 0xCFC;
const VENDOR_INTEL: u32 = 0x8086;
const CLASS_DISPLAY_VGA: u32 = 0x0300;
const COMMAND_MEMORY: u32 = 1 << 1;
const BAR_64BIT: u32 = 0b0100;

/// Reads a dword of the configuration space of 00:02.0, where Intel's
/// integrated graphics always is, through the ports every x86 chipset decodes.
fn config_read(offset: u8) -> u32 {
    let address = 0x8000_0000u32 | 2 << 11 | (offset as u32 & 0xFC);
    let value: u32;
    // SAFETY: the PCI configuration mechanism; a read has no side effects on
    // these registers, and nothing else uses the ports now.
    unsafe {
        asm!("out dx, eax", in("dx") PCI_ADDRESS, in("eax") address, options(nomem, nostack));
        asm!("in eax, dx", in("dx") PCI_DATA, out("eax") value, options(nomem, nostack));
    }
    value
}

fn bar(offset: u8) -> Option<u64> {
    let low = config_read(offset);
    if low & 1 != 0 || low & 0b0110 != BAR_64BIT {
        return None;
    }
    let address = (low & !0xF) as u64 | (config_read(offset + 4) as u64) << 32;
    (address != 0).then_some(address)
}

struct Bar0(u64);

impl Regs for Bar0 {
    fn read(&self, offset: u32) -> u32 {
        // SAFETY: inside BAR 0, whose size `plan` knows; mapped by the firmware.
        unsafe { ((self.0 + offset as u64) as *const u32).read_volatile() }
    }

    fn write(&mut self, offset: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe { ((self.0 + offset as u64) as *mut u32).write_volatile(value) }
    }
}

/// A change decided, not yet made.
pub struct Planned {
    bar0: u64,
    plan: Plan,
    /// The screen once it is made.
    pub framebuffer: FramebufferInfo,
}

/// Decides whether this machine can show `panel`, reading only.
pub fn plan(
    framebuffer: &FramebufferInfo,
    panel: (u32, u32),
    max_bytes: u64,
) -> Result<Planned, Refusal> {
    let id = config_read(0);
    if id & 0xFFFF != VENDOR_INTEL || config_read(8) >> 16 != CLASS_DISPLAY_VGA {
        return Err(Refusal::NotThisFamily);
    }
    let (bar0, aperture) = match (bar(0x10), bar(0x18)) {
        (Some(b), Some(a)) => (b, a),
        _ => return Err(Refusal::BadBars),
    };
    let aperture_size =
        intel::aperture_size((config_read(0x60) >> 16) as u8).ok_or(Refusal::BadBars)?;
    if config_read(4) & COMMAND_MEMORY == 0
        || !bar0.is_multiple_of(intel::BAR0_SIZE)
        || framebuffer.address < aperture
        || framebuffer.address >= aperture.saturating_add(aperture_size)
    {
        return Err(Refusal::BadBars);
    }
    let plan = intel::plan(
        &Bar0(bar0),
        &Request {
            device: (id >> 16) as u16,
            panel,
            firmware: (framebuffer.width, framebuffer.height),
            firmware_stride: framebuffer.stride,
            surface: framebuffer.address - aperture,
            aperture_size,
            max_bytes,
        },
    )?;
    Ok(Planned {
        bar0,
        plan,
        framebuffer: FramebufferInfo {
            size: plan.bytes,
            width: plan.width,
            height: plan.height,
            stride: plan.stride,
            ..*framebuffer
        },
    })
}

/// Blanks the new screen and makes the change. No log or allocator here:
/// this runs after ExitBootServices.
pub fn apply(planned: &Planned) -> Result<(), Refusal> {
    let f = &planned.framebuffer;
    for pixel in 0..f.size / 4 {
        // SAFETY: inside the framebuffer: `plan` checked the aperture and the
        // firmware's GTT for all of these bytes.
        unsafe {
            (f.address as *mut u32)
                .add(pixel as usize)
                .write_volatile(0)
        };
    }
    intel::apply(&mut Bar0(planned.bar0), &planned.plan)
}
