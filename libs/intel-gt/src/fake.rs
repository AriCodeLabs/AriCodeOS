// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! A register bank for host tests, in the state the reference laptop's probe
//! read (ADR 0083, 2026-11-07), with the behaviour the code relies on:
//! masked registers, forcewake acks that follow requests, `ECOBUS` reading 0
//! while the GT sleeps, and a GTT whose tail holds stale entries.

extern crate std;

use crate::regs::{self, bcs};
use crate::{Clock, Regs};
use core::cell::Cell;
use std::collections::BTreeMap;
use std::vec::Vec;

/// What the laptop's registers held (dump `gt-1.txt`).
pub const PROBED: &[(u32, u32)] = &[
    (regs::GT_CORE_STATUS, 0x2010_0000),
    (regs::GT_THREAD_STATUS, 0x4000_0000),
    (regs::RP_STATE_CAP, 0x0007_0715),
    (regs::RPNSWREQ, 0x0E00_0000),
    (regs::RPSTAT1, 0x0004_0700),
    (regs::RP_CONTROL, 0x0000_0592),
    (regs::RP_INTERRUPT_LIMITS, 0x1206_0000),
    (regs::RC_CONTROL, 0x8806_0000),
    (regs::PMINTRMSK, 0),
    (regs::GDRST, 0),
    (regs::RCS_GFX_MODE, 0x2800),
    (regs::GTIMR, 0xFFFF_FFFF),
    (bcs::CTL, 0),
    (bcs::HEAD, 0),
    (bcs::TAIL, 0),
    (bcs::START, 0),
    (bcs::MI_MODE, 0x200),
    (bcs::MODE, 0),
    (bcs::HWSTAM, 0xFFFF_FFFF),
    (bcs::IMR, 0xFFFF_FFFF),
    (bcs::EIR, 0),
    (bcs::FAULT, 0),
    (bcs::HWS_PGA, 0),
];
/// `ECOBUS` while the GT is awake.
pub const ECOBUS_AWAKE: u32 = 0x8410_0020;
/// The firmware's stolen memory, mapped by the first 16,383 entries.
pub const STOLEN: u64 = 0xDBE0_0000;
pub const FIRMWARE_RUN: u32 = 16_383;
/// The first stale entry after the run, as read.
pub const STALE: u32 = 0x217D_80C1;
pub const ENTRIES: u32 = 512 * 1024;

const MASKED: [u32; 5] = [
    regs::FORCEWAKE_MT,
    bcs::MI_MODE,
    bcs::MODE,
    bcs::INSTPM,
    regs::RCS_GFX_MODE,
];

pub struct Bank {
    pub regs: BTreeMap<u32, u32>,
    pub gtt: Vec<u32>,
    pub writes: Vec<(u32, u32)>,
    /// Acks follow requests (false: they never come).
    pub acks: bool,
    /// Self-clearing bits clear (false: a reset or flush never ends).
    pub completes: bool,
    /// The head ignores writes (a ring that will not take its head).
    pub stuck_head: bool,
    pub ecobus: u32,
}

impl Bank {
    /// The laptop as probed: both forcewake requests held by the firmware.
    pub fn probed() -> Self {
        let mut regs: BTreeMap<u32, u32> = PROBED.iter().copied().collect();
        regs.insert(regs::FORCEWAKE_MT, 0x0000_0001);
        regs.insert(regs::FORCEWAKE_MT_ACK, 1);
        regs.insert(regs::FORCEWAKE, 1);
        regs.insert(regs::FORCEWAKE_ACK, 1);
        let mut gtt = Vec::with_capacity(ENTRIES as usize);
        for i in 0..ENTRIES {
            gtt.push(if i < FIRMWARE_RUN {
                (STOLEN as u32 + i * 4096) | 1
            } else {
                STALE ^ (i << 12)
            });
        }
        Bank {
            regs,
            gtt,
            writes: Vec::new(),
            acks: true,
            completes: true,
            stuck_head: false,
            ecobus: ECOBUS_AWAKE,
        }
    }

    pub fn get(&self, offset: u32) -> u32 {
        self.regs.get(&offset).copied().unwrap_or(0)
    }

    fn awake(&self) -> bool {
        self.get(regs::FORCEWAKE_MT) & 1 != 0 || self.get(regs::FORCEWAKE) & 1 != 0
    }

    /// Writes made to `offset`, in order.
    pub fn writes_to(&self, offset: u32) -> Vec<u32> {
        self.writes
            .iter()
            .filter(|w| w.0 == offset)
            .map(|w| w.1)
            .collect()
    }
}

impl Regs for Bank {
    fn read(&self, offset: u32) -> u32 {
        if offset >= regs::GTT {
            return self.gtt[((offset - regs::GTT) / 4) as usize];
        }
        match offset {
            regs::ECOBUS if self.awake() => self.ecobus,
            regs::ECOBUS => 0,
            _ => self.get(offset),
        }
    }

    fn write(&mut self, offset: u32, value: u32) {
        assert!(!regs::NEVER_TOUCH.contains(&offset), "touched {offset:#x}");
        self.writes.push((offset, value));
        if offset >= regs::GTT {
            self.gtt[((offset - regs::GTT) / 4) as usize] = value;
            return;
        }
        let new = if MASKED.contains(&offset) {
            let mask = value >> 16;
            (self.get(offset) & !mask) | (value & mask)
        } else if offset == regs::FORCEWAKE {
            // Linux writes it in the masked form; the hardware keeps bit 0.
            value & 1
        } else {
            value
        };
        if offset == bcs::HEAD && self.stuck_head {
            return;
        }
        // A synchronizing flush and a reset end at once, if they end.
        let new = match offset {
            bcs::INSTPM if self.completes => new & !bcs::INSTPM_SYNC_FLUSH,
            regs::GDRST if self.completes => 0,
            _ => new,
        };
        self.regs.insert(offset, new);
        let ack = match offset {
            regs::FORCEWAKE_MT => Some(regs::FORCEWAKE_MT_ACK),
            regs::FORCEWAKE => Some(regs::FORCEWAKE_ACK),
            _ => None,
        };
        if let (Some(ack), true) = (ack, self.acks) {
            self.regs.insert(ack, new & 1);
        }
    }
}

/// A clock that moves one microsecond each time it is read.
pub struct Ticks(pub Cell<u64>);

impl Ticks {
    pub fn new() -> Self {
        Ticks(Cell::new(0))
    }
}

impl Clock for Ticks {
    fn now_us(&self) -> u64 {
        let t = self.0.get() + 1;
        self.0.set(t);
        t
    }
}
