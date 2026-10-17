// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! xHCI (Phase 25, second part): everything the ring 3 driver needs to
//! know about the controller and the USB protocol without touching hardware:
//! register offsets and bits, TRBs (commands, transfers and
//! events), rings with cycle bit and link TRB, slot and endpoint
//! contexts (32 or 64 bytes), event ring segment table,
//! SETUP packets and USB descriptors. Tested on the host; the driver only
//! copies these structures into DMA memory and writes MMIO.

#![no_std]

/// Capability registers (from the BAR0 base).
pub mod cap {
    pub const CAPLENGTH: u64 = 0x00;
    pub const HCIVERSION: u64 = 0x02;
    pub const HCSPARAMS1: u64 = 0x04;
    pub const HCSPARAMS2: u64 = 0x08;
    pub const HCCPARAMS1: u64 = 0x10;
    pub const DBOFF: u64 = 0x14;
    pub const RTSOFF: u64 = 0x18;
}

/// Operational registers (from `base + CAPLENGTH`).
pub mod op {
    pub const USBCMD: u64 = 0x00;
    pub const USBSTS: u64 = 0x04;
    pub const PAGESIZE: u64 = 0x08;
    pub const CRCR: u64 = 0x18;
    pub const DCBAAP: u64 = 0x30;
    pub const CONFIG: u64 = 0x38;
    /// Port registers: `PORTSC` of port `n` (from 1) at `0x400 + 0x10 * (n - 1)`.
    pub const PORT_BASE: u64 = 0x400;
    pub const PORT_STRIDE: u64 = 0x10;

    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;
    pub const CMD_INTE: u32 = 1 << 2;
    pub const STS_HALTED: u32 = 1 << 0;
    /// Host System Error: a DMA or system bus fault seen by the controller.
    pub const STS_HSE: u32 = 1 << 2;
    pub const STS_EINT: u32 = 1 << 3;
    pub const STS_PORT_CHANGE: u32 = 1 << 4;
    pub const STS_NOT_READY: u32 = 1 << 11;
    /// Host Controller Error: the controller stopped on an internal error.
    pub const STS_HCE: u32 = 1 << 12;
    /// Ring Cycle State of the command ring.
    pub const CRCR_RCS: u64 = 1 << 0;

    pub const PORTSC_CCS: u32 = 1 << 0;
    pub const PORTSC_PED: u32 = 1 << 1;
    pub const PORTSC_OCA: u32 = 1 << 3;
    pub const PORTSC_RESET: u32 = 1 << 4;
    pub const PORTSC_POWER: u32 = 1 << 9;
    pub const PORTSC_CSC: u32 = 1 << 17;
    pub const PORTSC_PEC: u32 = 1 << 18;
    pub const PORTSC_WRC: u32 = 1 << 19;
    pub const PORTSC_OCC: u32 = 1 << 20;
    pub const PORTSC_PRC: u32 = 1 << 21;
    pub const PORTSC_PLC: u32 = 1 << 22;
    pub const PORTSC_CEC: u32 = 1 << 23;
    /// RW1C change bits: written as 1 to clear them, never by accident.
    pub const PORTSC_CHANGE_BITS: u32 = 0x00FE_0000;
    /// The device behind the port disconnected, reconnected or was reset:
    /// the slot addressing it no longer does (xHCI 4.19.2, USB 2.0 9.1).
    pub const PORTSC_DEVICE_LOST: u32 = PORTSC_CSC | PORTSC_PRC | PORTSC_WRC;

    pub fn port_speed(portsc: u32) -> u8 {
        ((portsc >> 10) & 0xF) as u8
    }

    /// Port Link State (PLS, bits 8:5): 0 is U0.
    pub fn port_link_state(portsc: u32) -> u8 {
        ((portsc >> 5) & 0xF) as u8
    }

    /// PORTSC decoded as text (xHCI 5.4.8) for the console:
    /// `CCS=1 PED=1 OCA=0 PR=0 PLS=0 PP=1 speed=3 CSC=0 PEC=0 WRC=0 OCC=0
    /// PRC=0 PLC=0 CEC=0`. `out` needs 96 bytes.
    pub fn describe_portsc(portsc: u32, out: &mut [u8]) -> &str {
        const FLAGS: [(&str, u32); 12] = [
            ("CCS", PORTSC_CCS),
            ("PED", PORTSC_PED),
            ("OCA", PORTSC_OCA),
            ("PR", PORTSC_RESET),
            ("PLS", 0),
            ("PP", PORTSC_POWER),
            ("speed", 0),
            ("CSC", PORTSC_CSC),
            ("PEC", PORTSC_PEC),
            ("WRC", PORTSC_WRC),
            ("OCC", PORTSC_OCC),
            ("PRC", PORTSC_PRC),
        ];
        let mut w = super::TextOut { out, len: 0 };
        for (i, (name, bit)) in FLAGS.iter().enumerate() {
            if i > 0 {
                w.put(" ");
            }
            w.put(name);
            w.put("=");
            let value = match *name {
                "PLS" => port_link_state(portsc),
                "speed" => port_speed(portsc),
                _ => (portsc & bit != 0) as u8,
            };
            w.num(value);
        }
        w.put(" PLC=");
        w.num((portsc & PORTSC_PLC != 0) as u8);
        w.put(" CEC=");
        w.num((portsc & PORTSC_CEC != 0) as u8);
        let len = w.len;
        core::str::from_utf8(&out[..len]).unwrap_or("")
    }
}

/// Minimal text writer over a byte buffer (console lines without `alloc`).
struct TextOut<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl TextOut<'_> {
    fn put(&mut self, s: &str) {
        let n = s.len().min(self.out.len() - self.len);
        self.out[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
    }
    fn num(&mut self, mut v: u8) {
        let mut digits = [0u8; 3];
        let mut n = 0;
        loop {
            digits[n] = b'0' + v % 10;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            self.put(core::str::from_utf8(&digits[i..i + 1]).unwrap_or("?"));
        }
    }
}

/// Runtime registers (from `base + RTSOFF`): interrupter 0.
pub mod rt {
    pub const IMAN: u64 = 0x20;
    pub const IMOD: u64 = 0x24;
    pub const ERSTSZ: u64 = 0x28;
    pub const ERSTBA: u64 = 0x30;
    pub const ERDP: u64 = 0x38;
    pub const IMAN_PENDING: u32 = 1 << 0;
    pub const IMAN_ENABLE: u32 = 1 << 1;
    /// Event Handler Busy (RW1C) in ERDP.
    pub const ERDP_EHB: u64 = 1 << 3;
}

/// Speeds of the PORTSC Port Speed field and of the slot context.
pub mod speed {
    pub const FULL: u8 = 1;
    pub const LOW: u8 = 2;
    pub const HIGH: u8 = 3;
    pub const SUPER: u8 = 4;

    /// Name of a PORTSC / slot speed value for the console.
    pub fn name(speed: u8) -> &'static str {
        match speed {
            FULL => "full",
            LOW => "low",
            HIGH => "high",
            SUPER => "super",
            _ => "unknown",
        }
    }

    /// Default endpoint 0 max packet size for each speed.
    pub fn default_ep0_max_packet(speed: u8) -> u16 {
        match speed {
            LOW | FULL => 8,
            HIGH => 64,
            _ => 512,
        }
    }
}

/// Decoded HCSPARAMS1 / HCSPARAMS2 / HCCPARAMS1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub max_slots: u8,
    pub max_interrupters: u16,
    pub max_ports: u8,
    pub scratchpad_buffers: u16,
    pub context_size_64: bool,
    pub addressing_64: bool,
}

impl Params {
    pub fn decode(hcsparams1: u32, hcsparams2: u32, hccparams1: u32) -> Self {
        let hi = (hcsparams2 >> 21) & 0x1F;
        let lo = (hcsparams2 >> 27) & 0x1F;
        Params {
            max_slots: (hcsparams1 & 0xFF) as u8,
            max_interrupters: ((hcsparams1 >> 8) & 0x7FF) as u16,
            max_ports: (hcsparams1 >> 24) as u8,
            scratchpad_buffers: ((hi << 5) | lo) as u16,
            context_size_64: hccparams1 & (1 << 2) != 0,
            addressing_64: hccparams1 & 1 != 0,
        }
    }

    /// Bytes of DMA buffer the driver needs: 64 KiB for the DCBAA,
    /// rings, ERST and device contexts, plus one page for each
    /// scratchpad the controller asks for (2012 Intel parts ask for dozens;
    /// QEMU none) and one spare for scratchpad alignment.
    pub fn dma_arena_bytes(&self) -> u64 {
        const BASE: u64 = 64 * 1024;
        const PAGE: u64 = 4096;
        BASE + (self.scratchpad_buffers as u64 + 1) * PAGE
    }

    /// Bytes of each context (slot or endpoint).
    pub fn context_bytes(&self) -> usize {
        if self.context_size_64 {
            64
        } else {
            32
        }
    }
}

/// Transfer Request Block: 16 bytes little endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Trb {
    pub parameter: u64,
    pub status: u32,
    pub control: u32,
}

/// TRB types (bits 15:10 of `control`).
pub mod trb_type {
    pub const NORMAL: u32 = 1;
    pub const SETUP_STAGE: u32 = 2;
    pub const DATA_STAGE: u32 = 3;
    pub const STATUS_STAGE: u32 = 4;
    pub const LINK: u32 = 6;
    pub const ENABLE_SLOT: u32 = 9;
    pub const DISABLE_SLOT: u32 = 10;
    pub const ADDRESS_DEVICE: u32 = 11;
    pub const CONFIGURE_ENDPOINT: u32 = 12;
    pub const EVALUATE_CONTEXT: u32 = 13;
    pub const RESET_ENDPOINT: u32 = 14;
    pub const STOP_ENDPOINT: u32 = 15;
    pub const SET_TR_DEQUEUE: u32 = 16;
    pub const NOOP_COMMAND: u32 = 23;
    pub const TRANSFER_EVENT: u32 = 32;
    pub const COMMAND_COMPLETION: u32 = 33;
    pub const PORT_STATUS_CHANGE: u32 = 34;
}

/// Event completion codes (xHCI 6.4.5). The driver uses 0, which the
/// controller never produces, for "no event" (a timeout).
pub mod completion {
    pub const NO_EVENT: u8 = 0;
    pub const SUCCESS: u8 = 1;
    pub const DATA_BUFFER_ERROR: u8 = 2;
    pub const BABBLE_DETECTED: u8 = 3;
    /// The device did not answer or answered badly on the bus, CErr times
    /// in a row; the endpoint is Halted (4.10.2.1).
    pub const USB_TRANSACTION_ERROR: u8 = 4;
    pub const TRB_ERROR: u8 = 5;
    /// The endpoint answered STALL (it stays halted until Reset Endpoint).
    pub const STALL: u8 = 6;
    pub const SHORT_PACKET: u8 = 13;
    /// A command was issued to a slot or endpoint in the wrong state (for
    /// example Reset Endpoint on an endpoint that is not Halted).
    pub const CONTEXT_STATE_ERROR: u8 = 19;
    /// A Stop Endpoint command retired the TD in progress (4.6.9).
    pub const STOPPED: u8 = 26;
    pub const STOPPED_LENGTH_INVALID: u8 = 27;
    pub const STOPPED_SHORT_PACKET: u8 = 28;
    pub const SPLIT_TRANSACTION_ERROR: u8 = 36;

    /// The Transfer Event a Stop Endpoint command produces for the TD it
    /// interrupted.
    pub fn is_stopped(code: u8) -> bool {
        matches!(
            code,
            STOPPED | STOPPED_LENGTH_INVALID | STOPPED_SHORT_PACKET
        )
    }

    /// Name of a code for the console (empty if unknown).
    pub fn name(code: u8) -> &'static str {
        match code {
            NO_EVENT => "no event",
            SUCCESS => "Success",
            DATA_BUFFER_ERROR => "Data Buffer Error",
            BABBLE_DETECTED => "Babble Detected",
            USB_TRANSACTION_ERROR => "USB Transaction Error",
            TRB_ERROR => "TRB Error",
            STALL => "Stall Error",
            SHORT_PACKET => "Short Packet",
            CONTEXT_STATE_ERROR => "Context State Error",
            STOPPED => "Stopped",
            STOPPED_LENGTH_INVALID => "Stopped - Length Invalid",
            STOPPED_SHORT_PACKET => "Stopped - Short Packet",
            SPLIT_TRANSACTION_ERROR => "Split Transaction Error",
            _ => "",
        }
    }
}

/// Endpoint Context states (word 0 bits 2:0, xHCI 6.2.3 and figure 4-5).
pub mod ep_state {
    pub const DISABLED: u8 = 0;
    pub const RUNNING: u8 = 1;
    pub const HALTED: u8 = 2;
    pub const STOPPED: u8 = 3;
    pub const ERROR: u8 = 4;

    pub fn name(state: u8) -> &'static str {
        match state {
            DISABLED => "Disabled",
            RUNNING => "Running",
            HALTED => "Halted",
            STOPPED => "Stopped",
            ERROR => "Error",
            _ => "",
        }
    }
}

/// Endpoint state and TR Dequeue Pointer read from an Output Endpoint
/// Context (the first 16 bytes). `None` if the slice is too short.
pub fn endpoint_status(ctx: &[u8]) -> Option<(u8, u64)> {
    if ctx.len() < 16 {
        return None;
    }
    let w0 = u32::from_le_bytes(ctx[..4].try_into().ok()?);
    let lo = u32::from_le_bytes(ctx[8..12].try_into().ok()?);
    let hi = u32::from_le_bytes(ctx[12..16].try_into().ok()?);
    Some(((w0 & 7) as u8, ((hi as u64) << 32 | lo as u64) & !0xF))
}

/// What the driver does to an endpoint whose transfer ended with
/// completion code `code` (`completion::NO_EVENT` for a timeout) while the
/// Endpoint Context reads `state` (xHCI 4.8.3: doorbells are ignored
/// unless the endpoint is Running).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRecovery {
    /// Running and nothing in progress to retire: nothing to do.
    Nothing,
    /// Running with a TD that never completed: Stop Endpoint (4.6.9), then
    /// Set TR Dequeue past it (4.6.10) before the ring is used again.
    StopAndRetire,
    /// Stopped by someone else (a Stopped Transfer Event): Set TR Dequeue
    /// past the interrupted TD (4.6.10).
    SetDequeue,
    /// Halted (4.10.2.1): Reset Endpoint with TSP = 0 (4.6.8), Set TR
    /// Dequeue (4.6.10) and, on a bulk endpoint, `CLEAR_FEATURE(ENDPOINT_HALT)`
    /// to the device for every halting code: the reset restarts the host's
    /// data toggle at DATA0 and only that request restarts the device's
    /// (USB 2.0 9.4.5); otherwise mismatched packets are ACKed and
    /// discarded (8.6.4). EP0 is exempt (a control pipe's STALL ends with
    /// the next SETUP, 8.5.3.4).
    ResetHalted,
    /// Disabled or Error: no command puts it back; the device must be
    /// configured again.
    Reenumerate,
}

impl EndpointRecovery {
    pub fn name(self) -> &'static str {
        match self {
            Self::Nothing => "nothing to retire",
            Self::StopAndRetire => "Stop Endpoint, Set TR Dequeue",
            Self::SetDequeue => "Set TR Dequeue",
            Self::ResetHalted => "Reset Endpoint, Set TR Dequeue, CLEAR_FEATURE(ENDPOINT_HALT)",
            Self::Reenumerate => "endpoint unusable: re-enumerate",
        }
    }
}

pub fn endpoint_recovery(code: u8, state: u8) -> EndpointRecovery {
    match state {
        ep_state::HALTED => EndpointRecovery::ResetHalted,
        ep_state::RUNNING if code == completion::NO_EVENT => EndpointRecovery::StopAndRetire,
        ep_state::RUNNING => EndpointRecovery::Nothing,
        ep_state::STOPPED => EndpointRecovery::SetDequeue,
        _ => EndpointRecovery::Reenumerate,
    }
}

/// No transfer or command starts with less than this left on its
/// deadline: a TRB enqueued and never waited for would complete into a
/// buffer the driver has already released (incident 0001, audit 2 N1).
pub const MIN_START_MS: u64 = 5;

/// `Some(min(deadline, now + timeout_ms))`, or `None` when the step must
/// not start because less than `MIN_START_MS` is left.
pub fn step_deadline(now: u64, deadline: u64, timeout_ms: u64) -> Option<u64> {
    if deadline < now.saturating_add(MIN_START_MS) {
        return None;
    }
    Some(deadline.min(now.saturating_add(timeout_ms)))
}

/// Time budget of one request. Transfers (bulk stages, a CSW re-read)
/// must end `reserve_ms` before `deadline`, so that the recovery a failure
/// needs (Reset or Stop Endpoint, Set TR Dequeue, CLEAR_FEATURE, Disable
/// Slot, each with its own command timeout) still fits; recovery commands
/// may run until `deadline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub deadline: u64,
    pub reserve_ms: u64,
}

impl Budget {
    pub fn new(now: u64, budget_ms: u64, reserve_ms: u64) -> Self {
        Self {
            deadline: now + budget_ms,
            reserve_ms,
        }
    }

    /// End of the transfer window: `deadline - reserve`.
    pub fn transfer_window(&self) -> u64 {
        self.deadline.saturating_sub(self.reserve_ms)
    }

    /// Deadline of a transfer with its own `timeout_ms`; `None` when the
    /// window is (nearly) over.
    pub fn transfer(&self, now: u64, timeout_ms: u64) -> Option<u64> {
        step_deadline(now, self.transfer_window(), timeout_ms)
    }

    /// Deadline of a recovery command with its own `timeout_ms`; `None`
    /// when the budget is (nearly) over.
    pub fn recovery(&self, now: u64, timeout_ms: u64) -> Option<u64> {
        step_deadline(now, self.deadline, timeout_ms)
    }

    /// Whether a transfer of `needed_ms` still fits in the window.
    pub fn fits_transfer(&self, now: u64, needed_ms: u64) -> bool {
        now + needed_ms <= self.transfer_window()
    }

    /// A budget of at most `budget_ms` that also ends by `enclosing`
    /// (the deadline of the step it runs in), whose transfer window is at
    /// least `min_window_ms`: the reserve shrinks when the enclosing
    /// deadline is short, and `None` says the command must not start at
    /// all because not even the minimum window fits.
    pub fn within(
        now: u64,
        enclosing: u64,
        budget_ms: u64,
        reserve_ms: u64,
        min_window_ms: u64,
    ) -> Option<Self> {
        let deadline = (now + budget_ms).min(enclosing);
        let total = deadline.saturating_sub(now);
        if total < min_window_ms {
            return None;
        }
        Some(Self {
            deadline,
            reserve_ms: reserve_ms.min(total - min_window_ms),
        })
    }
}

/// Soft Retry (xHCI 4.6.8.1): a USB Transaction Error that halted the
/// endpoint can be retried in place with Reset Endpoint TSP = 1, which
/// keeps the data toggle, and a doorbell. The TSP flag exists from xHCI
/// 1.1 (`hci_version` is HCIVERSION, BCD).
pub fn soft_retry_applies(hci_version: u16, code: u8, state: u8) -> bool {
    hci_version >= 0x0110 && code == completion::USB_TRANSACTION_ERROR && state == ep_state::HALTED
}

pub const TRB_CYCLE: u32 = 1 << 0;
const TRB_TOGGLE_CYCLE: u32 = 1 << 1;
/// Interrupt on Short Packet.
const TRB_ISP: u32 = 1 << 2;
/// Chain: the next TRB belongs to the same TD.
const TRB_CHAIN: u32 = 1 << 4;
/// Interrupt on Completion.
const TRB_IOC: u32 = 1 << 5;
/// Immediate Data (the parameter is the data, not a pointer).
const TRB_IDT: u32 = 1 << 6;
/// Transfer State Preserve (Reset Endpoint, xHCI 1.1).
const TRB_TSP: u32 = 1 << 9;
const TRB_DIR_IN: u32 = 1 << 16;

impl Trb {
    pub const LEN: usize = 16;

    pub fn trb_type(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }
    pub fn cycle(&self) -> bool {
        self.control & TRB_CYCLE != 0
    }
    pub fn completion_code(&self) -> u8 {
        (self.status >> 24) as u8
    }
    pub fn slot_id(&self) -> u8 {
        (self.control >> 24) as u8
    }
    /// Endpoint (DCI) of a Transfer Event.
    pub fn endpoint_id(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }
    /// Bytes not transferred (Transfer Event).
    pub fn residual(&self) -> u32 {
        self.status & 0x00FF_FFFF
    }
    /// Port of a Port Status Change Event.
    pub fn port_id(&self) -> u8 {
        (self.parameter >> 24) as u8
    }

    fn with_type(t: u32) -> Self {
        Trb {
            parameter: 0,
            status: 0,
            control: t << 10,
        }
    }

    pub fn link(ring_phys: u64, toggle_cycle: bool) -> Self {
        let mut t = Self::with_type(trb_type::LINK);
        t.parameter = ring_phys;
        if toggle_cycle {
            t.control |= TRB_TOGGLE_CYCLE;
        }
        t
    }
    pub fn enable_slot() -> Self {
        Self::with_type(trb_type::ENABLE_SLOT)
    }
    pub fn noop_command() -> Self {
        Self::with_type(trb_type::NOOP_COMMAND)
    }
    pub fn address_device(input_context_phys: u64, slot: u8) -> Self {
        let mut t = Self::with_type(trb_type::ADDRESS_DEVICE);
        t.parameter = input_context_phys;
        t.control |= (slot as u32) << 24;
        t
    }
    pub fn configure_endpoint(input_context_phys: u64, slot: u8) -> Self {
        let mut t = Self::with_type(trb_type::CONFIGURE_ENDPOINT);
        t.parameter = input_context_phys;
        t.control |= (slot as u32) << 24;
        t
    }
    pub fn evaluate_context(input_context_phys: u64, slot: u8) -> Self {
        let mut t = Self::with_type(trb_type::EVALUATE_CONTEXT);
        t.parameter = input_context_phys;
        t.control |= (slot as u32) << 24;
        t
    }
    pub fn disable_slot(slot: u8) -> Self {
        let mut t = Self::with_type(trb_type::DISABLE_SLOT);
        t.control |= (slot as u32) << 24;
        t
    }
    /// Reset Endpoint (4.6.8): takes endpoint `dci` out of the Halted state
    /// (it fails with Context State Error otherwise). `preserve_state`
    /// (TSP, xHCI 1.1) keeps the data toggle for a Soft Retry (4.6.8.1);
    /// without it the toggle restarts at DATA0.
    pub fn reset_endpoint(slot: u8, dci: u8, preserve_state: bool) -> Self {
        let mut t = Self::with_type(trb_type::RESET_ENDPOINT);
        t.control |= (dci as u32) << 16 | (slot as u32) << 24;
        if preserve_state {
            t.control |= TRB_TSP;
        }
        t
    }
    /// Stop Endpoint (4.6.9): the controller retires the TD in progress
    /// with a Stopped Transfer Event and leaves the endpoint Stopped.
    pub fn stop_endpoint(slot: u8, dci: u8) -> Self {
        let mut t = Self::with_type(trb_type::STOP_ENDPOINT);
        t.control |= (dci as u32) << 16 | (slot as u32) << 24;
        t
    }
    /// Set TR Dequeue Pointer: the controller follows the ring of `dci` from
    /// `pointer` (TRB address with the producer cycle bit).
    pub fn set_tr_dequeue(slot: u8, dci: u8, pointer: u64, cycle: bool) -> Self {
        let mut t = Self::with_type(trb_type::SET_TR_DEQUEUE);
        t.parameter = (pointer & !0xF) | cycle as u64;
        t.control |= (dci as u32) << 16 | (slot as u32) << 24;
        t
    }
    /// Setup Stage with the 8 bytes of the SETUP packet as immediate data.
    /// `TRT`: 0 no data, 2 OUT data, 3 IN data.
    pub fn setup_stage(setup: &SetupPacket) -> Self {
        let mut t = Self::with_type(trb_type::SETUP_STAGE);
        t.parameter = u64::from_le_bytes(setup.to_bytes());
        t.status = 8;
        let trt = if setup.length == 0 {
            0
        } else if setup.is_in() {
            3
        } else {
            2
        };
        t.control |= TRB_IDT | (trt << 16);
        t
    }
    pub fn data_stage(buffer_phys: u64, len: u16, device_to_host: bool) -> Self {
        let mut t = Self::with_type(trb_type::DATA_STAGE);
        t.parameter = buffer_phys;
        t.status = len as u32;
        if device_to_host {
            t.control |= TRB_DIR_IN;
        }
        t
    }
    /// Status Stage; its direction is the opposite of the data (or IN if
    /// there was no data). With `IOC` to know that the transfer finished.
    pub fn status_stage(device_to_host: bool) -> Self {
        let mut t = Self::with_type(trb_type::STATUS_STAGE);
        if device_to_host {
            t.control |= TRB_DIR_IN;
        }
        t.control |= TRB_IOC;
        t
    }
    /// Normal transfer (interrupt endpoint) with notification on completion,
    /// also if the packet is short.
    pub fn normal(buffer_phys: u64, len: u32) -> Self {
        Self::normal_part(buffer_phys, len, 0, false)
    }

    /// One TRB of a multi-TRB TD: `td_size` = packets left after this
    /// TRB (capped at 31), `chain` if it is not the last. Only the last one notifies.
    pub fn normal_part(buffer_phys: u64, len: u32, td_size: u32, chain: bool) -> Self {
        let mut t = Self::with_type(trb_type::NORMAL);
        t.parameter = buffer_phys;
        t.status = len | td_size.min(31) << 17;
        t.control |= if chain { TRB_CHAIN } else { TRB_IOC | TRB_ISP };
        t
    }

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut b = [0u8; Self::LEN];
        b[..8].copy_from_slice(&self.parameter.to_le_bytes());
        b[8..12].copy_from_slice(&self.status.to_le_bytes());
        b[12..].copy_from_slice(&self.control.to_le_bytes());
        b
    }
    pub fn from_bytes(b: &[u8; Self::LEN]) -> Self {
        Trb {
            parameter: u64::from_le_bytes(b[..8].try_into().unwrap()),
            status: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            control: u32::from_le_bytes(b[12..].try_into().unwrap()),
        }
    }
}

/// Producer ring (commands or transfers): `size - 1` usable TRBs and
/// a link TRB at the end with `Toggle Cycle`. Keeps the producer state;
/// the caller writes the contents wherever `next` tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProducerRing {
    pub phys: u64,
    pub size: usize,
    pub enqueue: usize,
    pub cycle: bool,
}

/// What must be written to the ring to enqueue a TRB: the position and
/// the bytes (with the correct cycle bit) and, when due, the updated
/// link TRB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub index: usize,
    pub trb: Trb,
    /// Physical address of the TRB (to match completion events).
    pub phys: u64,
    /// The link must be rewritten with this cycle before wrapping around.
    pub link: Option<(usize, Trb)>,
}

impl ProducerRing {
    pub fn new(phys: u64, size: usize) -> Self {
        assert!(size >= 2, "ring too small");
        Self {
            phys,
            size,
            enqueue: 0,
            cycle: true,
        }
    }

    /// Initial link TRB (last slot, points to the start, with toggle).
    pub fn initial_link(&self) -> (usize, Trb) {
        let mut link = Trb::link(self.phys, true);
        // The link starts with the producer cycle: the first pass only
        // finds it "owned" once the producer reaches and rewrites it.
        link.control |= TRB_CYCLE;
        link.control &= !TRB_CYCLE;
        (self.size - 1, link)
    }

    /// Back to an empty ring (the caller zeroes the memory and rewrites
    /// `initial_link`): what a controller sees after Configure Endpoint
    /// points it at `phys` with DCS = 1.
    pub fn reset(&mut self) {
        self.enqueue = 0;
        self.cycle = true;
    }

    /// Enqueues `trb` (without cycle bit) and advances.
    pub fn push(&mut self, mut trb: Trb) -> Enqueued {
        trb.control = (trb.control & !TRB_CYCLE) | if self.cycle { TRB_CYCLE } else { 0 };
        let index = self.enqueue;
        let phys = self.phys + (index * Trb::LEN) as u64;
        self.enqueue += 1;
        let mut link = None;
        if self.enqueue == self.size - 1 {
            // Link: it gets the current cycle so that the consumer follows it and
            // the producer switches to the opposite cycle on wrap-around.
            let mut l = Trb::link(self.phys, true);
            if self.cycle {
                l.control |= TRB_CYCLE;
            }
            // A TD that continues past the wrap carries its Chain bit on
            // the Link TRB too (xHCI 4.11.5.1), or the controller would
            // treat the link as the end of the TD.
            l.control |= trb.control & TRB_CHAIN;
            link = Some((self.size - 1, l));
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        Enqueued {
            index,
            trb,
            phys,
            link,
        }
    }
}

/// Event ring consumer (one segment): knows which TRB comes
/// next and with which cycle bit it must arrive to be valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventRing {
    pub phys: u64,
    pub size: usize,
    pub dequeue: usize,
    pub cycle: bool,
}

impl EventRing {
    pub fn new(phys: u64, size: usize) -> Self {
        Self {
            phys,
            size,
            dequeue: 0,
            cycle: true,
        }
    }

    /// If `trb` (read at `dequeue`) is a new event, advances and returns it.
    pub fn accept(&mut self, trb: Trb) -> Option<Trb> {
        if trb.cycle() != self.cycle {
            return None;
        }
        self.dequeue += 1;
        if self.dequeue == self.size {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(trb)
    }

    /// Value to write to ERDP after processing: current pointer with EHB.
    pub fn dequeue_pointer(&self) -> u64 {
        (self.phys + (self.dequeue * Trb::LEN) as u64) | rt::ERDP_EHB
    }
}

/// Event Ring Segment Table entry (16 bytes).
pub fn erst_entry(segment_phys: u64, trbs: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&segment_phys.to_le_bytes());
    b[8..10].copy_from_slice(&trbs.to_le_bytes());
    b
}

/// Endpoint types of the context (bits 5:3 of word 1).
pub mod ep_type {
    pub const BULK_OUT: u32 = 2;
    pub const INTERRUPT_OUT: u32 = 3;
    pub const CONTROL: u32 = 4;
    pub const BULK_IN: u32 = 6;
    pub const INTERRUPT_IN: u32 = 7;
}

/// A bulk transfer TD as normal TRBs: none crosses a 64 KiB
/// boundary (xHCI 4.11.7), non-final ones chain with the TD size
/// (pending packets) and only the last one notifies. Returns how many TRBs there are;
/// `None` if they do not fit in `out`.
pub fn bulk_trbs(buffer_phys: u64, len: u32, max_packet: u16, out: &mut [Trb]) -> Option<usize> {
    const BOUNDARY: u64 = 64 * 1024;
    let max_packet = max_packet.max(1) as u64;
    let mut parts: [(u64, u32); 8] = [(0, 0); 8];
    let mut n = 0;
    let (mut phys, mut left) = (buffer_phys, len as u64);
    while left > 0 {
        let to_boundary = BOUNDARY - (phys % BOUNDARY);
        let chunk = left.min(to_boundary);
        if n == parts.len() {
            return None;
        }
        parts[n] = (phys, chunk as u32);
        n += 1;
        phys += chunk;
        left -= chunk;
    }
    if n == 0 || n > out.len() {
        return None;
    }
    let mut sent = 0u64;
    for (i, &(p, l)) in parts[..n].iter().enumerate() {
        sent += l as u64;
        let remaining_packets = (len as u64 - sent).div_ceil(max_packet) as u32;
        out[i] = Trb::normal_part(p, l, remaining_packets, i + 1 < n);
    }
    Some(n)
}

/// Device context index (DCI) of a USB endpoint.
pub fn dci(endpoint_number: u8, device_to_host: bool) -> u8 {
    if endpoint_number == 0 {
        1
    } else {
        endpoint_number * 2 + device_to_host as u8
    }
}

/// Slot context (the first 8 words; the rest is reserved).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotContext {
    pub route_string: u32,
    pub speed: u8,
    /// Last valid DCI of the device.
    pub context_entries: u8,
    pub root_hub_port: u8,
}

impl SlotContext {
    pub fn to_words(&self) -> [u32; 8] {
        let mut w = [0u32; 8];
        w[0] = (self.route_string & 0xF_FFFF)
            | (self.speed as u32) << 20
            | (self.context_entries as u32) << 27;
        w[1] = (self.root_hub_port as u32) << 16;
        w
    }
}

/// Endpoint context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointContext {
    pub ep_type: u32,
    pub max_packet_size: u16,
    /// Polling interval: `2^interval * 125 us`.
    pub interval: u8,
    pub error_count: u8,
    /// Transfer ring with its Dequeue Cycle State.
    pub ring_phys: u64,
    pub ring_cycle: bool,
    pub average_trb_len: u16,
    pub max_esit_payload: u16,
}

impl EndpointContext {
    pub fn control(max_packet_size: u16, ring_phys: u64) -> Self {
        Self {
            ep_type: ep_type::CONTROL,
            max_packet_size,
            interval: 0,
            error_count: 3,
            ring_phys,
            ring_cycle: true,
            average_trb_len: 8,
            max_esit_payload: 0,
        }
    }

    /// Bulk endpoint, IN or OUT.
    pub fn bulk(max_packet_size: u16, ring_phys: u64, device_to_host: bool) -> Self {
        Self {
            ep_type: if device_to_host {
                ep_type::BULK_IN
            } else {
                ep_type::BULK_OUT
            },
            max_packet_size,
            interval: 0,
            error_count: 3,
            ring_phys,
            ring_cycle: true,
            average_trb_len: max_packet_size,
            max_esit_payload: 0,
        }
    }

    pub fn interrupt_in(max_packet_size: u16, interval: u8, ring_phys: u64) -> Self {
        Self {
            ep_type: ep_type::INTERRUPT_IN,
            max_packet_size,
            interval,
            error_count: 3,
            ring_phys,
            ring_cycle: true,
            average_trb_len: max_packet_size,
            max_esit_payload: max_packet_size,
        }
    }

    pub fn to_words(&self) -> [u32; 8] {
        let mut w = [0u32; 8];
        w[0] = (self.interval as u32) << 16;
        w[1] = (self.error_count as u32 & 3) << 1
            | (self.ep_type & 7) << 3
            | (self.max_packet_size as u32) << 16;
        let dq = (self.ring_phys & !0xF) | self.ring_cycle as u64;
        w[2] = dq as u32;
        w[3] = (dq >> 32) as u32;
        w[4] = self.average_trb_len as u32 | (self.max_esit_payload as u32) << 16;
        w
    }
}

/// Interval of an interrupt endpoint derived from `bInterval`.
/// High speed and above: `2^(bInterval-1)` microframes. Low and
/// full speed: `bInterval` is in ms and the field is `floor(log2(ms * 8))`,
/// clamped to 3..=10 (xHCI 6.2.3.6, table 6-12: rounded down, to the
/// equal or shorter period, never longer).
pub fn interrupt_interval(speed: u8, b_interval: u8) -> u8 {
    match speed {
        speed::HIGH | speed::SUPER => b_interval.clamp(1, 16) - 1,
        _ => {
            let microframes = (b_interval.max(1) as u32) * 8;
            let floor_log2 = 31 - microframes.leading_zeros() as u8;
            floor_log2.clamp(3, 10)
        }
    }
}

/// Writes an Input Context: control (drop and add flags, xHCI 6.2.2.2),
/// slot and the endpoints given by DCI, with contexts of `context_bytes`
/// each. Returns the bytes used.
pub fn write_input_context(
    out: &mut [u8],
    context_bytes: usize,
    drop_flags: u32,
    add_flags: u32,
    slot: &SlotContext,
    endpoints: &[(u8, EndpointContext)],
) -> usize {
    out.fill(0);
    let put = |out: &mut [u8], index: usize, words: &[u32; 8]| {
        let base = index * context_bytes;
        for (i, w) in words.iter().enumerate() {
            out[base + 4 * i..base + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
    };
    // Input Control Context: word 0 drop flags, word 1 add flags.
    let mut icc = [0u32; 8];
    icc[0] = drop_flags;
    icc[1] = add_flags;
    put(out, 0, &icc);
    put(out, 1, &slot.to_words());
    let mut max = 1;
    for (dci, ep) in endpoints {
        put(out, 1 + *dci as usize, &ep.to_words());
        max = max.max(1 + *dci as usize);
    }
    (max + 1) * context_bytes
}

/// SETUP packet of a control request (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

pub mod request {
    pub const GET_STATUS: u8 = 0;
    pub const CLEAR_FEATURE: u8 = 1;
    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    /// HID class: boot protocol (0) or report protocol (1).
    pub const HID_SET_PROTOCOL: u8 = 0x0B;
    pub const HID_SET_IDLE: u8 = 0x0A;
    pub const DESC_DEVICE: u8 = 1;
    pub const DESC_CONFIGURATION: u8 = 2;
    pub const DESC_STRING: u8 = 3;
}

impl SetupPacket {
    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }
    pub fn get_descriptor(kind: u8, index: u8, length: u16) -> Self {
        Self {
            request_type: 0x80,
            request: request::GET_DESCRIPTOR,
            value: (kind as u16) << 8 | index as u16,
            index: 0,
            length,
        }
    }
    /// `GET_STATUS` of the device (USB 2.0 9.4.5, 2 bytes): the smallest
    /// request that tells whether the device answers on EP0 again.
    pub fn get_device_status() -> Self {
        Self {
            request_type: 0x80,
            request: request::GET_STATUS,
            value: 0,
            index: 0,
            length: 2,
        }
    }
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` on endpoint `address` (bit 7 =
    /// IN), after a STALL in a bulk transfer.
    pub fn clear_endpoint_halt(address: u8) -> Self {
        Self {
            request_type: 0x02,
            request: request::CLEAR_FEATURE,
            value: 0,
            index: address as u16,
            length: 0,
        }
    }
    /// Bulk-Only Mass Storage Reset (BOT 3.1) on `interface`: the device
    /// drops the current command and the host then clears both bulk halts
    /// (Reset Recovery, BOT 5.3.4).
    pub fn mass_storage_reset(interface: u8) -> Self {
        Self {
            request_type: 0x21,
            request: 0xFF,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }
    pub fn set_configuration(value: u8) -> Self {
        Self {
            request_type: 0x00,
            request: request::SET_CONFIGURATION,
            value: value as u16,
            index: 0,
            length: 0,
        }
    }
    /// HID `SET_PROTOCOL`: 0 = boot protocol (fixed reports).
    pub fn hid_set_protocol(interface: u8, protocol: u16) -> Self {
        Self {
            request_type: 0x21,
            request: request::HID_SET_PROTOCOL,
            value: protocol,
            index: interface as u16,
            length: 0,
        }
    }
    /// HID `SET_IDLE` with duration 0: report only when something changes.
    pub fn hid_set_idle(interface: u8) -> Self {
        Self {
            request_type: 0x21,
            request: request::HID_SET_IDLE,
            value: 0,
            index: interface as u16,
            length: 0,
        }
    }
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0] = self.request_type;
        b[1] = self.request;
        b[2..4].copy_from_slice(&self.value.to_le_bytes());
        b[4..6].copy_from_slice(&self.index.to_le_bytes());
        b[6..8].copy_from_slice(&self.length.to_le_bytes());
        b
    }
}

/// Device descriptor (what the driver uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub class: u8,
    pub max_packet_size0: u8,
    pub vendor: u16,
    pub product: u16,
    pub configurations: u8,
}

impl DeviceDescriptor {
    pub const LEN: u16 = 18;
    pub fn parse(d: &[u8]) -> Option<Self> {
        if d.len() < 18 || d[1] != request::DESC_DEVICE {
            return None;
        }
        Some(Self {
            usb_version: u16::from_le_bytes([d[2], d[3]]),
            class: d[4],
            max_packet_size0: d[7],
            vendor: u16::from_le_bytes([d[8], d[9]]),
            product: u16::from_le_bytes([d[10], d[11]]),
            configurations: d[17],
        })
    }
}

/// Boot HID interface found in a configuration descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidInterface {
    pub interface: u8,
    /// 1 keyboard, 2 mouse (boot protocol).
    pub protocol: u8,
    pub endpoint: u8,
    pub max_packet_size: u16,
    pub b_interval: u8,
}

/// Mass storage interface (class 8, subclass 6 transparent SCSI,
/// protocol 0x50 bulk-only) with its bulk endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageInterface {
    pub interface: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
    pub max_packet_in: u16,
    pub max_packet_out: u16,
}

/// First bulk-only storage interface of a complete configuration
/// descriptor, with the configuration value.
pub fn parse_storage_interface(d: &[u8]) -> Option<(u8, StorageInterface)> {
    if d.len() < 9 || d[1] != request::DESC_CONFIGURATION {
        return None;
    }
    let total = (u16::from_le_bytes([d[2], d[3]]) as usize).min(d.len());
    let config_value = d[5];
    let mut off = 0;
    let mut current: Option<StorageInterface> = None;
    while off + 2 <= total {
        let len = d[off] as usize;
        if len < 2 || off + len > total {
            break;
        }
        let e = &d[off..off + len];
        match e[1] {
            4 if len >= 9 => {
                if let Some(i) = current.filter(|i| i.bulk_in != 0 && i.bulk_out != 0) {
                    return Some((config_value, i));
                }
                current = (e[5] == 8 && e[6] == 6 && e[7] == 0x50).then_some(StorageInterface {
                    interface: e[2],
                    bulk_in: 0,
                    bulk_out: 0,
                    max_packet_in: 0,
                    max_packet_out: 0,
                });
            }
            5 if len >= 7 => {
                if let Some(i) = current.as_mut() {
                    let addr = e[2];
                    let max_packet = u16::from_le_bytes([e[4], e[5]]) & 0x7FF;
                    if e[3] & 0x3 == 2 {
                        if addr & 0x80 != 0 && i.bulk_in == 0 {
                            i.bulk_in = addr & 0xF;
                            i.max_packet_in = max_packet;
                        } else if addr & 0x80 == 0 && i.bulk_out == 0 {
                            i.bulk_out = addr & 0xF;
                            i.max_packet_out = max_packet;
                        }
                    }
                }
            }
            _ => {}
        }
        off += len;
    }
    current
        .filter(|i| i.bulk_in != 0 && i.bulk_out != 0)
        .map(|i| (config_value, i))
}

/// Walks a complete configuration descriptor and returns its configuration
/// value and the boot HID interfaces (class 3, subclass 1) with
/// their IN interrupt endpoint.
pub fn parse_configuration(d: &[u8], out: &mut [Option<HidInterface>; 4]) -> Option<(u8, usize)> {
    if d.len() < 9 || d[1] != request::DESC_CONFIGURATION {
        return None;
    }
    let total = (u16::from_le_bytes([d[2], d[3]]) as usize).min(d.len());
    let config_value = d[5];
    let mut off = 0;
    let mut current: Option<HidInterface> = None;
    let mut n = 0;
    let flush =
        |cur: &mut Option<HidInterface>, out: &mut [Option<HidInterface>; 4], n: &mut usize| {
            if let Some(i) = cur.take() {
                if i.endpoint != 0 && *n < out.len() {
                    out[*n] = Some(i);
                    *n += 1;
                }
            }
        };
    while off + 2 <= total {
        let len = d[off] as usize;
        if len < 2 || off + len > total {
            break;
        }
        let e = &d[off..off + len];
        match e[1] {
            4 if len >= 9 => {
                flush(&mut current, out, &mut n);
                if e[5] == 3 && e[6] == 1 {
                    current = Some(HidInterface {
                        interface: e[2],
                        protocol: e[7],
                        endpoint: 0,
                        max_packet_size: 0,
                        b_interval: 0,
                    });
                }
            }
            5 if len >= 7 => {
                if let Some(i) = current.as_mut() {
                    let addr = e[2];
                    let attrs = e[3];
                    if addr & 0x80 != 0 && attrs & 0x3 == 3 && i.endpoint == 0 {
                        i.endpoint = addr & 0xF;
                        i.max_packet_size = u16::from_le_bytes([e[4], e[5]]) & 0x7FF;
                        i.b_interval = e[6];
                    }
                }
            }
            _ => {}
        }
        off += len;
    }
    flush(&mut current, out, &mut n);
    Some((config_value, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_and_speeds() {
        // MaxSlots 32, MaxIntrs 8, MaxPorts 4; scratchpads hi=0 lo=2; CSZ=0, AC64=1.
        let p = Params::decode(32 | 8 << 8 | 4 << 24, 2 << 27, 1);
        assert_eq!(p.max_slots, 32);
        assert_eq!(p.max_interrupters, 8);
        assert_eq!(p.max_ports, 4);
        assert_eq!(p.scratchpad_buffers, 2);
        assert!(!p.context_size_64);
        assert_eq!(p.context_bytes(), 32);
        assert!(p.addressing_64);
        assert_eq!(
            Params::decode(0, 1 << 21 | 3 << 27, 4).scratchpad_buffers,
            35
        );
        assert_eq!(Params::decode(0, 0, 4).context_bytes(), 64);
        // Without scratchpads the base is enough; with 35, 35 more pages plus a spare one.
        assert_eq!(Params::decode(0, 0, 4).dma_arena_bytes(), 64 * 1024 + 4096);
        assert_eq!(
            Params::decode(0, 1 << 21 | 3 << 27, 4).dma_arena_bytes(),
            64 * 1024 + 36 * 4096
        );
        assert_eq!(speed::default_ep0_max_packet(speed::FULL), 8);
        assert_eq!(speed::default_ep0_max_packet(speed::HIGH), 64);
        assert_eq!(speed::default_ep0_max_packet(speed::SUPER), 512);
        assert_eq!(speed::name(speed::HIGH), "high");
        assert_eq!(speed::name(speed::SUPER), "super");
        assert_eq!(speed::name(9), "unknown");
        assert_eq!(op::port_speed(3 << 10 | 1), 3);
        assert_eq!(interrupt_interval(speed::HIGH, 4), 3);
        assert_eq!(interrupt_interval(speed::HIGH, 0), 0);
        assert_eq!(interrupt_interval(speed::HIGH, 255), 15);
        assert_eq!(
            interrupt_interval(speed::FULL, 8),
            6,
            "8 ms = 64 microframes"
        );
        assert_eq!(
            interrupt_interval(speed::FULL, 10),
            6,
            "10 ms is rounded down to 8 ms"
        );
        assert_eq!(interrupt_interval(speed::FULL, 3), 4, "3 ms -> 2 ms");
        assert_eq!(interrupt_interval(speed::LOW, 1), 3);
        assert_eq!(interrupt_interval(speed::LOW, 0), 3);
        assert_eq!(interrupt_interval(speed::FULL, 129), 10, "field maximum");
        assert_eq!(interrupt_interval(speed::FULL, 255), 10);
        // Audit 4836023, finding 2: field in range and period never
        // longer than requested (except the 1 ms minimum).
        for b in 1..=255u8 {
            let i = interrupt_interval(speed::FULL, b);
            assert!((3..=10).contains(&i));
            assert!(
                (1u32 << i) <= (b as u32) * 8,
                "never a longer period: {b} -> {i}"
            );
        }
    }

    #[test]
    fn trb_encoding_and_decoding() {
        let t = Trb::address_device(0x1234_5000, 7);
        assert_eq!(t.trb_type(), trb_type::ADDRESS_DEVICE);
        assert_eq!(t.slot_id(), 7);
        assert_eq!(t.parameter, 0x1234_5000);
        let bytes = t.to_bytes();
        assert_eq!(Trb::from_bytes(&bytes), t);
        let setup = SetupPacket::get_descriptor(request::DESC_DEVICE, 0, 18);
        let s = Trb::setup_stage(&setup);
        assert_eq!(s.trb_type(), trb_type::SETUP_STAGE);
        assert_eq!(s.status, 8);
        assert_eq!((s.control >> 16) & 3, 3, "TRT = IN data");
        assert!(s.control & TRB_IDT != 0);
        assert_eq!(s.parameter.to_le_bytes(), setup.to_bytes());
        let no_data = Trb::setup_stage(&SetupPacket::set_configuration(1));
        assert_eq!((no_data.control >> 16) & 3, 0);
        let d = Trb::data_stage(0x9000, 18, true);
        assert!(d.control & TRB_DIR_IN != 0);
        assert_eq!(d.status, 18);
        let st = Trb::status_stage(false);
        assert!(st.control & TRB_IOC != 0 && st.control & TRB_DIR_IN == 0);
        let n = Trb::normal(0xA000, 8);
        assert!(n.control & (TRB_IOC | TRB_ISP) == (TRB_IOC | TRB_ISP));
        // Command completion event: code 1, slot 3.
        let ev = Trb {
            parameter: 0x100,
            status: 1 << 24,
            control: trb_type::COMMAND_COMPLETION << 10 | 3 << 24 | TRB_CYCLE,
        };
        assert_eq!(ev.completion_code(), completion::SUCCESS);
        assert_eq!(ev.slot_id(), 3);
        let pev = Trb {
            parameter: 2u64 << 24,
            status: 0,
            control: trb_type::PORT_STATUS_CHANGE << 10,
        };
        assert_eq!(pev.port_id(), 2);
        let tev = Trb {
            parameter: 0,
            status: 13 << 24 | 5,
            control: trb_type::TRANSFER_EVENT << 10 | 3 << 16,
        };
        assert_eq!(tev.endpoint_id(), 3);
        assert_eq!(tev.residual(), 5);
        assert_eq!(tev.completion_code(), completion::SHORT_PACKET);
    }

    #[test]
    fn producer_ring_cycles_and_links() {
        let mut r = ProducerRing::new(0x1000, 4);
        let (li, link) = r.initial_link();
        assert_eq!(li, 3);
        assert_eq!(link.trb_type(), trb_type::LINK);
        assert!(!link.cycle());
        assert!(link.control & TRB_TOGGLE_CYCLE != 0);
        let a = r.push(Trb::noop_command());
        assert_eq!((a.index, a.phys), (0, 0x1000));
        assert!(a.trb.cycle());
        assert!(a.link.is_none());
        let b = r.push(Trb::noop_command());
        assert_eq!(b.index, 1);
        let c = r.push(Trb::noop_command());
        assert_eq!(c.index, 2);
        // Filling the last usable one rewrites the link with the current cycle (1)
        // and the producer switches to cycle 0.
        let (li, l) = c.link.expect("link");
        assert_eq!(li, 3);
        assert!(l.cycle());
        assert_eq!(r.enqueue, 0);
        assert!(!r.cycle);
        let d = r.push(Trb::noop_command());
        assert_eq!(d.index, 0);
        assert!(!d.trb.cycle(), "second pass with cycle 0");
        assert_eq!(
            l.control & TRB_CHAIN,
            0,
            "a TD that ended keeps the link unchained"
        );
        // A multi-TRB TD that spans the wrap: the Link TRB carries the
        // Chain bit (xHCI 4.11.5.1) so the controller continues the TD.
        r.push(Trb::noop_command());
        let e = r.push(Trb::normal_part(0x5000, 512, 1, true));
        let (_, l) = e.link.expect("link");
        assert_ne!(
            l.control & TRB_CHAIN,
            0,
            "chained TRB before the wrap chains the link"
        );
        assert!(!l.cycle(), "link carries the producer cycle of that pass");
        r.reset();
        assert_eq!((r.enqueue, r.cycle), (0, true));
        let (li, link) = r.initial_link();
        assert_eq!(li, 3);
        assert!(!link.cycle());
    }

    #[test]
    fn endpoint_status_and_recovery_decisions() {
        // Output Endpoint Context: Halted (2), dequeue 0x1_2345_6780 with DCS = 1.
        let mut ctx = [0u8; 32];
        ctx[0] = 2;
        ctx[8..12].copy_from_slice(&0x2345_6781u32.to_le_bytes());
        ctx[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            endpoint_status(&ctx),
            Some((ep_state::HALTED, 0x1_2345_6780))
        );
        assert_eq!(endpoint_status(&ctx[..15]), None);
        assert_eq!(ep_state::name(ep_state::RUNNING), "Running");
        // Halted: Reset Endpoint, Set TR Dequeue and CLEAR_FEATURE(ENDPOINT_HALT)
        // for every halting code (the toggle must be reset on both sides).
        for code in [
            completion::STALL,
            completion::USB_TRANSACTION_ERROR,
            completion::BABBLE_DETECTED,
            completion::DATA_BUFFER_ERROR,
            completion::SPLIT_TRANSACTION_ERROR,
            completion::NO_EVENT,
        ] {
            assert_eq!(
                endpoint_recovery(code, ep_state::HALTED),
                EndpointRecovery::ResetHalted,
                "code {code}"
            );
        }
        // A timeout on a Running endpoint retires the TD with Stop Endpoint;
        // an error that did not halt it leaves nothing to retire.
        assert_eq!(
            endpoint_recovery(completion::NO_EVENT, ep_state::RUNNING),
            EndpointRecovery::StopAndRetire
        );
        assert_eq!(
            endpoint_recovery(completion::USB_TRANSACTION_ERROR, ep_state::RUNNING),
            EndpointRecovery::Nothing
        );
        assert_eq!(
            endpoint_recovery(completion::STOPPED, ep_state::STOPPED),
            EndpointRecovery::SetDequeue
        );
        assert_eq!(
            endpoint_recovery(completion::NO_EVENT, ep_state::DISABLED),
            EndpointRecovery::Reenumerate
        );
        assert_eq!(
            endpoint_recovery(completion::STALL, ep_state::ERROR),
            EndpointRecovery::Reenumerate
        );
        // Soft Retry: transaction error, Halted, xHCI 1.1 or later.
        assert!(soft_retry_applies(
            0x0110,
            completion::USB_TRANSACTION_ERROR,
            ep_state::HALTED
        ));
        assert!(!soft_retry_applies(
            0x0100,
            completion::USB_TRANSACTION_ERROR,
            ep_state::HALTED
        ));
        assert!(!soft_retry_applies(
            0x0110,
            completion::STALL,
            ep_state::HALTED
        ));
        assert!(!soft_retry_applies(
            0x0110,
            completion::USB_TRANSACTION_ERROR,
            ep_state::RUNNING
        ));
        assert!(completion::is_stopped(completion::STOPPED_LENGTH_INVALID));
        assert!(!completion::is_stopped(completion::SUCCESS));
        assert_eq!(completion::name(4), "USB Transaction Error");
        assert_eq!(completion::name(19), "Context State Error");
        assert_eq!(completion::name(200), "");
    }

    #[test]
    fn budget_arithmetic() {
        // 8 s budget from t = 1000 with a 3 s recovery reserve: transfers
        // end at 6000 at the latest, recovery commands at 9000.
        let b = Budget::new(1000, 8000, 3000);
        assert_eq!(b.transfer_window(), 6000);
        assert_eq!(b.transfer(1000, 5000), Some(6000), "capped by the window");
        assert_eq!(b.transfer(1000, 1000), Some(2000), "its own timeout first");
        assert_eq!(b.transfer(5990, 1000), Some(6000));
        assert_eq!(b.transfer(5996, 1000), None, "less than MIN_START_MS left");
        assert_eq!(
            b.transfer(7000, 1000),
            None,
            "window over: recovery time only"
        );
        assert_eq!(b.recovery(7000, 500), Some(7500));
        assert_eq!(b.recovery(8800, 500), Some(9000), "capped by the deadline");
        assert_eq!(b.recovery(8996, 500), None);
        assert!(b.fits_transfer(1000, 5000));
        assert!(!b.fits_transfer(1001, 5000));
        assert_eq!(step_deadline(10, 100, 50), Some(60));
        assert_eq!(step_deadline(96, 100, 50), None);
        assert_eq!(step_deadline(200, 100, 50), None, "already expired");
        let short = Budget::new(0, 1000, 3000);
        assert_eq!(
            short.transfer_window(),
            0,
            "reserve larger than the budget: no transfers"
        );
        assert_eq!(short.transfer(0, 100), None);
        assert_eq!(short.recovery(0, 100), Some(100));
        // A command inside a step: the full budget when the step is long,
        // a reduced reserve when it is short, refused when the minimum
        // window (CBW + a DATA slice + CSW) cannot fit.
        let long = Budget::within(1000, 100_000, 12_000, 3000, 5000).unwrap();
        assert_eq!((long.deadline, long.reserve_ms), (13_000, 3000));
        let tight = Budget::within(1000, 6500, 12_000, 3000, 5000).unwrap();
        assert_eq!((tight.deadline, tight.reserve_ms), (6500, 500));
        assert_eq!(tight.transfer_window(), 6000, "the minimum window survives");
        let exact = Budget::within(1000, 6000, 12_000, 3000, 5000).unwrap();
        assert_eq!(
            (exact.deadline, exact.reserve_ms, exact.transfer_window()),
            (6000, 0, 6000)
        );
        assert_eq!(
            Budget::within(1000, 5999, 12_000, 3000, 5000),
            None,
            "no room: refused up front"
        );
        assert_eq!(
            Budget::within(1000, 500, 12_000, 3000, 5000),
            None,
            "already expired"
        );
    }

    #[test]
    fn portsc_decoding() {
        let mut buf = [0u8; 96];
        // Connected, enabled, powered, High Speed, U0, no change bits.
        let v = op::PORTSC_CCS | op::PORTSC_PED | op::PORTSC_POWER | 3 << 10;
        assert_eq!(
            op::describe_portsc(v, &mut buf),
            "CCS=1 PED=1 OCA=0 PR=0 PLS=0 PP=1 speed=3 CSC=0 PEC=0 WRC=0 OCC=0 PRC=0 PLC=0 CEC=0"
        );
        // Disconnected with CSC, link in RxDetect (5).
        let v = op::PORTSC_POWER | op::PORTSC_CSC | 5 << 5;
        assert_eq!(
            op::describe_portsc(v, &mut buf),
            "CCS=0 PED=0 OCA=0 PR=0 PLS=5 PP=1 speed=0 CSC=1 PEC=0 WRC=0 OCC=0 PRC=0 PLC=0 CEC=0"
        );
        assert_eq!(op::port_link_state(15 << 5), 15);
        assert_ne!(op::PORTSC_DEVICE_LOST & op::PORTSC_PRC, 0);
        assert_eq!(op::PORTSC_DEVICE_LOST & op::PORTSC_PEC, 0);
        // A short buffer truncates instead of panicking.
        assert_eq!(op::describe_portsc(v, &mut buf[..5]), "CCS=0");
    }

    #[test]
    fn event_ring_accepts_only_current_cycle() {
        let mut e = EventRing::new(0x2000, 2);
        let stale = Trb {
            control: 0,
            ..Default::default()
        };
        assert_eq!(e.accept(stale), None);
        let fresh = Trb {
            control: TRB_CYCLE | trb_type::PORT_STATUS_CHANGE << 10,
            ..Default::default()
        };
        assert!(e.accept(fresh).is_some());
        assert_eq!(e.dequeue_pointer(), 0x2010 | rt::ERDP_EHB);
        assert!(e.accept(fresh).is_some());
        assert_eq!(e.dequeue, 0);
        assert!(!e.cycle, "wrap-around: new events now carry cycle 0");
        assert_eq!(e.accept(fresh), None);
        assert!(e.accept(stale).is_some());
        let entry = erst_entry(0x2000, 2);
        assert_eq!(&entry[..8], &0x2000u64.to_le_bytes());
        assert_eq!(&entry[8..10], &2u16.to_le_bytes());
    }

    #[test]
    fn contexts_and_input_context_layout() {
        assert_eq!(dci(0, true), 1);
        assert_eq!(dci(1, true), 3);
        assert_eq!(dci(2, false), 4);
        let slot = SlotContext {
            route_string: 0,
            speed: speed::HIGH,
            context_entries: 3,
            root_hub_port: 2,
        };
        let w = slot.to_words();
        assert_eq!(w[0], 3 << 20 | 3 << 27);
        assert_eq!(w[1], 2 << 16);
        let ep0 = EndpointContext::control(64, 0x3000);
        let w = ep0.to_words();
        assert_eq!(w[1], 3 << 1 | ep_type::CONTROL << 3 | 64 << 16);
        assert_eq!(w[2], 0x3001, "dequeue with DCS = 1");
        assert_eq!(w[4], 8);
        let int = EndpointContext::interrupt_in(8, 6, 0x4000);
        let w = int.to_words();
        assert_eq!(w[0], 6 << 16);
        assert_eq!(w[1] >> 3 & 7, ep_type::INTERRUPT_IN);
        assert_eq!(w[4], 8 | 8 << 16);
        let mut buf = [0xFFu8; 33 * 64];
        let used = write_input_context(&mut buf, 32, 0, 0b1011, &slot, &[(1, ep0), (3, int)]);
        assert_eq!(used, 5 * 32);
        assert_eq!(
            u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            0b1011,
            "add flags"
        );
        assert_eq!(
            u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            slot.to_words()[0]
        );
        assert_eq!(
            u32::from_le_bytes(buf[64 + 4..64 + 8].try_into().unwrap()),
            ep0.to_words()[1]
        );
        assert_eq!(
            u32::from_le_bytes(buf[128..132].try_into().unwrap()),
            int.to_words()[0]
        );
        assert!(
            buf[5 * 32..].iter().all(|&b| b == 0),
            "the rest stays zeroed"
        );
        let used64 = write_input_context(&mut buf, 64, 0, 0b11, &slot, &[(1, ep0)]);
        assert_eq!(used64, 3 * 64);
    }

    #[test]
    fn descriptors() {
        let dev = [
            18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0, 0, 1, 2, 3, 1,
        ];
        let d = DeviceDescriptor::parse(&dev).unwrap();
        assert_eq!(d.usb_version, 0x0200);
        assert_eq!(d.max_packet_size0, 64);
        assert_eq!((d.vendor, d.product), (0x0627, 0x0001));
        assert_eq!(d.configurations, 1);
        assert!(DeviceDescriptor::parse(&dev[..17]).is_none());
        // Configuration with two boot HID interfaces: keyboard (ep 1 IN,
        // 8 bytes, 7 ms) and mouse (ep 2 IN, 4 bytes, 10 ms), plus an OUT
        // endpoint that is ignored.
        let mut cfg = Vec::new();
        cfg.extend_from_slice(&[9, 2, 0, 0, 2, 1, 0, 0xA0, 50]);
        cfg.extend_from_slice(&[9, 4, 0, 0, 1, 3, 1, 1, 0]);
        cfg.extend_from_slice(&[9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0]);
        cfg.extend_from_slice(&[7, 5, 0x81, 3, 8, 0, 7]);
        cfg.extend_from_slice(&[9, 4, 1, 0, 2, 3, 1, 2, 0]);
        cfg.extend_from_slice(&[7, 5, 0x02, 3, 4, 0, 10]);
        cfg.extend_from_slice(&[7, 5, 0x82, 3, 4, 0, 10]);
        let total = cfg.len() as u16;
        cfg[2..4].copy_from_slice(&total.to_le_bytes());
        let mut out = [None; 4];
        let (value, n) = parse_configuration(&cfg, &mut out).unwrap();
        assert_eq!((value, n), (1, 2));
        assert_eq!(
            out[0],
            Some(HidInterface {
                interface: 0,
                protocol: 1,
                endpoint: 1,
                max_packet_size: 8,
                b_interval: 7
            })
        );
        assert_eq!(
            out[1],
            Some(HidInterface {
                interface: 1,
                protocol: 2,
                endpoint: 2,
                max_packet_size: 4,
                b_interval: 10
            })
        );
        assert!(parse_configuration(&cfg[..8], &mut out).is_none());
        let setup = SetupPacket::hid_set_protocol(1, 0).to_bytes();
        assert_eq!(setup, [0x21, 0x0B, 0, 0, 1, 0, 0, 0]);
        // No storage interface.
        assert!(parse_storage_interface(&cfg).is_none());
    }

    #[test]
    fn storage_interface_and_bulk_endpoints() {
        // USB stick: interface 0 of class 8/6/0x50 with bulk IN 0x81 (512) and
        // bulk OUT 0x02 (512), preceded by an interface of another class.
        let mut cfg = Vec::new();
        cfg.extend_from_slice(&[9, 2, 0, 0, 2, 1, 0, 0x80, 50]);
        cfg.extend_from_slice(&[9, 4, 0, 0, 1, 0xFF, 0, 0, 0]);
        cfg.extend_from_slice(&[7, 5, 0x83, 3, 8, 0, 7]);
        cfg.extend_from_slice(&[9, 4, 1, 0, 2, 8, 6, 0x50, 0]);
        cfg.extend_from_slice(&[7, 5, 0x81, 2, 0, 2, 0]);
        cfg.extend_from_slice(&[7, 5, 0x02, 2, 0, 2, 0]);
        let total = cfg.len() as u16;
        cfg[2..4].copy_from_slice(&total.to_le_bytes());
        assert_eq!(
            parse_storage_interface(&cfg),
            Some((
                1,
                StorageInterface {
                    interface: 1,
                    bulk_in: 1,
                    bulk_out: 2,
                    max_packet_in: 512,
                    max_packet_out: 512,
                }
            ))
        );
        let ctx = EndpointContext::bulk(512, 0x1000, true).to_words();
        assert_eq!((ctx[1] >> 3) & 7, ep_type::BULK_IN);
        assert_eq!(
            (EndpointContext::bulk(512, 0x1000, false).to_words()[1] >> 3) & 7,
            ep_type::BULK_OUT
        );
        assert_eq!(
            SetupPacket::clear_endpoint_halt(0x81).to_bytes(),
            [0x02, 1, 0, 0, 0x81, 0, 0, 0]
        );
        let t = Trb::reset_endpoint(3, 2, false);
        assert_eq!((t.trb_type(), t.slot_id(), t.endpoint_id()), (14, 3, 2));
        assert_eq!(t.control & TRB_TSP, 0);
        assert_ne!(Trb::reset_endpoint(3, 2, true).control & TRB_TSP, 0);
        let t = Trb::stop_endpoint(3, 4);
        assert_eq!((t.trb_type(), t.slot_id(), t.endpoint_id()), (15, 3, 4));
        let t = Trb::disable_slot(5);
        assert_eq!((t.trb_type(), t.slot_id()), (10, 5));
        assert_eq!(
            SetupPacket::get_device_status().to_bytes(),
            [0x80, 0, 0, 0, 0, 0, 2, 0]
        );
        let t = Trb::set_tr_dequeue(3, 2, 0x2340, true);
        assert_eq!((t.trb_type(), t.parameter), (16, 0x2341));
    }

    #[test]
    fn bulk_td_never_crosses_64k() {
        let mut out = [Trb::noop_command(); 4];
        // 8 KiB crossing a 64 KiB boundary: two TRBs, the first chained
        // with the packets (512 B) left, the last one with IOC.
        let n = bulk_trbs(0x1_F000, 8192, 512, &mut out).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            (out[0].parameter, out[0].status & 0x1FFFF),
            (0x1_F000, 4096)
        );
        assert_eq!((out[0].status >> 17) & 0x1F, 8);
        assert_ne!(out[0].control & TRB_CHAIN, 0);
        assert_eq!(out[0].control & TRB_IOC, 0);
        assert_eq!(
            (out[1].parameter, out[1].status & 0x1FFFF),
            (0x2_0000, 4096)
        );
        assert_eq!((out[1].status >> 17) & 0x1F, 0);
        assert_eq!(out[1].control & TRB_CHAIN, 0);
        assert_ne!(out[1].control & TRB_IOC, 0);
        // No crossing: a single TRB with IOC.
        assert_eq!(bulk_trbs(0x1000, 31, 512, &mut out), Some(1));
        assert_ne!(out[0].control & TRB_IOC, 0);
        assert_eq!(bulk_trbs(0x1000, 0, 512, &mut out), None);
        assert_eq!(bulk_trbs(0x1_F000, 8192, 512, &mut out[..1]), None);
    }

    extern crate std;
    use std::vec::Vec;
}
