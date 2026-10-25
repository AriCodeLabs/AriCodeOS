// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! PCI Express (section 30): everything that can be computed and decoded without
//! touching hardware. The kernel provides the configuration space reads.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Address of a PCI function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Address {
    /// Offset within the segment's ECAM region. The caller keeps `bus`
    /// within the segment's bus range (audit 2026-10-25 A, L4: a bridge
    /// naming a bus below it wrapped this offset).
    pub const fn ecam_offset(&self, bus_start: u8) -> u64 {
        debug_assert!(self.bus >= bus_start);
        (((self.bus - bus_start) as u64) << 20)
            | ((self.device as u64) << 15)
            | ((self.function as u64) << 12)
    }
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

/// Common header (first 64 bytes) of a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub vendor: u16,
    pub device: u16,
    pub command: u16,
    pub status: u16,
    pub revision: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class: u8,
    pub header_type: u8,
    pub multifunction: bool,
}

pub const VENDOR_NONE: u16 = 0xFFFF;
const STATUS_CAPABILITIES: u16 = 1 << 4;

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

impl Header {
    /// `None` if there is no function (vendor 0xFFFF) or the slice is short.
    pub fn parse(cfg: &[u8]) -> Option<Self> {
        if cfg.len() < 64 {
            return None;
        }
        let vendor = u16_at(cfg, 0);
        if vendor == VENDOR_NONE {
            return None;
        }
        Some(Self {
            vendor,
            device: u16_at(cfg, 2),
            command: u16_at(cfg, 4),
            status: u16_at(cfg, 6),
            revision: cfg[8],
            prog_if: cfg[9],
            subclass: cfg[10],
            class: cfg[11],
            header_type: cfg[14] & 0x7F,
            multifunction: cfg[14] & 0x80 != 0,
        })
    }

    pub fn has_capabilities(&self) -> bool {
        self.status & STATUS_CAPABILITIES != 0
    }

    /// Secondary bus of a PCI-PCI bridge (header type 1).
    pub fn secondary_bus(cfg: &[u8]) -> Option<u8> {
        (cfg.len() > 0x19 && cfg[14] & 0x7F == 1).then(|| cfg[0x19])
    }

    pub fn class_name(&self) -> &'static str {
        match (self.class, self.subclass) {
            (0x01, 0x01) => "IDE",
            (0x01, 0x06) => "SATA (AHCI)",
            (0x01, 0x08) => "NVMe",
            (0x01, _) => "storage",
            (0x02, 0x00) => "Ethernet",
            (0x02, _) => "network",
            (0x03, _) => "VGA/display",
            (0x04, _) => "multimedia",
            (0x06, 0x00) => "host bridge",
            (0x06, 0x01) => "ISA bridge",
            (0x06, 0x04) => "PCI-PCI bridge",
            (0x06, _) => "bridge",
            (0x0C, 0x03) => "USB",
            (0x0C, 0x05) => "SMBus",
            (0x0C, _) => "serial bus",
            (0x09, 0x00) => "keyboard",
            (0x09, 0x02) => "mouse",
            (0x09, _) => "input",
            _ => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Io {
        port: u32,
    },
    Memory {
        address: u64,
        prefetchable: bool,
        is_64: bool,
    },
}

/// Decodes the BARs of a type 0 (6) or type 1 (2) header.
pub fn bars(cfg: &[u8]) -> Vec<(usize, Bar)> {
    let count = if cfg.len() > 14 && cfg[14] & 0x7F == 1 {
        2
    } else {
        6
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < count && 0x10 + i * 4 + 4 <= cfg.len() {
        let raw = u32_at(cfg, 0x10 + i * 4);
        if raw == 0 {
            i += 1;
            continue;
        }
        if raw & 1 == 1 {
            out.push((i, Bar::Io { port: raw & !0x3 }));
            i += 1;
            continue;
        }
        let is_64 = (raw >> 1) & 0x3 == 0x2;
        let prefetchable = raw & 0x8 != 0;
        let mut address = (raw & !0xF) as u64;
        if is_64 {
            if 0x10 + (i + 1) * 4 + 4 > cfg.len() {
                break;
            }
            address |= (u32_at(cfg, 0x10 + (i + 1) * 4) as u64) << 32;
        }
        out.push((
            i,
            Bar::Memory {
                address,
                prefetchable,
                is_64,
            },
        ));
        i += if is_64 { 2 } else { 1 };
    }
    out
}

/// Size of a BAR from the value read back after writing all ones.
pub const fn bar_size_from_probe(probed: u32, is_io: bool) -> u32 {
    let mask = if is_io { !0x3u32 } else { !0xFu32 };
    (!(probed & mask)).wrapping_add(1)
}

/// A physical range a device's memory BAR must not touch: RAM, the
/// kernel's own MMIO, another device's BAR (ADR 0051).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forbidden {
    pub start: u64,
    /// Exclusive.
    pub end: u64,
    pub what: &'static str,
}

/// Why a memory BAR is not offered to a driver (ADR 0051).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarRefusal {
    /// Size 0, or below the 16 bytes a memory BAR decodes at least.
    Empty,
    /// The size a BAR decodes is always a power of two.
    NotPowerOfTwo,
    /// A BAR is aligned to its size.
    Misaligned,
    /// The range runs past the top of the address space.
    Wraps,
    /// It shares a page with a forbidden range: a driver maps whole pages.
    Overlaps(&'static str),
}

/// The pages a range spans: `[start, end)` rounded out to 4 KiB.
pub fn page_span(start: u64, len: u64) -> Option<(u64, u64)> {
    let end = start.checked_add(len)?;
    Some((start & !0xFFF, end.checked_next_multiple_of(0x1000)?))
}

/// Checks a memory BAR the device reports (`address`, `size` from the
/// sizing probe) before the kernel maps it or hands it to a driver: a
/// device must not choose where the CPU writes (audit 2026-10-25 A, H1).
/// Overlap is judged by whole pages, since that is what gets mapped.
pub fn check_bar(address: u64, size: u64, forbidden: &[Forbidden]) -> Result<(), BarRefusal> {
    if size < 16 {
        return Err(BarRefusal::Empty);
    }
    if !size.is_power_of_two() {
        return Err(BarRefusal::NotPowerOfTwo);
    }
    if address & (size - 1) != 0 {
        return Err(BarRefusal::Misaligned);
    }
    let (start, end) = page_span(address, size).ok_or(BarRefusal::Wraps)?;
    match forbidden.iter().find(|f| f.start < end && start < f.end) {
        Some(f) => Err(BarRefusal::Overlaps(f.what)),
        None => Ok(()),
    }
}

/// Whether MSI-X table entries `0..entries` (16 bytes each) at `offset`
/// lie within a BAR of `bar_size` bytes (audit 2026-10-25 A, H1: the
/// offset was never compared with the BAR).
pub fn msix_table_fits(offset: u64, entries: u64, bar_size: u64) -> bool {
    entries
        .checked_mul(16)
        .and_then(|len| offset.checked_add(len))
        .is_some_and(|end| end <= bar_size)
}

pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;
pub const CAP_PCIE: u8 = 0x10;
pub const CAP_VENDOR: u8 = 0x09;
/// Advanced Features capability (PCI Advanced Capabilities ECN): FLR for
/// conventional PCI functions.
pub const CAP_AF: u8 = 0x13;

/// Command register (offset 0x04) bits.
pub const COMMAND_MEMORY: u16 = 1 << 1;
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
pub const COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// How a function can be reset on its own (Function Level Reset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flr {
    /// PCI Express capability at this offset: Device Capabilities (+4)
    /// bit 28 advertises FLR, Device Control (+8) bit 15 starts it (PCIe
    /// Base Specification, 7.5.3.3 and 7.5.3.4).
    Pcie { cap: u8 },
    /// Advanced Features capability at this offset: AF Capabilities (+3)
    /// bit 1 advertises FLR, AF Control (+4) bit 0 starts it.
    AdvancedFeatures { cap: u8 },
}

impl Flr {
    /// Configuration offset of the 16-bit register that starts the reset.
    pub const fn control(self) -> u16 {
        match self {
            Flr::Pcie { cap } => cap as u16 + 8,
            Flr::AdvancedFeatures { cap } => cap as u16 + 4,
        }
    }

    /// Value to write to `control()` to start the reset, given what it
    /// holds now (`current`).
    pub const fn initiate(self, current: u16) -> u16 {
        match self {
            Flr::Pcie { .. } => current | 1 << 15,
            Flr::AdvancedFeatures { .. } => 1,
        }
    }

    /// Configuration byte and bit that say the function still has
    /// transactions pending, which an FLR would cut short: Transactions
    /// Pending in Device Status (PCIe 7.5.3.5, bit 5 of +0x0A) or TP in AF
    /// Status (+5, bit 0).
    pub const fn transactions_pending(self) -> (u16, u8) {
        match self {
            Flr::Pcie { cap } => (cap as u16 + 0x0A, 1 << 5),
            Flr::AdvancedFeatures { cap } => (cap as u16 + 5, 1),
        }
    }
}

/// PCI Express control registers an FLR resets that the kernel saves and
/// restores (16-bit, offsets from the PCIe capability): Device Control
/// (+8), and Device Control 2 (+0x28) from capability version 2 on. `caps`
/// is the PCIe Capabilities register (+2).
pub fn pcie_saved_controls(caps: u16) -> &'static [u16] {
    if caps & 0xF >= 2 {
        &[0x08, 0x28]
    } else {
        &[0x08]
    }
}

/// Device Control value to write back after an FLR: the saved one without
/// Initiate Function Level Reset (bit 15).
pub const fn device_control_after_flr(saved: u16) -> u16 {
    saved & !(1 << 15)
}

/// Milliseconds software waits after starting an FLR before it touches the
/// function again (PCIe Base Specification 6.6.2; the AF ECN uses the same
/// figure).
pub const FLR_WAIT_MS: u64 = 100;

/// Type 0 header an FLR clears that the kernel puts back, as read at
/// boot (the firmware's assignment, not whatever a dead driver left): the
/// six BARs (dwords 0x10-0x24) and the cache line size, latency timer and
/// interrupt line bytes (0x0C, 0x0D, 0x3C). Not the command register (the
/// claim writes it) nor BIST (0x0F: writing it may start a self-test).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedHeader {
    pub bars: [u32; 6],
    pub bytes: [u8; 3],
}

impl SavedHeader {
    pub const BAR_OFFSETS: [u8; 6] = [0x10, 0x14, 0x18, 0x1C, 0x20, 0x24];
    pub const BYTE_OFFSETS: [u8; 3] = [0x0C, 0x0D, 0x3C];

    /// From a function's configuration bytes (at least 0x40).
    pub fn from_config(cfg: &[u8]) -> Option<Self> {
        if cfg.len() < 0x40 {
            return None;
        }
        let mut bars = [0u32; 6];
        for (slot, &off) in bars.iter_mut().zip(Self::BAR_OFFSETS.iter()) {
            let o = off as usize;
            *slot = u32::from_le_bytes([cfg[o], cfg[o + 1], cfg[o + 2], cfg[o + 3]]);
        }
        let mut bytes = [0u8; 3];
        for (slot, &off) in bytes.iter_mut().zip(Self::BYTE_OFFSETS.iter()) {
            *slot = cfg[off as usize];
        }
        Some(Self { bars, bytes })
    }
}

/// FLR support advertised in a function's 256 configuration bytes: the PCI
/// Express capability first, the Advanced Features one otherwise.
pub fn flr_method(cfg: &[u8]) -> Option<Flr> {
    let caps = capabilities(cfg);
    let dword = |off: usize| {
        cfg.get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let pcie = caps.iter().find(|c| c.id == CAP_PCIE).and_then(|c| {
        let devcap = dword(c.offset as usize + 4)?;
        (devcap & 1 << 28 != 0).then_some(Flr::Pcie { cap: c.offset })
    });
    pcie.or_else(|| {
        caps.iter().find(|c| c.id == CAP_AF).and_then(|c| {
            let af_cap = *cfg.get(c.offset as usize + 3)?;
            (af_cap & 1 << 1 != 0).then_some(Flr::AdvancedFeatures { cap: c.offset })
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub id: u8,
    pub offset: u8,
}

/// Walks the capability list (loop tolerant).
pub fn capabilities(cfg: &[u8]) -> Vec<Capability> {
    let mut out = Vec::new();
    let Some(h) = Header::parse(cfg) else {
        return out;
    };
    if !h.has_capabilities() || cfg.len() < 0x35 {
        return out;
    }
    let mut ptr = cfg[0x34] & !0x3;
    let mut guard = 0;
    while ptr != 0 && (ptr as usize + 1) < cfg.len() && guard < 48 {
        out.push(Capability {
            id: cfg[ptr as usize],
            offset: ptr,
        });
        ptr = cfg[ptr as usize + 1] & !0x3;
        guard += 1;
    }
    out
}

pub fn cap_name(id: u8) -> &'static str {
    match id {
        CAP_MSI => "MSI",
        CAP_MSIX => "MSI-X",
        CAP_PCIE => "PCIe",
        CAP_VENDOR => "vendor",
        CAP_AF => "AF",
        0x01 => "PM",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_controller_class_names() {
        let mut h = Header {
            vendor: 0x1AF4,
            device: 0x1052,
            command: 0,
            status: 0,
            revision: 1,
            prog_if: 0,
            subclass: 0x00,
            class: 0x09,
            header_type: 0,
            multifunction: false,
        };
        assert_eq!(h.class_name(), "keyboard");
        h.subclass = 0x02;
        assert_eq!(h.class_name(), "mouse");
        h.subclass = 0x80;
        assert_eq!(h.class_name(), "input");
    }
    use alloc::vec;

    fn config(vendor: u16, device: u16, class: u8, sub: u8, header_type: u8) -> Vec<u8> {
        let mut c = vec![0u8; 256];
        c[0..2].copy_from_slice(&vendor.to_le_bytes());
        c[2..4].copy_from_slice(&device.to_le_bytes());
        c[9] = 0x01;
        c[10] = sub;
        c[11] = class;
        c[14] = header_type;
        c
    }

    #[test]
    fn ecam_offsets_and_display() {
        let a = Address {
            segment: 0,
            bus: 0,
            device: 0x1f,
            function: 2,
        };
        assert_eq!(a.ecam_offset(0), (0x1f << 15) | (2 << 12));
        let b = Address {
            segment: 0,
            bus: 3,
            device: 0,
            function: 0,
        };
        assert_eq!(b.ecam_offset(0), 3 << 20);
        assert_eq!(b.ecam_offset(2), 1 << 20);
        assert_eq!(alloc::format!("{a}"), "00:1f.2");
    }

    #[test]
    fn header_parse_and_absent_function() {
        let c = config(0x8086, 0x2922, 0x01, 0x06, 0x80);
        let h = Header::parse(&c).unwrap();
        assert_eq!((h.vendor, h.device), (0x8086, 0x2922));
        assert_eq!(h.class_name(), "SATA (AHCI)");
        assert!(h.multifunction && h.header_type == 0);
        assert!(!h.has_capabilities());
        let none = config(0xFFFF, 0, 0, 0, 0);
        assert!(Header::parse(&none).is_none());
        assert!(Header::parse(&c[..10]).is_none());
        let mut bridge = config(0x8086, 0x2918, 0x06, 0x04, 0x01);
        bridge[0x19] = 5;
        assert_eq!(Header::secondary_bus(&bridge), Some(5));
        assert_eq!(Header::secondary_bus(&c), None);
    }

    #[test]
    fn bar_decoding() {
        let mut c = config(0x1234, 0x1111, 0x03, 0x00, 0);
        c[0x10..0x14].copy_from_slice(&0xFD00_0008u32.to_le_bytes()); // mem32 prefetchable
        c[0x14..0x18].copy_from_slice(&0xC001u32.to_le_bytes()); // io
        c[0x18..0x1C].copy_from_slice(&0x0000_000Cu32.to_le_bytes()); // mem64 prefetchable, low
        c[0x1C..0x20].copy_from_slice(&0x0000_0008u32.to_le_bytes()); // high
        let b = bars(&c);
        assert_eq!(
            b[0],
            (
                0,
                Bar::Memory {
                    address: 0xFD00_0000,
                    prefetchable: true,
                    is_64: false
                }
            )
        );
        assert_eq!(b[1], (1, Bar::Io { port: 0xC000 }));
        assert_eq!(
            b[2],
            (
                2,
                Bar::Memory {
                    address: 0x8_0000_0000,
                    prefetchable: true,
                    is_64: true
                }
            )
        );
        assert_eq!(b.len(), 3);
        assert_eq!(bar_size_from_probe(0xFFFF_F000, false), 4096);
        assert_eq!(bar_size_from_probe(0xFFFF_FFE1, true), 32);
    }

    #[test]
    fn capability_list_walk_stops_on_loops() {
        let mut c = config(0x1AF4, 0x1000, 0x02, 0x00, 0);
        c[6] |= 0x10; // status: capabilities
        c[0x34] = 0x40;
        c[0x40] = CAP_MSIX;
        c[0x41] = 0x50;
        c[0x50] = CAP_MSI;
        c[0x51] = 0x40; // loop
        let caps = capabilities(&c);
        assert_eq!(caps.len(), 48, "the guard breaks the loop");
        assert_eq!(
            caps[0],
            Capability {
                id: CAP_MSIX,
                offset: 0x40
            }
        );
        assert_eq!(cap_name(CAP_MSI), "MSI");
        c[0x51] = 0;
        assert_eq!(capabilities(&c).len(), 2);
        c[6] &= !0x10;
        assert!(capabilities(&c).is_empty());
    }
    #[test]
    fn flr_is_found_in_the_pcie_or_af_capability() {
        let mut c = config(0x8086, 0x1E31, 0x0C, 0x03, 0);
        c[6] |= 0x10;
        c[0x34] = 0x50;
        c[0x50] = CAP_MSI;
        c[0x51] = 0x70;
        c[0x70] = CAP_PCIE;
        c[0x71] = 0x90;
        c[0x90] = CAP_AF;
        c[0x91] = 0;
        // Neither advertises FLR.
        assert_eq!(flr_method(&c), None);
        // AF: capabilities byte at +3; TP (bit 0) alone is not FLR (bit 1).
        c[0x93] = 0b01;
        assert_eq!(flr_method(&c), None);
        c[0x93] = 0b11;
        assert_eq!(flr_method(&c), Some(Flr::AdvancedFeatures { cap: 0x90 }));
        // PCIe Device Capabilities bit 28 wins over AF.
        c[0x74..0x78].copy_from_slice(&(1u32 << 28).to_le_bytes());
        assert_eq!(flr_method(&c), Some(Flr::Pcie { cap: 0x70 }));
        // Initiate: Device Control bit 15, keeping the other bits; AF Control bit 0.
        let pcie = Flr::Pcie { cap: 0x70 };
        assert_eq!((pcie.control(), pcie.initiate(0x2810)), (0x78, 0xA810));
        let af = Flr::AdvancedFeatures { cap: 0x90 };
        assert_eq!((af.control(), af.initiate(0)), (0x94, 1));
        // A truncated configuration never panics.
        assert_eq!(flr_method(&c[..0x75]), None);
        assert_eq!(cap_name(CAP_AF), "AF");
    }

    #[test]
    fn flr_restores_the_boot_header_but_not_command_or_bist() {
        let mut c = config(0x8086, 0x10C9, 0x02, 0x00, 0);
        c[0x04] = 0x07;
        c[0x0C] = 0x10;
        c[0x0D] = 0x20;
        c[0x0F] = 0x80; // BIST capable
        c[0x10..0x14].copy_from_slice(&0xFEB0_0000u32.to_le_bytes());
        c[0x24..0x28].copy_from_slice(&0xC001u32.to_le_bytes());
        c[0x3C] = 11;
        c[0x3D] = 1;
        let h = SavedHeader::from_config(&c).unwrap();
        assert_eq!(h.bars, [0xFEB0_0000, 0, 0, 0, 0, 0xC001]);
        assert_eq!(h.bytes, [0x10, 0x20, 11]);
        let touched: Vec<u8> = SavedHeader::BAR_OFFSETS
            .iter()
            .flat_map(|&o| o..o + 4)
            .chain(SavedHeader::BYTE_OFFSETS)
            .collect();
        for never in [0x04u8, 0x05, 0x06, 0x0E, 0x0F, 0x3D] {
            assert!(!touched.contains(&never), "{never:#x}");
        }
        assert_eq!(SavedHeader::from_config(&c[..0x3F]), None);
    }

    #[test]
    fn pcie_controls_and_pending_transactions() {
        assert_eq!(pcie_saved_controls(0x0001), &[0x08]);
        assert_eq!(pcie_saved_controls(0x0042), &[0x08, 0x28]);
        assert_eq!(device_control_after_flr(0xA810), 0x2810);
        assert_eq!(
            Flr::Pcie { cap: 0x70 }.transactions_pending(),
            (0x7A, 1 << 5)
        );
        assert_eq!(
            Flr::AdvancedFeatures { cap: 0x90 }.transactions_pending(),
            (0x95, 1)
        );
    }

    #[test]
    fn device_reported_bars_are_checked() {
        let ram = Forbidden {
            start: 0x10_0000,
            end: 0x8000_0000,
            what: "RAM",
        };
        let lapic = Forbidden {
            start: 0xFEE0_0000,
            end: 0xFEE0_1000,
            what: "local APIC",
        };
        let f = [ram, lapic];
        assert_eq!(check_bar(0xC000_0000, 0x1000, &f), Ok(()));
        assert_eq!(check_bar(0x80_0000_0000, 0x4000, &f), Ok(()));
        // A 1 KiB BAR alone in its page (EHCI on the reference laptop).
        assert_eq!(check_bar(0xF7E0_0400, 0x400, &f), Ok(()));
        assert_eq!(
            check_bar(0x1000_0000, 0x1000, &f),
            Err(BarRefusal::Overlaps("RAM"))
        );
        // Sharing the page is enough.
        assert_eq!(
            check_bar(0x7FFF_FF00, 0x100, &f),
            Err(BarRefusal::Overlaps("RAM"))
        );
        assert_eq!(
            check_bar(0xFEE0_0000, 0x1000, &f),
            Err(BarRefusal::Overlaps("local APIC"))
        );
        assert_eq!(check_bar(0xC000_0000, 0, &f), Err(BarRefusal::Empty));
        assert_eq!(
            check_bar(0xC000_0000, 0x3000, &f),
            Err(BarRefusal::NotPowerOfTwo)
        );
        assert_eq!(
            check_bar(0xC000_1000, 0x4000, &f),
            Err(BarRefusal::Misaligned)
        );
        assert_eq!(
            check_bar(0xFFFF_FFFF_FFFF_F000, 0x1000, &f),
            Err(BarRefusal::Wraps)
        );
    }

    #[test]
    fn msix_table_bounds() {
        assert!(msix_table_fits(0x2000, 1, 0x4000));
        assert!(msix_table_fits(0x3FF0, 1, 0x4000));
        assert!(!msix_table_fits(0x3FF8, 1, 0x4000));
        // An offset of up to 4 GiB past a 16 KiB BAR (the audit's case).
        assert!(!msix_table_fits(0xFFFF_FFF8, 1, 0x4000));
        assert!(!msix_table_fits(u64::MAX - 8, 1, u64::MAX));
        assert!(!msix_table_fits(0, u64::MAX, 0x4000));
    }
}
