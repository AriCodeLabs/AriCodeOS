// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! rtl-net (Phase 34, ADR 0024): ring 3 driver for the Realtek r8169
//! family (RTL8169/8168/8101; the RTL8136 in the test laptop) on top of
//! the shared network core (`ari-net-core`). Receives the driver contract
//! from init (`SERVICE`, cfg, hardware, IRQ, BARs) and `SYSTEM` as an
//! extra. Rings of 16 receive and 8 transmit descriptors, each with a
//! 2 KiB buffer, all in a single DMA buffer.
#![no_std]
#![no_main]

use ari_net_core::{run, say, Clock, Link, MAX_FRAME};
use aricode_net::MacAddr;
use aricode_rt::sys;
use aricode_rt::sys::virtio_pci::{bar_handle, extra_handle, DmaRegion, Mmio, IRQ};
use aricode_rtl8169::{ack_interrupts, link_state, reg, rx_frame, Desc, Regs, TX_POLL_NPQ};

const NAME: &str = "rtl-net";
const RX_SLOTS: usize = 16;
const TX_SLOTS: usize = 8;
const SLOT: u64 = 2048;
/// Rings at the start (256-aligned), buffers from 4 KiB onwards.
const RX_RING_OFF: u64 = 0;
const TX_RING_OFF: u64 = 1024;
const BUFFERS_OFF: u64 = 4096;
const DMA_BYTES: u64 = BUFFERS_OFF + SLOT * (RX_SLOTS + TX_SLOTS) as u64;

/// Initialization exit codes.
mod exit {
    pub const NO_BAR: u64 = 10;
    pub const DMA: u64 = 11;
    pub const RESET: u64 = 12;
}

struct MmioRegs(Mmio);

impl Regs for MmioRegs {
    fn r8(&self, off: u64) -> u8 {
        self.0.r8(off)
    }
    fn r16(&self, off: u64) -> u16 {
        self.0.r16(off)
    }
    fn r32(&self, off: u64) -> u32 {
        self.0.r32(off)
    }
    fn w8(&self, off: u64, v: u8) {
        self.0.w8(off, v)
    }
    fn w16(&self, off: u64, v: u16) {
        self.0.w16(off, v)
    }
    fn w32(&self, off: u64, v: u32) {
        self.0.w32(off, v)
    }
}

struct Nic {
    regs: MmioRegs,
    dma: DmaRegion,
    rx_next: usize,
    tx_next: usize,
    mac: MacAddr,
}

impl Nic {
    fn desc_virt(&self, ring_off: u64, i: usize) -> *mut [u8; Desc::LEN] {
        (self.dma.virt + ring_off + (i * Desc::LEN) as u64) as *mut [u8; Desc::LEN]
    }

    fn read_desc(&self, ring_off: u64, i: usize) -> Desc {
        // SAFETY: descriptor inside the driver's DMA buffer.
        Desc::from_bytes(&unsafe { self.desc_virt(ring_off, i).read_volatile() })
    }

    fn write_desc(&self, ring_off: u64, i: usize, d: Desc) {
        // SAFETY: same as above.
        unsafe { self.desc_virt(ring_off, i).write_volatile(d.to_bytes()) };
    }

    fn rx_buffer_phys(&self, i: usize) -> u64 {
        self.dma.phys + BUFFERS_OFF + SLOT * i as u64
    }

    fn tx_buffer_phys(&self, i: usize) -> u64 {
        self.dma.phys + BUFFERS_OFF + SLOT * (RX_SLOTS + i) as u64
    }

    fn setup(mask: u64, clock: &Clock) -> Result<Self, u64> {
        let bar = (0..6u8)
            .find_map(|b| bar_handle(mask, b))
            .ok_or(exit::NO_BAR)?;
        let regs = MmioRegs(Mmio(sys::mmio_map(bar).map_err(|_| exit::NO_BAR)?));
        let dma = DmaRegion::alloc(DMA_BYTES).map_err(|_| exit::DMA)?;
        if !aricode_rtl8169::reset(&regs, 100, || {
            let _ = sys::irq_wait(IRQ, 2);
        }) {
            return Err(exit::RESET);
        }
        let mac = MacAddr(aricode_rtl8169::mac(&regs));
        let nic = Self {
            regs,
            dma,
            rx_next: 0,
            tx_next: 0,
            mac,
        };
        for i in 0..RX_SLOTS {
            nic.write_desc(
                RX_RING_OFF,
                i,
                Desc::rx(nic.rx_buffer_phys(i), SLOT as u16, i + 1 == RX_SLOTS),
            );
        }
        for i in 0..TX_SLOTS {
            // No OWN: free for the driver; EOR on the last one.
            let mut d = Desc::tx(nic.tx_buffer_phys(i), 0, i + 1 == TX_SLOTS);
            d.opts1 &= !aricode_rtl8169::desc::OWN;
            nic.write_desc(TX_RING_OFF, i, d);
        }
        aricode_rtl8169::start(
            &nic.regs,
            nic.dma.phys + RX_RING_OFF,
            nic.dma.phys + TX_RING_OFF,
            SLOT as u16,
        );
        // Link: up to 3 s for the PHY to announce it; without it, DHCP retries.
        let deadline = clock.now() + 3000;
        loop {
            let link = link_state(nic.regs.r8(reg::PHY_STATUS));
            if link.up {
                let mut b = [0u8; 20];
                sys::print(b"rtl-net: link at ");
                sys::print(aricode_rt::fmt_u64(link.mbps as u64, &mut b));
                sys::print(if link.full_duplex {
                    b" Mb/s full duplex\n"
                } else {
                    b" Mb/s half duplex\n"
                });
                break;
            }
            if clock.now() >= deadline {
                say(NAME, b"no link (cable unplugged?); still trying");
                break;
            }
            let _ = sys::irq_wait(IRQ, 100);
        }
        Ok(nic)
    }
}

impl Link for Nic {
    fn mac(&self) -> MacAddr {
        self.mac
    }

    fn send_frame(&mut self, frame: &[u8]) -> bool {
        if frame.len() > MAX_FRAME || frame.is_empty() {
            return false;
        }
        let i = self.tx_next;
        if self.read_desc(TX_RING_OFF, i).opts1 & aricode_rtl8169::desc::OWN != 0 {
            return false; // the card has not sent the oldest descriptor yet
        }
        let off = BUFFERS_OFF + SLOT * (RX_SLOTS + i) as u64;
        // Short frames padded to 60 bytes: the card does not always do it.
        let len = frame.len().max(60);
        let buf = self.dma.slice_mut(off, len as u64);
        buf[..frame.len()].copy_from_slice(frame);
        buf[frame.len()..].fill(0);
        self.write_desc(
            TX_RING_OFF,
            i,
            Desc::tx(self.tx_buffer_phys(i), len as u16, i + 1 == TX_SLOTS),
        );
        self.regs.w8(reg::TX_POLL, TX_POLL_NPQ);
        self.tx_next = (i + 1) % TX_SLOTS;
        true
    }

    fn link_up(&self) -> Option<bool> {
        Some(link_state(self.regs.r8(reg::PHY_STATUS)).up)
    }

    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize> {
        let _ = ack_interrupts(&self.regs);
        loop {
            let i = self.rx_next;
            let d = self.read_desc(RX_RING_OFF, i);
            let got = rx_frame(d.opts1)?;
            let off = BUFFERS_OFF + SLOT * i as u64;
            let len = match got {
                Ok(n) => n.min(MAX_FRAME),
                Err(()) => 0,
            };
            if len > 0 {
                out[..len].copy_from_slice(self.dma.slice_mut(off, len as u64));
            }
            // Hand the descriptor back to the card and advance.
            self.write_desc(
                RX_RING_OFF,
                i,
                Desc::rx(self.rx_buffer_phys(i), SLOT as u16, i + 1 == RX_SLOTS),
            );
            self.rx_next = (i + 1) % RX_SLOTS;
            if len > 0 {
                return Some(len);
            }
        }
    }
}

fn main(mask: u64) -> u64 {
    let clock = Clock(extra_handle(mask, 0));
    let nic = match Nic::setup(mask, &clock) {
        Ok(n) => n,
        Err(step) => {
            sys::print(b"rtl-net: initialization failed: ");
            sys::print(match step {
                exit::NO_BAR => b"no memory BAR\n",
                exit::DMA => b"no DMA memory\n",
                _ => b"the card does not complete the reset\n",
            });
            return step;
        }
    };
    run(nic, clock, NAME)
}

aricode_rt::entry!(main);
