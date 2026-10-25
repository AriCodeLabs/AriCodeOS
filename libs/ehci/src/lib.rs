// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! EHCI (ADR 0031): what the ring 3 driver needs to know about an Enhanced
//! Host Controller Interface 1.0 controller and USB 2.0 hubs without
//! touching hardware: register offsets and bits, the legacy support
//! (firmware handoff) capability, queue heads and qTDs in their 32-bit and
//! 64-bit layouts, transfer completion decoding, the periodic schedule
//! parameters of an interrupt endpoint, split transactions through a
//! transaction translator and the hub class (descriptor, port status,
//! requests). Section numbers are those of the EHCI 1.0 specification and
//! of the USB 2.0 specification. Tested on the host; the driver only copies
//! these words into DMA memory and writes MMIO.

#![no_std]

use aricode_xhci::SetupPacket;

/// Capability registers (EHCI 2.2), from the BAR0 base.
pub mod cap {
    /// CAPLENGTH (byte 0) and HCIVERSION (bytes 2-3), read as one dword.
    pub const CAPLENGTH: u64 = 0x00;
    pub const HCSPARAMS: u64 = 0x04;
    pub const HCCPARAMS: u64 = 0x08;
}

/// Structural and capability parameters (EHCI 2.2.3, 2.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// N_PORTS: root hub ports.
    pub ports: u8,
    /// PPC: ports have power switches (software sets PORTSC.PP).
    pub port_power_control: bool,
    /// N_CC: companion controllers (UHCI/OHCI) that take full- and
    /// low-speed devices; 0 on Intel 6/7 series, which put a rate matching
    /// hub behind each EHCI instead.
    pub companions: u8,
    /// 64-bit Addressing Capability: the 64-bit data structures of
    /// Appendix B are used and CTRLDSSEGMENT holds the upper 32 bits of
    /// every link pointer.
    pub addressing_64: bool,
    /// EECP: offset of the first extended capability in PCI configuration
    /// space (0: none).
    pub eecp: u8,
}

impl Params {
    /// Whether the operational registers, every root port's included, lie
    /// within the controller's BAR of `bar_len` bytes at `caplength` (audit
    /// 2026-10-25 A, L1): the driver addresses them as the BAR's base plus
    /// that offset.
    pub fn registers_fit(&self, caplength: u64, bar_len: u64) -> bool {
        let ports = (self.ports as u64).max(1);
        caplength + op::portsc(ports as u8) + 4 <= bar_len
    }

    pub fn decode(hcsparams: u32, hccparams: u32) -> Self {
        Self {
            ports: (hcsparams & 0xF) as u8,
            port_power_control: hcsparams & 1 << 4 != 0,
            companions: ((hcsparams >> 12) & 0xF) as u8,
            addressing_64: hccparams & 1 != 0,
            eecp: (hccparams >> 8) as u8,
        }
    }
}

/// HCIVERSION of EHCI 1.0 (BCD); anything below is not this interface.
pub const HCI_VERSION_1_0: u16 = 0x0100;

/// Operational registers (EHCI 2.3), from `base + CAPLENGTH`.
pub mod op {
    pub const USBCMD: u64 = 0x00;
    pub const USBSTS: u64 = 0x04;
    pub const USBINTR: u64 = 0x08;
    pub const FRINDEX: u64 = 0x0C;
    pub const CTRLDSSEGMENT: u64 = 0x10;
    pub const PERIODICLISTBASE: u64 = 0x14;
    pub const ASYNCLISTADDR: u64 = 0x18;
    pub const CONFIGFLAG: u64 = 0x40;
    const PORTSC_BASE: u64 = 0x44;

    /// PORTSC of root port `port` (1-based).
    pub fn portsc(port: u8) -> u64 {
        PORTSC_BASE + 4 * (port as u64 - 1)
    }

    /// USBCMD (2.3.1).
    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;
    pub const CMD_PERIODIC: u32 = 1 << 4;
    pub const CMD_ASYNC: u32 = 1 << 5;
    /// Interrupt on Async Advance Doorbell (4.8.2).
    pub const CMD_IAA_DOORBELL: u32 = 1 << 6;
    /// Interrupt threshold of 8 micro-frames, the default (bits 23:16).
    pub const CMD_ITC_8: u32 = 0x08 << 16;

    /// USBSTS (2.3.2); the low six bits are write-1-to-clear.
    pub const STS_ERROR: u32 = 1 << 1;
    pub const STS_PORT_CHANGE: u32 = 1 << 2;
    pub const STS_HOST_SYSTEM_ERROR: u32 = 1 << 4;
    pub const STS_ASYNC_ADVANCE: u32 = 1 << 5;
    pub const STS_HALTED: u32 = 1 << 12;
    pub const STS_PERIODIC: u32 = 1 << 14;
    pub const STS_ASYNC: u32 = 1 << 15;
    pub const STS_ACK_BITS: u32 = 0x3F;

    /// CONFIGFLAG (2.3.8): every port routed to this controller instead of
    /// its companions.
    pub const CONFIGURED: u32 = 1;
}

/// Port status and control (EHCI 2.3.9).
pub mod portsc {
    pub const CONNECTED: u32 = 1 << 0;
    pub const CONNECT_CHANGE: u32 = 1 << 1;
    pub const ENABLED: u32 = 1 << 2;
    pub const ENABLE_CHANGE: u32 = 1 << 3;
    pub const OVER_CURRENT: u32 = 1 << 4;
    pub const OVER_CURRENT_CHANGE: u32 = 1 << 5;
    pub const RESET: u32 = 1 << 8;
    pub const POWER: u32 = 1 << 12;
    /// Port Owner: 1 hands the port to a companion controller (4.2.2).
    pub const OWNER: u32 = 1 << 13;
    /// Write-1-to-clear change bits: a read-modify-write must not echo them.
    pub const CHANGE_BITS: u32 = CONNECT_CHANGE | ENABLE_CHANGE | OVER_CURRENT_CHANGE;

    /// Value to write to change a port: the current value without the
    /// change bits (writing them back would clear them) and without
    /// `clear`, plus `set` (which may name change bits to acknowledge).
    /// Port Enabled is kept as read: software can only clear it.
    pub fn write_value(current: u32, set: u32, clear: u32) -> u32 {
        (current & !CHANGE_BITS & !clear) | set
    }

    /// Line Status (bits 11:10) with the port not enabled: `01` is the
    /// K-state of a low-speed device (Table 2-16), which belongs to a
    /// companion controller.
    pub fn low_speed_line(v: u32) -> bool {
        (v >> 10) & 3 == 0b01
    }

    /// Short description for traces: `connected enabled power ...`.
    pub fn describe(v: u32, out: &mut [u8]) -> usize {
        let names: [(u32, &str); 8] = [
            (CONNECTED, "connected"),
            (CONNECT_CHANGE, "connect-change"),
            (ENABLED, "enabled"),
            (ENABLE_CHANGE, "enable-change"),
            (OVER_CURRENT, "over-current"),
            (RESET, "reset"),
            (POWER, "power"),
            (OWNER, "companion"),
        ];
        let mut len = 0;
        for (bit, name) in names {
            if v & bit == 0 {
                continue;
            }
            let sep = usize::from(len > 0);
            if len + sep + name.len() > out.len() {
                break;
            }
            if sep == 1 {
                out[len] = b' ';
            }
            out[len + sep..len + sep + name.len()].copy_from_slice(name.as_bytes());
            len += sep + name.len();
        }
        len
    }
}

/// USB Legacy Support extended capability in PCI configuration space
/// (EHCI 2.1.7, 2.1.8, handoff in 5.1).
pub mod legacy {
    /// Capability ID of USBLEGSUP.
    pub const CAPABILITY_ID: u8 = 1;
    /// USBLEGSUP: HC BIOS Owned and HC OS Owned semaphores.
    pub const BIOS_OWNED: u32 = 1 << 16;
    pub const OS_OWNED: u32 = 1 << 24;
    /// USBLEGCTLSTS (at +4): SMI enables (bits 5:0 and 15:13) and the
    /// write-1-to-clear SMI events (bits 31:29).
    const SMI_ENABLES: u32 = 0x3F | 0x7 << 13;
    const SMI_EVENTS: u32 = 0x7 << 29;

    /// Walks the extended capability list from `eecp` with `read` (a dword
    /// of configuration space) and returns the offset of USBLEGSUP. The
    /// walk is bounded (a corrupt list cannot loop) and stays above the
    /// standard header (offsets below 0x40 end it).
    pub fn find(eecp: u8, read: impl Fn(u8) -> u32) -> Option<u8> {
        let mut off = eecp;
        for _ in 0..48 {
            if off < 0x40 {
                return None;
            }
            let v = read(off);
            if v as u8 == CAPABILITY_ID {
                return Some(off);
            }
            off = (v >> 8) as u8;
        }
        None
    }

    /// USBLEGCTLSTS with every SMI source disabled and the pending events
    /// acknowledged (5.1: the OS owns the controller, the firmware must not
    /// be called on its events any more).
    pub fn smis_disabled(ctlsts: u32) -> u32 {
        (ctlsts & !SMI_ENABLES & !SMI_EVENTS) | SMI_EVENTS
    }
}

/// Endpoint speed as the queue head encodes it (EPS, EHCI 3.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Full = 0,
    Low = 1,
    High = 2,
}

impl Speed {
    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Low => "low",
            Self::High => "high",
        }
    }

    /// EP0 packet size assumed before the device descriptor says it
    /// (USB 2.0 5.5.3: 64 at high speed, 8 is valid at every lower speed).
    pub fn default_ep0_max_packet(self) -> u16 {
        match self {
            Self::High => 64,
            Self::Full | Self::Low => 8,
        }
    }
}

/// Transaction translator of a full- or low-speed device: the high-speed
/// hub that converts its transactions and the hub port it hangs from
/// (EHCI 4.12, USB 2.0 11.14). Split transactions name both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tt {
    pub hub: u8,
    pub port: u8,
}

/// The translator a device on `port` of a hub uses. A device at the same
/// speed as a high-speed hub needs none; a full- or low-speed device below
/// a high-speed hub uses that hub's translator on `port`; anything below a
/// full-speed hub keeps the translator of that hub (USB 2.0 11.14.1).
pub fn tt_for(
    hub_speed: Speed,
    hub_address: u8,
    hub_tt: Option<Tt>,
    port: u8,
    device: Speed,
) -> Result<Option<Tt>, &'static str> {
    match (hub_speed, device) {
        (Speed::High, Speed::High) => Ok(None),
        (Speed::High, _) => Ok(Some(Tt {
            hub: hub_address,
            port,
        })),
        (_, Speed::High) => Err("a full-speed hub reported a high-speed device"),
        _ => hub_tt
            .map(Some)
            .ok_or("full-speed hub without a transaction translator"),
    }
}

/// Link pointers (EHCI 3.1): 32-byte aligned address, type in bits 2:1,
/// Terminate in bit 0.
pub mod link {
    pub const TERMINATE: u32 = 1;
    const TYPE_QH: u32 = 1 << 1;

    /// Pointer to a queue head. `None` if `phys` is not 32-byte aligned.
    pub fn qh(phys: u64) -> Option<u32> {
        (phys & 0x1F == 0).then_some(phys as u32 | TYPE_QH)
    }
    /// Pointer to a qTD (next / alternate next, type bits unused).
    pub fn qtd(phys: u64) -> Option<u32> {
        (phys & 0x1F == 0).then_some(phys as u32)
    }
}

/// qTD token (EHCI 3.5.3).
pub mod token {
    pub const ACTIVE: u32 = 1 << 7;
    pub const HALTED: u32 = 1 << 6;
    pub const DATA_BUFFER_ERROR: u32 = 1 << 5;
    pub const BABBLE: u32 = 1 << 4;
    pub const TRANSACTION_ERROR: u32 = 1 << 3;
    pub const MISSED_MICRO_FRAME: u32 = 1 << 2;
    /// Ping State (high speed) / split transaction ERR bit.
    pub const PING: u32 = 1 << 0;
    pub const IOC: u32 = 1 << 15;
    pub const DATA_TOGGLE: u32 = 1 << 31;
    /// Error counter: three tries per transaction (4.15.1.1.1); the
    /// controller retries on its own, software does not.
    pub const CERR_3: u32 = 3 << 10;

    pub fn cerr(token: u32) -> u8 {
        ((token >> 10) & 3) as u8
    }
    pub fn bytes(token: u32) -> u32 {
        (token >> 16) & 0x7FFF
    }
}

/// PID code of a qTD (EHCI 3.5.3, bits 9:8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pid {
    Out = 0,
    In = 1,
    Setup = 2,
}

/// Bytes one qTD can move (EHCI 3.5.3: Total Bytes to Transfer allows up
/// to 0x5000, five 4 KiB pages).
pub const QTD_MAX_BYTES: u32 = 5 * 4096;

/// Queue element transfer descriptor (EHCI 3.5; Appendix B for the
/// extended buffer pointers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qtd {
    /// Next qTD pointer (`link::TERMINATE` for the last one).
    pub next: u32,
    /// Alternate next qTD pointer: where the queue goes after a short
    /// packet (EHCI 4.10.2): the status stage of a control transfer, or an
    /// inactive qTD that stops the queue; `link::TERMINATE` continues with
    /// `next`.
    pub alt_next: u32,
    pub pid: Pid,
    pub bytes: u32,
    /// Data toggle, used only when the queue head takes it from the qTD
    /// (DTC = 1: control endpoints).
    pub toggle: bool,
    pub buffer: u64,
}

impl Qtd {
    /// 32-bit layout: 8 dwords; 64-bit layout: 13 (Appendix B).
    pub const WORDS_32: usize = 8;
    pub const WORDS_64: usize = 13;
    /// Bytes reserved per qTD in DMA memory (32-byte alignment, room for
    /// the 64-bit layout).
    pub const STRIDE: u64 = 64;

    /// The dwords of the qTD, active, with three error tries. `None` if
    /// the buffer does not fit in one qTD: more than `QTD_MAX_BYTES` from
    /// its page offset, or crossing a 4 GiB boundary.
    pub fn words(&self) -> Option<[u32; Self::WORDS_64]> {
        let pages = buffer_pages(self.buffer, self.bytes)?;
        let mut w = [0u32; Self::WORDS_64];
        w[0] = self.next;
        w[1] = self.alt_next;
        w[2] = token::ACTIVE
            | token::CERR_3
            | (self.pid as u32) << 8
            | self.bytes << 16
            | if self.toggle { token::DATA_TOGGLE } else { 0 };
        for (i, page) in pages.iter().enumerate() {
            w[3 + i] = *page as u32;
            w[8 + i] = (*page >> 32) as u32;
        }
        Some(w)
    }
}

/// Buffer page pointers of a qTD (EHCI 3.5.4): the first carries the byte
/// offset, the rest are the following 4 KiB pages. Unused pages are 0.
fn buffer_pages(phys: u64, len: u32) -> Option<[u64; 5]> {
    let offset = (phys & 0xFFF) as u32;
    if len > QTD_MAX_BYTES - offset {
        return None;
    }
    // The upper 32 bits of a qTD's pages are one segment per page, but a
    // buffer straddling 4 GiB is refused to keep the 32-bit check simple.
    if len > 0 && (phys >> 32) != ((phys + len as u64 - 1) >> 32) {
        return None;
    }
    let mut pages = [0u64; 5];
    pages[0] = phys;
    let last_page = if len == 0 {
        0
    } else {
        (offset + len - 1) / 4096
    };
    for (i, page) in pages.iter_mut().enumerate().skip(1) {
        if i as u32 <= last_page {
            *page = (phys & !0xFFF) + 4096 * i as u64;
        }
    }
    Some(pages)
}

/// Splits a transfer into qTD-sized pieces `(physical, bytes)`. Every piece
/// but the last ends on a `max_packet` boundary, since a packet cannot span
/// two qTDs (EHCI 4.10.6: a short packet would otherwise end the transfer
/// early). Returns the number of pieces, `None` if `out` is too small or
/// `max_packet` is 0.
pub fn split_transfer(
    phys: u64,
    len: u32,
    max_packet: u16,
    out: &mut [(u64, u32)],
) -> Option<usize> {
    if max_packet == 0 {
        return None;
    }
    let mp = max_packet as u32;
    let mut done = 0u32;
    let mut n = 0;
    loop {
        let at = phys + done as u64;
        let room = QTD_MAX_BYTES - (at & 0xFFF) as u32;
        let left = len - done;
        let piece = if left <= room { left } else { room - room % mp };
        if piece == 0 && left > 0 {
            return None;
        }
        *out.get_mut(n)? = (at, piece);
        n += 1;
        done += piece;
        if done == len {
            return Some(n);
        }
    }
}

/// How a qTD ended (EHCI 4.10.3 and 4.15.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still owned by the controller.
    Active,
    /// Completed; `remaining` bytes were not transferred (short packet).
    Done { remaining: u32 },
    /// The queue head halted.
    Error(TransferError),
}

/// Why a qTD halted the queue (EHCI 4.15.1.1, 4.12.1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    /// STALL handshake: the endpoint (or a control request) was refused.
    Stall,
    /// The device sent more than the packet size (4.15.1.1.3).
    Babble,
    /// The controller could not move the data in time (4.15.1.1.2).
    DataBuffer,
    /// Three transaction errors in a row (timeout, CRC, bad PID): CERR
    /// reached zero (4.15.1.1.1).
    Transaction,
    /// A split transaction lost its complete-split (4.12.1.3).
    MissedMicroFrame,
}

impl TransferError {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stall => "STALL",
            Self::Babble => "babble",
            Self::DataBuffer => "data buffer error",
            Self::Transaction => "transaction error",
            Self::MissedMicroFrame => "missed micro-frame",
        }
    }

    /// Stable code for fault events.
    pub fn code(self) -> u64 {
        self as u64 + 1
    }
}

/// Decodes a qTD token. A halt with none of the error bits that cause one
/// (babble, data buffer error, CERR exhausted after transaction errors,
/// missed micro-frame) was a STALL handshake (4.10.3).
pub fn decode_token(t: u32) -> Outcome {
    if t & token::ACTIVE != 0 {
        return Outcome::Active;
    }
    if t & token::HALTED == 0 {
        return Outcome::Done {
            remaining: token::bytes(t),
        };
    }
    Outcome::Error(if t & token::BABBLE != 0 {
        TransferError::Babble
    } else if t & token::DATA_BUFFER_ERROR != 0 {
        TransferError::DataBuffer
    } else if t & token::TRANSACTION_ERROR != 0 && token::cerr(t) == 0 {
        TransferError::Transaction
    } else if t & token::MISSED_MICRO_FRAME != 0 {
        TransferError::MissedMicroFrame
    } else {
        TransferError::Stall
    })
}

/// Token to write into a queue head's overlay when it is given new qTDs or
/// after a halt (EHCI 4.10.2): not active, not halted, the data toggle
/// kept when the queue head carries it (`keep_toggle`) and, at high speed,
/// the ping state. At full and low speed bit 0 is the split transaction
/// ERR bit of the last transfer (4.12.1.3) and is not carried over. After
/// `CLEAR_FEATURE(ENDPOINT_HALT)` the toggle restarts at DATA0 (USB 2.0
/// 9.4.5), so `keep_toggle` is false then.
pub fn overlay_token(current: u32, keep_toggle: bool, speed: Speed) -> u32 {
    let mut keep = 0;
    if keep_toggle {
        keep |= token::DATA_TOGGLE;
    }
    if speed == Speed::High {
        keep |= token::PING;
    }
    current & keep
}

/// Queue head (EHCI 3.6; Appendix B for the 64-bit overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueHead {
    /// Horizontal link (`link::qh` or `link::TERMINATE`).
    pub horizontal: u32,
    pub address: u8,
    pub endpoint: u8,
    pub speed: Speed,
    pub max_packet: u16,
    /// Head of the reclamation list (H, only the async schedule's head).
    pub head: bool,
    /// Data toggle from the qTD (control endpoints) instead of the overlay.
    pub toggle_from_qtd: bool,
    /// Full- or low-speed control endpoint (C, 3.6.2).
    pub control_split: bool,
    /// Interrupt schedule masks (0 for the async schedule).
    pub s_mask: u8,
    pub c_mask: u8,
    /// Translator for split transactions (full and low speed).
    pub tt: Option<Tt>,
}

impl QueueHead {
    pub const WORDS_32: usize = 12;
    pub const WORDS_64: usize = 17;
    /// Bytes reserved per queue head (32-byte alignment, 68-byte layout).
    pub const STRIDE: u64 = 96;
    /// Offset of the transfer overlay's Next qTD Pointer and its token.
    pub const NEXT_QTD: u64 = 16;
    pub const ALT_NEXT_QTD: u64 = 20;
    pub const TOKEN: u64 = 24;

    /// The dwords of an idle queue head: no current qTD, the overlay
    /// pointing nowhere and not active, so the controller skips it until
    /// software gives it qTDs. NAK reload is 0 (the NAK counter is not
    /// used, 4.9) and one transaction per micro-frame (Mult = 1).
    pub fn words(&self) -> [u32; Self::WORDS_64] {
        let mut w = [0u32; Self::WORDS_64];
        w[0] = self.horizontal;
        w[1] = (self.address as u32 & 0x7F)
            | (self.endpoint as u32 & 0xF) << 8
            | (self.speed as u32) << 12
            | (self.toggle_from_qtd as u32) << 14
            | (self.head as u32) << 15
            | (self.max_packet as u32 & 0x7FF) << 16
            | (self.control_split as u32) << 27;
        let (hub, port) = self.tt.map_or((0, 0), |t| (t.hub, t.port));
        w[2] = self.s_mask as u32
            | (self.c_mask as u32) << 8
            | (hub as u32 & 0x7F) << 16
            | (port as u32 & 0x7F) << 23
            | 1 << 30;
        w[4] = link::TERMINATE;
        w[5] = link::TERMINATE;
        w
    }
}

/// How an interrupt endpoint is placed in the periodic schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupt {
    /// Every how many frames (a power of two, at most `MAX_PERIOD_FRAMES`).
    pub period_frames: u16,
    pub s_mask: u8,
    pub c_mask: u8,
}

/// Frame list entries (EHCI 2.3.1: 1024 by default, 4 KiB).
pub const FRAME_LIST_LEN: usize = 1024;
/// Longest period used: a HID device is polled at least every 32 ms. The
/// host may poll an interrupt endpoint more often than it asks for
/// (USB 2.0 5.7.4), never less.
pub const MAX_PERIOD_FRAMES: u16 = 32;

/// Schedule of an interrupt endpoint from `bInterval` (USB 2.0 9.6.6):
/// at full and low speed it is the longest period in ms; at high speed the
/// period is `2^(bInterval-1)` micro-frames. The period is rounded down to
/// a power of two. High speed below one frame uses several micro-frames of
/// each frame; full and low speed go through a translator, with the
/// start-split in micro-frame 0 and complete-splits in 2, 3 and 4
/// (EHCI 4.12.2).
pub fn interrupt_schedule(speed: Speed, b_interval: u8) -> Interrupt {
    match speed {
        Speed::High => {
            let exp = b_interval.clamp(1, 16) - 1;
            let (period_frames, s_mask) = match exp {
                0 => (1, 0xFF),
                1 => (1, 0x55),
                2 => (1, 0x11),
                e => (1u16 << (e - 3).min(5), 0x01),
            };
            Interrupt {
                period_frames: period_frames.min(MAX_PERIOD_FRAMES),
                s_mask,
                c_mask: 0,
            }
        }
        Speed::Full | Speed::Low => {
            let ms = b_interval.max(1) as u16;
            let pow2 = 1u16 << (15 - ms.leading_zeros() as u16);
            Interrupt {
                period_frames: pow2.min(MAX_PERIOD_FRAMES),
                s_mask: 0x01,
                c_mask: 0x1C,
            }
        }
    }
}

/// Frame list entry `index` when the interrupt chain starting at `chain`
/// runs every `period` frames (a power of two).
pub fn periodic_entry(index: usize, period: u16, chain: u32) -> u32 {
    if period > 0 && index.is_multiple_of(period as usize) {
        chain
    } else {
        link::TERMINATE
    }
}

/// USB 2.0 hubs (chapter 11).
pub mod hub {
    use super::{SetupPacket, Speed};

    /// Hub class device code (USB 2.0 11.23.1).
    pub const CLASS: u8 = 9;
    /// Hub descriptor type (11.23.2.1).
    pub const DESCRIPTOR_TYPE: u8 = 0x29;
    /// Bytes requested for the descriptor (7 fixed plus the variable
    /// bitmaps of up to 15 ports).
    pub const DESCRIPTOR_LEN: u16 = 11;
    /// Deepest tier (USB 2.0 4.1.1: five hubs between host and device).
    pub const MAX_DEPTH: u8 = 5;

    /// Port feature selectors (Table 11-17).
    pub mod feature {
        pub const PORT_ENABLE: u16 = 1;
        pub const PORT_RESET: u16 = 4;
        pub const PORT_POWER: u16 = 8;
        pub const C_PORT_CONNECTION: u16 = 16;
        pub const C_PORT_ENABLE: u16 = 17;
        pub const C_PORT_SUSPEND: u16 = 18;
        pub const C_PORT_OVER_CURRENT: u16 = 19;
        pub const C_PORT_RESET: u16 = 20;
    }

    /// What the driver uses of the hub descriptor (11.23.2.1).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Descriptor {
        pub ports: u8,
        pub characteristics: u16,
        /// bPwrOn2PwrGood in ms (the field counts 2 ms units).
        pub power_on_ms: u16,
    }

    impl Descriptor {
        pub fn parse(d: &[u8]) -> Option<Self> {
            if d.len() < 7 || (d[0] as usize) < 7 || d[1] != DESCRIPTOR_TYPE {
                return None;
            }
            Some(Self {
                ports: d[2],
                characteristics: u16::from_le_bytes([d[3], d[4]]),
                power_on_ms: d[5] as u16 * 2,
            })
        }

        /// Logical Power Switching Mode (bits 1:0): `1x` means the hub has
        /// no power switches and its ports are always powered.
        pub fn power_switching(&self) -> bool {
            self.characteristics & 0b10 == 0
        }
    }

    /// wPortStatus and wPortChange (11.24.2.7).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PortStatus {
        pub status: u16,
        pub change: u16,
    }

    impl PortStatus {
        const CONNECTION: u16 = 1 << 0;
        const ENABLE: u16 = 1 << 1;
        const OVER_CURRENT: u16 = 1 << 3;
        const RESET: u16 = 1 << 4;
        const POWER: u16 = 1 << 8;
        const LOW_SPEED: u16 = 1 << 9;
        const HIGH_SPEED: u16 = 1 << 10;
        /// Change bits (11.24.2.7.2): connection, enable, suspend, over-current, reset.
        const C_CONNECTION: u16 = 1 << 0;
        const C_OVER_CURRENT: u16 = 1 << 3;
        const C_RESET: u16 = 1 << 4;

        pub fn parse(b: &[u8]) -> Option<Self> {
            (b.len() >= 4).then(|| Self {
                status: u16::from_le_bytes([b[0], b[1]]),
                change: u16::from_le_bytes([b[2], b[3]]),
            })
        }
        pub fn connected(&self) -> bool {
            self.status & Self::CONNECTION != 0
        }
        pub fn enabled(&self) -> bool {
            self.status & Self::ENABLE != 0
        }
        pub fn over_current(&self) -> bool {
            self.status & Self::OVER_CURRENT != 0
        }
        pub fn resetting(&self) -> bool {
            self.status & Self::RESET != 0
        }
        pub fn powered(&self) -> bool {
            self.status & Self::POWER != 0
        }
        pub fn connection_changed(&self) -> bool {
            self.change & Self::C_CONNECTION != 0
        }
        pub fn reset_done(&self) -> bool {
            self.change & Self::C_RESET != 0
        }
        pub fn over_current_changed(&self) -> bool {
            self.change & Self::C_OVER_CURRENT != 0
        }
        /// Speed of the attached device (bits 9 and 10).
        pub fn speed(&self) -> Speed {
            if self.status & Self::LOW_SPEED != 0 {
                Speed::Low
            } else if self.status & Self::HIGH_SPEED != 0 {
                Speed::High
            } else {
                Speed::Full
            }
        }

        /// The `C_PORT_*` features to clear to acknowledge every change
        /// bit set (11.24.2.7.2), in bit order; the count is returned.
        pub fn changes_to_clear(&self, out: &mut [u16; 5]) -> usize {
            const FEATURES: [u16; 5] = [
                feature::C_PORT_CONNECTION,
                feature::C_PORT_ENABLE,
                feature::C_PORT_SUSPEND,
                feature::C_PORT_OVER_CURRENT,
                feature::C_PORT_RESET,
            ];
            let mut n = 0;
            for (bit, f) in FEATURES.iter().enumerate() {
                if self.change & (1 << bit) != 0 {
                    out[n] = *f;
                    n += 1;
                }
            }
            n
        }
    }

    /// `GetHubDescriptor` (11.24.2.5).
    pub fn get_descriptor(length: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0xA0,
            request: 6,
            value: (DESCRIPTOR_TYPE as u16) << 8,
            index: 0,
            length,
        }
    }
    /// `GetPortStatus` (11.24.2.7): four bytes.
    pub fn get_port_status(port: u8) -> SetupPacket {
        SetupPacket {
            request_type: 0xA3,
            request: 0,
            value: 0,
            index: port as u16,
            length: 4,
        }
    }
    /// `SetPortFeature` (11.24.2.13).
    pub fn set_port_feature(port: u8, feature: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x23,
            request: 3,
            value: feature,
            index: port as u16,
            length: 0,
        }
    }
    /// Endpoint type of `ClearTTBuffer` (USB 2.0 9.6.6 bmAttributes).
    pub const TT_CONTROL: u8 = 0;
    pub const TT_BULK: u8 = 2;

    /// `ClearTTBuffer` (11.24.2.3): after a full- or low-speed control or
    /// bulk transfer through a translator was aborted (timeout or
    /// transaction errors), the translator's buffer for that endpoint is
    /// freed (11.17.5). wIndex is the TT port: 1, since the hub runs a
    /// single translator (multi-TT hubs stay in their default setting).
    pub fn clear_tt_buffer(address: u8, endpoint: u8, ep_type: u8, dir_in: bool) -> SetupPacket {
        SetupPacket {
            request_type: 0x23,
            request: 8,
            value: (endpoint as u16 & 0xF)
                | (address as u16 & 0x7F) << 4
                | (ep_type as u16 & 3) << 11
                | (dir_in as u16) << 15,
            index: 1,
            length: 0,
        }
    }

    /// `ClearPortFeature` (11.24.2.2).
    pub fn clear_port_feature(port: u8, feature: u16) -> SetupPacket {
        SetupPacket {
            request_type: 0x23,
            request: 1,
            value: feature,
            index: port as u16,
            length: 0,
        }
    }
}

/// `SET_ADDRESS` (USB 2.0 9.4.6).
/// How long a polling driver may wait before it looks at the schedules
/// and ports again (ms): `busy` (devices or work that need watching)
/// gives `poll_ms`; otherwise `idle_ms`, never past the next hub poll
/// (`next_hub_poll`) or a client read's deadline, and at least 1 ms.
pub fn poll_wait(
    now: u64,
    busy: bool,
    poll_ms: u64,
    idle_ms: u64,
    next_hub_poll: u64,
    read_deadline: Option<u64>,
) -> u64 {
    if busy {
        return poll_ms;
    }
    let mut wait = idle_ms.min(next_hub_poll.saturating_sub(now));
    if let Some(deadline) = read_deadline {
        wait = wait.min(deadline.saturating_sub(now));
    }
    wait.clamp(1, idle_ms.max(1))
}

pub fn set_address(address: u8) -> SetupPacket {
    SetupPacket {
        request_type: 0x00,
        request: 5,
        value: address as u16,
        index: 0,
        length: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_driver_waits_long_but_never_past_its_deadlines() {
        // Busy: the short period, whatever the deadlines.
        assert_eq!(poll_wait(1000, true, 8, 250, 1001, Some(1001)), 8);
        // Idle: the long period...
        assert_eq!(poll_wait(1000, false, 8, 250, 5000, None), 250);
        // ...cut by the next hub poll or a read's deadline...
        assert_eq!(poll_wait(1000, false, 8, 250, 1100, None), 100);
        assert_eq!(poll_wait(1000, false, 8, 250, 5000, Some(1020)), 20);
        // ...and at least 1 ms when one is already due.
        assert_eq!(poll_wait(1000, false, 8, 250, 900, None), 1);
        assert_eq!(poll_wait(1000, false, 8, 250, 5000, Some(1000)), 1);
    }

    #[test]
    fn parameters_and_registers() {
        // Intel 7 series EHCI: 2 ports, no companions, 64-bit, EECP 0x68.
        let p = Params::decode(0x0020_0002, 0x0000_6881);
        assert_eq!(p.ports, 2);
        assert!(!p.port_power_control);
        assert_eq!(p.companions, 0);
        assert!(p.addressing_64);
        assert_eq!(p.eecp, 0x68);
        // QEMU usb-ehci: HCSPARAMS 0x06 (6 ports, no power switches, no
        // companions), HCCPARAMS 0x80 (32-bit, no extended capabilities).
        let q = Params::decode(0x0000_0006, 0x0000_0080);
        assert_eq!(q.eecp, 0);
        assert_eq!(
            (q.ports, q.port_power_control, q.addressing_64),
            (6, false, false)
        );
        let c = Params::decode(3 << 12 | 1 << 4 | 6, 0);
        assert_eq!(c.companions, 3);
        assert_eq!(op::portsc(1), 0x44);
        assert_eq!(op::portsc(6), 0x58);
    }

    #[test]
    fn port_writes_never_echo_change_bits() {
        let v = portsc::CONNECTED | portsc::CONNECT_CHANGE | portsc::ENABLED | portsc::POWER;
        // Starting a reset: PR set, PE cleared (2.3.9), the change bit not echoed.
        let w = portsc::write_value(v, portsc::RESET, portsc::ENABLED);
        assert_eq!(w, portsc::CONNECTED | portsc::POWER | portsc::RESET);
        // Acknowledging the connect change writes exactly that bit.
        let w = portsc::write_value(v, portsc::CONNECT_CHANGE, 0);
        assert_eq!(w & portsc::CHANGE_BITS, portsc::CONNECT_CHANGE);
        assert!(portsc::low_speed_line(0b01 << 10 | 1));
        assert!(!portsc::low_speed_line(0b10 << 10 | 1));
        let mut out = [0u8; 64];
        let n = portsc::describe(v, &mut out);
        assert_eq!(&out[..n], b"connected connect-change enabled power");
        let mut small = [0u8; 12];
        let n = portsc::describe(v, &mut small);
        assert_eq!(&small[..n], b"connected");
    }

    #[test]
    fn legacy_capability_walk_and_smi_off() {
        // 0x68: debug port (id 0x0A) -> 0x70: USBLEGSUP.
        let read = |off: u8| match off {
            0x68 => 0x0000_700A,
            0x70 => 0x0001_0001,
            _ => 0,
        };
        assert_eq!(legacy::find(0x68, read), Some(0x70));
        assert_eq!(legacy::find(0, read), None);
        // A list that loops onto itself ends.
        assert_eq!(legacy::find(0x80, |_| 0x0000_800A), None);
        // A pointer into the standard header ends.
        assert_eq!(legacy::find(0x68, |_| 0x0000_100A), None);
        let ctl = 0xE000_0000 | 0x3F | 0x7 << 13 | 0x3F << 16;
        let off = legacy::smis_disabled(ctl);
        assert_eq!(off & (0x3F | 0x7 << 13), 0, "enables cleared");
        assert_eq!(off & 0xE000_0000, 0xE000_0000, "events acknowledged");
        assert_eq!(legacy::smis_disabled(0), 0xE000_0000);
    }

    #[test]
    fn qtd_layout_and_pages() {
        let q = Qtd {
            next: link::TERMINATE,
            alt_next: link::TERMINATE,
            pid: Pid::In,
            bytes: 8192,
            toggle: true,
            buffer: 0x1_2345_6800,
        };
        let w = q.words().unwrap();
        assert_eq!(w[0], 1);
        assert_eq!(w[1], 1);
        assert_eq!(w[2] & token::ACTIVE, token::ACTIVE);
        assert_eq!(token::cerr(w[2]), 3);
        assert_eq!((w[2] >> 8) & 3, 1, "PID IN");
        assert_eq!(token::bytes(w[2]), 8192);
        assert_ne!(w[2] & token::DATA_TOGGLE, 0);
        // Offset 0x800: the 8 KiB touch three pages.
        assert_eq!(w[3], 0x2345_6800);
        assert_eq!(w[4], 0x2345_7000);
        assert_eq!(w[5], 0x2345_8000);
        assert_eq!((w[6], w[7]), (0, 0));
        assert_eq!((w[8], w[9], w[10], w[11]), (1, 1, 1, 0));
        // Too long from its offset, or across 4 GiB.
        let big = Qtd {
            bytes: QTD_MAX_BYTES,
            buffer: 0x1000_0010,
            ..q
        };
        assert!(big.words().is_none());
        assert!(Qtd {
            buffer: 0x1000_0000,
            ..big
        }
        .words()
        .is_some());
        let straddle = Qtd {
            bytes: 64,
            buffer: 0xFFFF_FFE0,
            ..q
        };
        assert!(straddle.words().is_none());
        let zero = Qtd {
            bytes: 0,
            pid: Pid::Out,
            buffer: 0,
            toggle: false,
            next: link::qtd(0x2000).unwrap(),
            alt_next: link::qtd(0x2040).unwrap(),
        };
        let w = zero.words().unwrap();
        assert_eq!(
            (w[0], w[1], token::bytes(w[2]), (w[2] >> 8) & 3),
            (0x2000, 0x2040, 0, 0)
        );
        assert!(link::qtd(0x2010).is_none());
        assert_eq!(link::qh(0x3000), Some(0x3002));
    }

    #[test]
    fn transfers_split_on_packet_boundaries() {
        let mut out = [(0u64, 0u32); 4];
        // 8 KiB at a page boundary: one qTD.
        assert_eq!(split_transfer(0x10000, 8192, 512, &mut out), Some(1));
        assert_eq!(out[0], (0x10000, 8192));
        // 64 KiB from offset 0x100: pieces end on a 512-byte boundary.
        let n = split_transfer(0x10100, 65536, 512, &mut out).unwrap();
        assert_eq!(n, 4);
        let mut total = 0;
        for (i, (at, len)) in out[..n].iter().enumerate() {
            assert_eq!(*at, 0x10100 + total as u64);
            assert!(*len <= QTD_MAX_BYTES - (*at & 0xFFF) as u32);
            if i + 1 < n {
                assert_eq!(len % 512, 0);
            }
            total += len;
        }
        assert_eq!(total, 65536);
        // Zero-length transfers are one empty qTD; no room fails.
        assert_eq!(split_transfer(0x10000, 0, 64, &mut out), Some(1));
        assert_eq!(out[0].1, 0);
        assert_eq!(split_transfer(0x10000, 200_000, 512, &mut out), None);
        assert_eq!(split_transfer(0x10000, 10, 0, &mut out), None);
    }

    #[test]
    fn completions_decode() {
        assert_eq!(decode_token(token::ACTIVE | token::CERR_3), Outcome::Active);
        assert_eq!(
            decode_token(token::CERR_3 | 5 << 16),
            Outcome::Done { remaining: 5 }
        );
        assert_eq!(
            decode_token(token::HALTED | token::CERR_3),
            Outcome::Error(TransferError::Stall)
        );
        assert_eq!(
            decode_token(token::HALTED | token::BABBLE),
            Outcome::Error(TransferError::Babble)
        );
        assert_eq!(
            decode_token(token::HALTED | token::DATA_BUFFER_ERROR),
            Outcome::Error(TransferError::DataBuffer)
        );
        assert_eq!(
            decode_token(token::HALTED | token::TRANSACTION_ERROR),
            Outcome::Error(TransferError::Transaction)
        );
        // A transaction error that left tries and then a STALL: the STALL halted it.
        assert_eq!(
            decode_token(token::HALTED | token::TRANSACTION_ERROR | 2 << 10),
            Outcome::Error(TransferError::Stall)
        );
        assert_eq!(
            decode_token(token::HALTED | token::MISSED_MICRO_FRAME | 1 << 10),
            Outcome::Error(TransferError::MissedMicroFrame)
        );
        assert_eq!(TransferError::Transaction.name(), "transaction error");
        assert_eq!(TransferError::Stall.code(), 1);
        // The overlay keeps toggle and ping only, and drops the toggle
        // after a cleared halt.
        let t = token::DATA_TOGGLE | token::HALTED | token::PING | 7 << 16;
        assert_eq!(
            overlay_token(t, true, Speed::High),
            token::DATA_TOGGLE | token::PING
        );
        assert_eq!(overlay_token(t, false, Speed::High), token::PING);
        // Full and low speed: bit 0 is the split ERR bit, never carried.
        assert_eq!(overlay_token(t, true, Speed::Low), token::DATA_TOGGLE);
        assert_eq!(overlay_token(t, false, Speed::Full), 0);
    }

    #[test]
    fn queue_head_layout() {
        let qh = QueueHead {
            horizontal: link::qh(0x4000).unwrap(),
            address: 5,
            endpoint: 2,
            speed: Speed::Low,
            max_packet: 8,
            head: false,
            toggle_from_qtd: true,
            control_split: true,
            s_mask: 0x01,
            c_mask: 0x1C,
            tt: Some(Tt { hub: 1, port: 3 }),
        };
        let w = qh.words();
        assert_eq!(w[0], 0x4002);
        assert_eq!(w[1] & 0x7F, 5);
        assert_eq!((w[1] >> 8) & 0xF, 2);
        assert_eq!((w[1] >> 12) & 3, 1, "EPS low");
        assert_ne!(w[1] & 1 << 14, 0, "DTC");
        assert_eq!(w[1] & 1 << 15, 0, "not the head");
        assert_eq!((w[1] >> 16) & 0x7FF, 8);
        assert_ne!(w[1] & 1 << 27, 0, "control endpoint flag");
        assert_eq!(w[1] >> 28, 0, "NAK reload 0");
        assert_eq!(w[2] & 0xFF, 0x01);
        assert_eq!((w[2] >> 8) & 0xFF, 0x1C);
        assert_eq!((w[2] >> 16) & 0x7F, 1);
        assert_eq!((w[2] >> 23) & 0x7F, 3);
        assert_eq!(w[2] >> 30, 1, "Mult 1");
        assert_eq!((w[3], w[4], w[5], w[6]), (0, 1, 1, 0));
        let head = QueueHead {
            head: true,
            speed: Speed::High,
            tt: None,
            control_split: false,
            s_mask: 0,
            c_mask: 0,
            ..qh
        };
        let w = head.words();
        assert_ne!(w[1] & 1 << 15, 0);
        assert_eq!((w[1] >> 12) & 3, 2);
        assert_eq!(w[2], 1 << 30);
    }

    #[test]
    fn interrupt_schedules() {
        // Low-speed keyboard, bInterval 10 ms: every 8 frames, split masks.
        let k = interrupt_schedule(Speed::Low, 10);
        assert_eq!((k.period_frames, k.s_mask, k.c_mask), (8, 0x01, 0x1C));
        assert_eq!(interrupt_schedule(Speed::Full, 1).period_frames, 1);
        assert_eq!(interrupt_schedule(Speed::Full, 0).period_frames, 1);
        assert_eq!(interrupt_schedule(Speed::Full, 255).period_frames, 32);
        assert_eq!(interrupt_schedule(Speed::Full, 64).period_frames, 32);
        // High speed: 2^(b-1) micro-frames.
        let h = |b| {
            let i = interrupt_schedule(Speed::High, b);
            (i.period_frames, i.s_mask, i.c_mask)
        };
        assert_eq!(h(1), (1, 0xFF, 0));
        assert_eq!(h(2), (1, 0x55, 0));
        assert_eq!(h(3), (1, 0x11, 0));
        assert_eq!(h(4), (1, 0x01, 0));
        assert_eq!(h(7), (8, 0x01, 0));
        assert_eq!(h(16), (32, 0x01, 0));
        assert_eq!(h(0), (1, 0xFF, 0));
        assert_eq!(periodic_entry(0, 8, 0x5002), 0x5002);
        assert_eq!(periodic_entry(8, 8, 0x5002), 0x5002);
        assert_eq!(periodic_entry(3, 8, 0x5002), link::TERMINATE);
        assert_eq!(periodic_entry(3, 1, 0x5002), 0x5002);
        assert_eq!(periodic_entry(3, 0, 0x5002), link::TERMINATE);
    }

    #[test]
    fn transaction_translators() {
        let tt = Tt { hub: 2, port: 4 };
        assert_eq!(tt_for(Speed::High, 2, None, 4, Speed::High), Ok(None));
        assert_eq!(tt_for(Speed::High, 2, None, 4, Speed::Low), Ok(Some(tt)));
        assert_eq!(tt_for(Speed::High, 2, None, 4, Speed::Full), Ok(Some(tt)));
        // Below a full-speed hub the translator of that hub is kept.
        assert_eq!(
            tt_for(Speed::Full, 7, Some(tt), 1, Speed::Low),
            Ok(Some(tt))
        );
        assert!(tt_for(Speed::Full, 7, Some(tt), 1, Speed::High).is_err());
        assert!(tt_for(Speed::Full, 7, None, 1, Speed::Full).is_err());
        assert_eq!(Speed::High.default_ep0_max_packet(), 64);
        assert_eq!(Speed::Low.default_ep0_max_packet(), 8);
        assert_eq!(Speed::Full.name(), "full");
    }

    #[test]
    fn hub_descriptor_status_and_requests() {
        // Intel rate matching hub: 8 ports, individual power switching,
        // 50 ms power-on.
        let d = [9, 0x29, 8, 0x09, 0x00, 25, 0, 0, 0xFF];
        let h = hub::Descriptor::parse(&d).unwrap();
        assert_eq!((h.ports, h.power_on_ms), (8, 50));
        assert!(h.power_switching());
        let no_switch = hub::Descriptor::parse(&[7, 0x29, 4, 0x02, 0, 1, 0]).unwrap();
        assert!(!no_switch.power_switching());
        assert!(hub::Descriptor::parse(&[7, 0x02, 4, 0, 0, 1, 0]).is_none());
        assert!(hub::Descriptor::parse(&[6, 0x29, 4, 0, 0, 1]).is_none());

        // Connected, enabled, powered, high speed; connection and reset changed.
        let s = hub::PortStatus::parse(&[0x03, 0x05, 0x11, 0x00]).unwrap();
        assert!(s.connected() && s.enabled() && s.powered());
        assert!(!s.over_current() && !s.resetting());
        assert_eq!(s.speed(), Speed::High);
        assert!(s.connection_changed() && s.reset_done());
        let mut f = [0u16; 5];
        let n = s.changes_to_clear(&mut f);
        assert_eq!(
            &f[..n],
            &[hub::feature::C_PORT_CONNECTION, hub::feature::C_PORT_RESET]
        );
        let low = hub::PortStatus::parse(&[0x01, 0x03, 0, 0]).unwrap();
        assert_eq!(low.speed(), Speed::Low);
        let full = hub::PortStatus::parse(&[0x19, 0x01, 0x0F, 0]).unwrap();
        assert_eq!(full.speed(), Speed::Full);
        assert!(full.over_current() && full.resetting());
        assert!(full.over_current_changed() && !s.over_current_changed());
        assert_eq!(full.changes_to_clear(&mut f), 4);
        assert!(hub::PortStatus::parse(&[0, 0, 0]).is_none());

        let r = hub::get_port_status(3).to_bytes();
        assert_eq!(r, [0xA3, 0, 0, 0, 3, 0, 4, 0]);
        let r = hub::set_port_feature(2, hub::feature::PORT_RESET).to_bytes();
        assert_eq!(r, [0x23, 3, 4, 0, 2, 0, 0, 0]);
        let r = hub::clear_port_feature(2, hub::feature::C_PORT_RESET).to_bytes();
        assert_eq!(r, [0x23, 1, 20, 0, 2, 0, 0, 0]);
        let r = hub::get_descriptor(hub::DESCRIPTOR_LEN).to_bytes();
        assert_eq!(r, [0xA0, 6, 0, 0x29, 0, 0, 11, 0]);
        assert_eq!(set_address(9).to_bytes(), [0, 5, 9, 0, 0, 0, 0, 0]);
        // Bulk IN endpoint 1 of address 5: 1 | 5 << 4 | 2 << 11 | 1 << 15.
        let r = hub::clear_tt_buffer(5, 1, hub::TT_BULK, true).to_bytes();
        assert_eq!(r, [0x23, 8, 0x51, 0x90, 1, 0, 0, 0]);
        let r = hub::clear_tt_buffer(3, 0, hub::TT_CONTROL, false).to_bytes();
        assert_eq!(r, [0x23, 8, 0x30, 0x00, 1, 0, 0, 0]);
    }

    #[test]
    fn operational_registers_must_fit_the_bar() {
        // Intel 7 series: 1 KiB BAR, CAPLENGTH 0x20, 3 ports.
        let p = Params::decode(3, 0);
        assert!(p.registers_fit(0x20, 0x400));
        assert!(!p.registers_fit(0xFF, 0x100));
        assert!(!Params::decode(15, 0).registers_fit(0x3F0, 0x400));
    }
}
