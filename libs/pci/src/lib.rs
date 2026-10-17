// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
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
    /// Offset within the segment's ECAM region.
    pub const fn ecam_offset(&self, bus_start: u8) -> u64 {
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

pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;
pub const CAP_PCIE: u8 = 0x10;
pub const CAP_VENDOR: u8 = 0x09;

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
}
