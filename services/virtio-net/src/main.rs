// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! netd (Phase 22): ring 3 virtio-net driver. The network stack and the
//! `NetServiceProtocol v1` server live in `ari-net-core` (ADR 0024);
//! only the card is here: the virtio receive and transmit queues.
//!
//! Receives the driver contract from init (`aricode_sys::virtio_pci`) and, as
//! an extra after the BARs, the `SYSTEM` object (`READ`) to measure time.
#![no_std]
#![no_main]

use ari_net_core::{run, say, Clock, Link, MAX_FRAME};
use aricode_net::MacAddr;
use aricode_rt::sys::virtio_pci::{extra_handle, DmaRegion, Queue, Transport};

const NAME: &str = "virtio-net";
/// VIRTIO_NET_F_MAC: the device has a MAC in its configuration.
const FEATURE_MAC: u64 = 1 << 5;
/// virtio-net header (no MRG_RXBUF but with VERSION_1: 12 bytes).
const NET_HDR: u64 = 12;
const SLOT: u64 = 2048;
const RX_SLOTS: u16 = 16;
const TX_SLOTS: u16 = 8;
/// Exit code when the device returned a buffer it never held: its queue
/// state cannot be trusted; init relaunches the driver, which resets it.
const EXIT_DEVICE_LIED: u64 = 50;

/// A used-ring element the device made up (audit 2026-10-25 A, M1).
fn device_lied(_: aricode_virtio::UsedError) -> ! {
    say(
        NAME,
        b"the device returned a buffer it never held; exiting to reset it",
    );
    aricode_rt::sys::process_exit(EXIT_DEVICE_LIED)
}

struct Nic {
    transport: Transport,
    rx: Queue,
    tx: Queue,
    rx_buf: DmaRegion,
    tx_buf: DmaRegion,
    tx_next: u16,
    tx_inflight: u16,
    mac: MacAddr,
}

impl Nic {
    fn setup(mask: u64) -> Result<Self, u64> {
        // The MAC: the first 6 bytes of the device configuration.
        let transport = Transport::open(mask, FEATURE_MAC, 6).map_err(|e| e.code())?;
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = transport.device_cfg.r8(i as u64);
        }
        let mut rx = transport.setup_queue(0, RX_SLOTS).map_err(|e| e.code())?;
        let tx = transport.setup_queue(1, TX_SLOTS).map_err(|e| e.code())?;
        let rx_buf = DmaRegion::alloc(SLOT * RX_SLOTS as u64).map_err(|_| 30u64)?;
        let tx_buf = DmaRegion::alloc(SLOT * TX_SLOTS as u64).map_err(|_| 31u64)?;
        for i in 0..rx.vq.size().min(RX_SLOTS) {
            rx.vq
                .add(&[(rx_buf.phys + SLOT * i as u64, SLOT as u32, true)])
                .ok_or(32u64)?;
        }
        transport.driver_ok();
        rx.notify(&transport);
        Ok(Self {
            transport,
            rx,
            tx,
            rx_buf,
            tx_buf,
            tx_next: 0,
            tx_inflight: 0,
            mac: MacAddr(mac),
        })
    }

    /// Collects completed transmissions so their slots can be reused.
    fn reclaim_tx(&mut self) {
        loop {
            match self.tx.vq.pop_used() {
                Ok(Some(_)) => self.tx_inflight = self.tx_inflight.saturating_sub(1),
                Ok(None) => break,
                Err(e) => device_lied(e),
            }
        }
    }

    /// Transmits a complete Ethernet frame.
    fn send_frame(&mut self, frame: &[u8]) -> bool {
        self.reclaim_tx();
        let slots = self.tx.vq.size().min(TX_SLOTS);
        if self.tx_inflight >= slots || frame.len() > MAX_FRAME {
            return false;
        }
        let slot = self.tx_next % slots;
        self.tx_next = self.tx_next.wrapping_add(1);
        let off = SLOT * slot as u64;
        let buf = self.tx_buf.slice_mut(off, NET_HDR + frame.len() as u64);
        buf[..NET_HDR as usize].fill(0);
        buf[NET_HDR as usize..].copy_from_slice(frame);
        let ok = self
            .tx
            .vq
            .add(&[(
                self.tx_buf.phys + off,
                (NET_HDR + frame.len() as u64) as u32,
                false,
            )])
            .is_some();
        if ok {
            self.tx_inflight += 1;
            self.tx.notify(&self.transport);
        }
        ok
    }

    /// Next received frame (copied into `out`); refills the slot.
    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize> {
        let (head, len) = match self.rx.vq.pop_used() {
            Ok(used) => used?,
            Err(e) => device_lied(e),
        };
        let off = SLOT * head as u64;
        let len = (len as usize)
            .saturating_sub(NET_HDR as usize)
            .min(MAX_FRAME);
        let src = self.rx_buf.slice_mut(off + NET_HDR, len as u64);
        out[..len].copy_from_slice(src);
        // Refill the same slot.
        let _ = self
            .rx
            .vq
            .add(&[(self.rx_buf.phys + off, SLOT as u32, true)]);
        self.rx.notify(&self.transport);
        Some(len)
    }
}
impl Link for Nic {
    fn mac(&self) -> MacAddr {
        self.mac
    }
    fn send_frame(&mut self, frame: &[u8]) -> bool {
        Nic::send_frame(self, frame)
    }
    fn recv_frame(&mut self, out: &mut [u8; MAX_FRAME]) -> Option<usize> {
        Nic::recv_frame(self, out)
    }
    fn rx_pending(&self) -> bool {
        self.rx.vq.has_used()
    }
    fn link_up(&self) -> Option<bool> {
        // Without VIRTIO_NET_F_STATUS negotiated the virtual link is always up.
        Some(true)
    }
}

fn main(mask: u64) -> u64 {
    let nic = match Nic::setup(mask) {
        Ok(n) => n,
        Err(step) => {
            say(NAME, b"initialization failed");
            return step;
        }
    };
    run(nic, Clock(extra_handle(mask, 0)), NAME)
}

aricode_rt::entry!(main);
