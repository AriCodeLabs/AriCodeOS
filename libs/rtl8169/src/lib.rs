//! Realtek r8169 family NICs (RTL8169, 8168, 8101/8102/8136...):
//! register map, 16-byte descriptors and bring-up sequence, as documented
//! by the Linux `r8169` driver. No I/O of its own: the driver supplies
//! register access through [`Regs`], so everything is tested on the
//! host with a simulated register file (ADR 0024).

#![no_std]

/// Register offsets (MMIO, first memory BAR).
pub mod reg {
    pub const MAC0: u64 = 0x00;
    pub const MAR0: u64 = 0x08;
    pub const TX_DESC_LO: u64 = 0x20;
    pub const TX_DESC_HI: u64 = 0x24;
    pub const CHIP_CMD: u64 = 0x37;
    pub const TX_POLL: u64 = 0x38;
    pub const INTR_MASK: u64 = 0x3C;
    pub const INTR_STATUS: u64 = 0x3E;
    pub const TX_CONFIG: u64 = 0x40;
    pub const RX_CONFIG: u64 = 0x44;
    pub const RX_MISSED: u64 = 0x4C;
    pub const CFG9346: u64 = 0x50;
    pub const MULTI_INTR: u64 = 0x5C;
    pub const PHY_STATUS: u64 = 0x6C;
    pub const RX_MAX_SIZE: u64 = 0xDA;
    pub const CPLUS_CMD: u64 = 0xE0;
    pub const INTR_MITIGATE: u64 = 0xE2;
    pub const RX_DESC_LO: u64 = 0xE4;
    pub const RX_DESC_HI: u64 = 0xE8;
    pub const EARLY_TX_THRESHOLD: u64 = 0xEC;
}

/// `CHIP_CMD` bits.
pub mod cmd {
    pub const RESET: u8 = 0x10;
    pub const RX_ENABLE: u8 = 0x08;
    pub const TX_ENABLE: u8 = 0x04;
}

/// `TX_POLL`: there are new descriptors in the normal queue.
pub const TX_POLL_NPQ: u8 = 0x40;

/// `INTR_STATUS`/`INTR_MASK` bits.
pub mod intr {
    pub const RX_OK: u16 = 0x0001;
    pub const RX_ERR: u16 = 0x0002;
    pub const TX_OK: u16 = 0x0004;
    pub const TX_ERR: u16 = 0x0008;
    pub const RX_OVERFLOW: u16 = 0x0010;
    pub const LINK_CHANGE: u16 = 0x0020;
    pub const RX_FIFO_OVER: u16 = 0x0040;
    pub const TX_DESC_UNAVAILABLE: u16 = 0x0080;
    pub const SYS_ERR: u16 = 0x8000;
    /// What the driver handles.
    pub const ENABLED: u16 =
        RX_OK | RX_ERR | TX_OK | TX_ERR | RX_OVERFLOW | LINK_CHANGE | RX_FIFO_OVER | SYS_ERR;
}

pub const CFG9346_UNLOCK: u8 = 0xC0;
pub const CFG9346_LOCK: u8 = 0x00;

/// `RX_CONFIG`: no FIFO threshold, 1024-byte DMA bursts, and accept
/// broadcast, multicast and our own address.
pub const RX_CONFIG: u32 = (7 << 13) | (6 << 8) | 0x08 | 0x04 | 0x02;
/// `TX_CONFIG`: 1024-byte DMA bursts and normal interframe gap.
pub const TX_CONFIG: u32 = (6 << 8) | (3 << 24);
/// `CPLUS_CMD`: PCI multiple read/write and hardware receive
/// checksum (unused, but it is what Linux enables).
pub const CPLUS_CMD: u16 = (1 << 3) | (1 << 5);
pub const EARLY_TX_THRESHOLD: u8 = 0x3F;

/// `PHY_STATUS` bits.
pub mod phy {
    pub const FULL_DUPLEX: u8 = 0x01;
    pub const LINK: u8 = 0x02;
    pub const SPEED_10: u8 = 0x04;
    pub const SPEED_100: u8 = 0x08;
    pub const SPEED_1000: u8 = 0x10;
}

/// Link state derived from `PHY_STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkState {
    pub up: bool,
    pub mbps: u16,
    pub full_duplex: bool,
}

pub fn link_state(phy_status: u8) -> LinkState {
    LinkState {
        up: phy_status & phy::LINK != 0,
        mbps: if phy_status & phy::SPEED_1000 != 0 {
            1000
        } else if phy_status & phy::SPEED_100 != 0 {
            100
        } else if phy_status & phy::SPEED_10 != 0 {
            10
        } else {
            0
        },
        full_duplex: phy_status & phy::FULL_DUPLEX != 0,
    }
}

/// Receive or transmit descriptor (16 bytes, little endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Desc {
    pub opts1: u32,
    pub opts2: u32,
    pub addr: u64,
}

pub mod desc {
    /// The card owns the descriptor.
    pub const OWN: u32 = 1 << 31;
    /// Last in the ring: the card wraps back to the first one.
    pub const EOR: u32 = 1 << 30;
    pub const FIRST: u32 = 1 << 29;
    pub const LAST: u32 = 1 << 28;
    /// Receive error (summary).
    pub const RX_ERROR: u32 = 1 << 21;
    pub const LEN_MASK: u32 = 0x3FFF;
}

impl Desc {
    pub const LEN: usize = 16;

    /// Armed receive descriptor: a `len`-byte buffer for the card.
    pub fn rx(buffer_phys: u64, len: u16, last: bool) -> Self {
        Self {
            opts1: desc::OWN | (len as u32 & desc::LEN_MASK) | if last { desc::EOR } else { 0 },
            opts2: 0,
            addr: buffer_phys,
        }
    }

    /// Transmit descriptor for a complete frame of `len` bytes.
    pub fn tx(buffer_phys: u64, len: u16, last: bool) -> Self {
        Self {
            opts1: desc::OWN
                | desc::FIRST
                | desc::LAST
                | (len as u32 & desc::LEN_MASK)
                | if last { desc::EOR } else { 0 },
            opts2: 0,
            addr: buffer_phys,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..4].copy_from_slice(&self.opts1.to_le_bytes());
        b[4..8].copy_from_slice(&self.opts2.to_le_bytes());
        b[8..].copy_from_slice(&self.addr.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; Self::LEN]) -> Self {
        Self {
            opts1: u32::from_le_bytes(b[..4].try_into().unwrap()),
            opts2: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            addr: u64::from_le_bytes(b[8..].try_into().unwrap()),
        }
    }
}

/// Received frame according to `opts1` of a receive descriptor: `None` if the
/// card still owns it, `Some(Err(()))` if it arrived with an error and `Some(Ok(n))`
/// with the useful bytes (without the 4-byte CRC the card appends).
pub fn rx_frame(opts1: u32) -> Option<Result<usize, ()>> {
    if opts1 & desc::OWN != 0 {
        return None;
    }
    if opts1 & desc::RX_ERROR != 0 {
        return Some(Err(()));
    }
    Some(Ok(((opts1 & desc::LEN_MASK) as usize).saturating_sub(4)))
}

/// Access to the card registers (MMIO in the driver, memory in the
/// tests).
pub trait Regs {
    fn r8(&self, off: u64) -> u8;
    fn r16(&self, off: u64) -> u16;
    fn r32(&self, off: u64) -> u32;
    fn w8(&self, off: u64, v: u8);
    fn w16(&self, off: u64, v: u16);
    fn w32(&self, off: u64, v: u32);
}

/// MAC address the firmware left in `MAC0..MAC5`.
pub fn mac(regs: &impl Regs) -> [u8; 6] {
    let mut m = [0u8; 6];
    for (i, b) in m.iter_mut().enumerate() {
        *b = regs.r8(reg::MAC0 + i as u64);
    }
    m
}

/// Software reset: requests the reset and waits for the card to
/// complete it (`attempts` reads, with whatever wait the caller provides).
pub fn reset(regs: &impl Regs, attempts: u32, mut wait: impl FnMut()) -> bool {
    regs.w8(reg::CHIP_CMD, cmd::RESET);
    for _ in 0..attempts {
        if regs.r8(reg::CHIP_CMD) & cmd::RESET == 0 {
            return true;
        }
        wait();
    }
    false
}

/// Bring-up sequence (like `rtl_hw_start` in Linux): configuration
/// unlocked, thresholds, C+ mode, descriptor rings, receive and
/// transmit configuration, open multicast filter, engine running
/// and interrupts enabled.
pub fn start(regs: &impl Regs, rx_ring_phys: u64, tx_ring_phys: u64, rx_max_size: u16) {
    regs.w8(reg::CFG9346, CFG9346_UNLOCK);
    regs.w8(reg::EARLY_TX_THRESHOLD, EARLY_TX_THRESHOLD);
    regs.w16(reg::RX_MAX_SIZE, rx_max_size);
    regs.w16(reg::CPLUS_CMD, regs.r16(reg::CPLUS_CMD) | CPLUS_CMD);
    regs.w16(reg::INTR_MITIGATE, 0);
    regs.w32(reg::TX_DESC_LO, tx_ring_phys as u32);
    regs.w32(reg::TX_DESC_HI, (tx_ring_phys >> 32) as u32);
    regs.w32(reg::RX_DESC_LO, rx_ring_phys as u32);
    regs.w32(reg::RX_DESC_HI, (rx_ring_phys >> 32) as u32);
    regs.w32(reg::TX_CONFIG, TX_CONFIG);
    regs.w32(reg::RX_CONFIG, RX_CONFIG);
    regs.w32(reg::MAR0, u32::MAX);
    regs.w32(reg::MAR0 + 4, u32::MAX);
    regs.w8(reg::CFG9346, CFG9346_LOCK);
    let _ = regs.r8(reg::INTR_MASK);
    regs.w32(reg::RX_MISSED, 0);
    regs.w8(reg::CHIP_CMD, cmd::RX_ENABLE | cmd::TX_ENABLE);
    regs.w16(reg::MULTI_INTR, regs.r16(reg::MULTI_INTR) & 0xF000);
    regs.w16(reg::INTR_MASK, intr::ENABLED);
}

/// Reads and acknowledges pending interrupts.
pub fn ack_interrupts(regs: &impl Regs) -> u16 {
    let status = regs.r16(reg::INTR_STATUS);
    if status != 0 {
        regs.w16(reg::INTR_STATUS, status);
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    /// Registers in memory: the reset "completes" on the first read.
    struct Mock {
        mem: RefCell<[u8; 256]>,
        log: RefCell<std::vec::Vec<(u64, u32)>>,
    }

    impl Mock {
        fn new() -> Self {
            let mut mem = [0u8; 256];
            mem[..6].copy_from_slice(&[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
            mem[reg::CPLUS_CMD as usize] = 0x01;
            mem[reg::MULTI_INTR as usize + 1] = 0xF1;
            Self {
                mem: RefCell::new(mem),
                log: RefCell::new(std::vec::Vec::new()),
            }
        }
    }

    impl Regs for Mock {
        fn r8(&self, off: u64) -> u8 {
            let v = self.mem.borrow()[off as usize];
            if off == reg::CHIP_CMD && v & cmd::RESET != 0 {
                self.mem.borrow_mut()[off as usize] = v & !cmd::RESET;
            }
            v
        }
        fn r16(&self, off: u64) -> u16 {
            let m = self.mem.borrow();
            u16::from_le_bytes([m[off as usize], m[off as usize + 1]])
        }
        fn r32(&self, off: u64) -> u32 {
            let m = self.mem.borrow();
            u32::from_le_bytes(m[off as usize..off as usize + 4].try_into().unwrap())
        }
        fn w8(&self, off: u64, v: u8) {
            self.mem.borrow_mut()[off as usize] = v;
            self.log.borrow_mut().push((off, v as u32));
        }
        fn w16(&self, off: u64, v: u16) {
            self.mem.borrow_mut()[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes());
            self.log.borrow_mut().push((off, v as u32));
        }
        fn w32(&self, off: u64, v: u32) {
            self.mem.borrow_mut()[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
            self.log.borrow_mut().push((off, v));
        }
    }

    #[test]
    fn mac_reset_and_start_sequence() {
        let m = Mock::new();
        assert_eq!(mac(&m), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut waits = 0;
        assert!(reset(&m, 10, || waits += 1));
        assert_eq!(
            waits, 1,
            "the first attempt sees the reset requested; the second, done"
        );
        start(&m, 0x1_0000_0100, 0x2000, 2048);
        let log = m.log.borrow();
        // Unlock before touching the configuration and lock afterwards.
        let unlock = log.iter().position(|e| *e == (reg::CFG9346, 0xC0)).unwrap();
        let lock = log.iter().position(|e| *e == (reg::CFG9346, 0x00)).unwrap();
        let rx_hi = log.iter().position(|e| *e == (reg::RX_DESC_HI, 1)).unwrap();
        let rx_lo = log
            .iter()
            .position(|e| *e == (reg::RX_DESC_LO, 0x100))
            .unwrap();
        assert!(unlock < rx_lo && rx_lo < lock && unlock < rx_hi);
        assert!(log.contains(&(reg::TX_DESC_LO, 0x2000)));
        assert!(log.contains(&(reg::TX_DESC_HI, 0)));
        assert!(log.contains(&(reg::RX_MAX_SIZE, 2048)));
        assert!(log.contains(&(reg::CPLUS_CMD, 0x01 | CPLUS_CMD as u32)));
        assert!(log.contains(&(reg::RX_CONFIG, RX_CONFIG)));
        assert!(log.contains(&(reg::TX_CONFIG, TX_CONFIG)));
        // The engine starts after locking and the mask goes last.
        let run = log
            .iter()
            .position(|e| *e == (reg::CHIP_CMD, (cmd::RX_ENABLE | cmd::TX_ENABLE) as u32))
            .unwrap();
        assert!(lock < run);
        assert_eq!(log.last(), Some(&(reg::INTR_MASK, intr::ENABLED as u32)));
        assert_eq!(m.r16(reg::MULTI_INTR), 0xF000);
        drop(log);
        // Acknowledging interrupts clears them.
        m.w16(reg::INTR_STATUS, intr::RX_OK | intr::LINK_CHANGE);
        assert_eq!(ack_interrupts(&m), intr::RX_OK | intr::LINK_CHANGE);
        assert_eq!(
            ack_interrupts(&m),
            intr::RX_OK | intr::LINK_CHANGE,
            "the simulator does not clear: the driver writes the bits"
        );
    }

    #[test]
    fn descriptors_and_link() {
        let d = Desc::rx(0x3000, 2048, true);
        assert_eq!(d.opts1, desc::OWN | desc::EOR | 2048);
        let b = d.to_bytes();
        assert_eq!(Desc::from_bytes(&b), d);
        let t = Desc::tx(0x4000, 60, false);
        assert_eq!(t.opts1, desc::OWN | desc::FIRST | desc::LAST | 60);
        assert_eq!(rx_frame(desc::OWN | 100), None);
        assert_eq!(rx_frame(desc::RX_ERROR | 100), Some(Err(())));
        assert_eq!(rx_frame(64 + 4), Some(Ok(64)));
        assert_eq!(rx_frame(2), Some(Ok(0)));
        assert_eq!(
            link_state(phy::LINK | phy::SPEED_100 | phy::FULL_DUPLEX),
            LinkState {
                up: true,
                mbps: 100,
                full_duplex: true
            }
        );
        assert_eq!(link_state(phy::SPEED_1000).mbps, 1000);
        assert!(!link_state(0).up);
    }

    extern crate std;
}
