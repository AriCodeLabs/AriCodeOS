// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! usbd (Phase 25, second part; ADR 0023): ring 3 xHCI driver for
//! USB HID keyboards and mice using the boot protocol, and for one
//! mass storage device (bulk-only, SCSI). Receives the driver contract
//! from init (`SERVICE`, cfg, hardware, IRQ, BAR0) and, as extras,
//! `SYSTEM` (`READ`) for the clock and the block service endpoint.
//! Enumerates the ports with a device attached, configures each boot HID
//! interface, polls its interrupt endpoints and delivers the events
//! over IPC (`InputServiceProtocol v1`) as a single input device;
//! storage is served with the block protocol (`aricode-proto::block`).
//!
//! Failures (ADR 0028, incident 0001): every failed transfer is printed
//! with the controller's view of it and recorded as a fault event; the
//! endpoint that reported it is recovered as xHCI 4.10.2.1 / 4.6.8 /
//! 4.6.9 / 4.6.10 require within the request's budget (transfers leave a
//! reserve for that recovery; nothing is ever enqueued without being
//! waited for), and everything slower (the Bulk-Only Reset Recovery, BOT
//! 5.3.4, and re-enumeration after a port change) runs from the idle loop,
//! one bounded step per iteration, while block requests answer `Io`.
//! A failed HID report is recorded and its interrupt endpoint recovered
//! the same way from the idle loop (the report TRBs are queued again); a
//! device re-enumerated while still on its port keeps its slot, contexts
//! and rings (Reset Device, xHCI 4.6.11), so re-enumerations do not
//! consume DMA memory.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;

use aricode_input::{Event, HidBootKeyboard, HidBootMouse, MAX_REPORT_EVENTS};
use aricode_proto::block::{
    Op as BlockOp, Reply as BlockReply, Request as BlockRequest, Status as BlockStatus, MAX_SECTORS,
};
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status};
use aricode_rt::sys::block::{map_client_buffer, ClientBuffers};
use aricode_rt::sys::client_buffer::{DeviceOwned, Kept};
use aricode_rt::sys::input::{EventQueue, QUEUE_LEN};
use aricode_rt::sys::virtio_pci::{bar_handle, extra_handle, DmaRegion, Mmio, CFG, IRQ, SERVICE};
use aricode_rt::sys::wait::{self, Deadline, Woken};
use aricode_rt::sys::{self, abi, Handle};
use aricode_usb_storage::{
    after_transfer_failure, bytes_not_moved, check_csw_transfer, fault, scsi, Cbw, Cdb, Csw,
    CswError, CswStatus, Next, Stage, CSW_TEXT_LEN,
};
use aricode_xhci::{
    bulk_trbs, cap, completion, dci, endpoint_recovery, endpoint_status, ep_state, erst_entry, op,
    parse_storage_interface, rt, slot_reuse, slot_state, slot_status, soft_retry_applies, speed,
    step_deadline, td_bytes_moved, trb_type, write_input_context, Budget, DeviceDescriptor,
    EndpointContext, EndpointRecovery, EventRing, HidInterface, Params, ProducerRing, SetupPacket,
    SlotContext, SlotReuse, StorageInterface, Trb, MIN_START_MS,
};

const CMD_RING_TRBS: usize = 16;
const EVENT_RING_TRBS: usize = 32;
const XFER_RING_TRBS: usize = 16;
const MAX_DEVICES: usize = 4;
/// Pending report TRBs per interrupt endpoint.
const REPORTS_IN_FLIGHT: usize = 4;

/// Timeouts of a SCSI command over Bulk-Only (ms). BOT 1.0 specifies no
/// transport timeout; SCSI stacks bound a whole command instead (Linux
/// `sd`: 30 s, `SD_TIMEOUT`). Incident 0001 measured why: the laptop's
/// stick (Innostor IS918) accepts a WRITE's data and confirms it (CSW)
/// only after up to seconds of flash housekeeping, more than 3 s on the
/// laptop and 1.5 s under Linux on the host, with no error. So the DATA
/// and CSW stages share one 30 s command window; the CBW, which the device
/// takes before starting any work, keeps 1 s.
const CBW_TIMEOUT_MS: u64 = 1000;
const SCSI_COMMAND_TIMEOUT_MS: u64 = 30_000;
/// Each stage may use the whole command window; `Budget::transfer` ends
/// both at the window's end, so together they never exceed it.
const DATA_TIMEOUT_MS: u64 = SCSI_COMMAND_TIMEOUT_MS;
const CSW_TIMEOUT_MS: u64 = SCSI_COMMAND_TIMEOUT_MS;
/// Transfer window of one block request: CBW + the command window.
const TRANSFER_WINDOW_MS: u64 = CBW_TIMEOUT_MS + SCSI_COMMAND_TIMEOUT_MS;
/// Server budget per block request (ms), from the moment it is served:
/// the transfer window plus `RECOVERY_RESERVE_MS` for the recovery
/// commands of a failed transfer (31 + 3 = 34 s). Chain of waits: the
/// block client waits `aricode_sys::block::REPLY_WAIT_MS` (90 s) > this
/// budget (34 s) + the idle-loop maintenance of one iteration, the
/// longest storage step (`REENUMERATE_STEP_MS`, 6 s) plus one HID
/// recovery step (`maintain_hid`, `MAINTENANCE_STEP_MS`, 3 s) + one queued
/// request (34 s) = 77 s; the file client waits 120 s > 90 s. The chain
/// assumes at most two concurrent block clients (one queued request).
const REQUEST_BUDGET_MS: u64 = TRANSFER_WINDOW_MS + RECOVERY_RESERVE_MS;
/// What a failed transfer may need before the request returns: Reset or
/// Stop Endpoint (500) + Set TR Dequeue (500) + CLEAR_FEATURE with its own
/// EP0 recovery (2000) for the failed endpoint. Retiring a CSW re-read or
/// a Disable Slot beyond that is refused or left pending, which the
/// kept-buffer bookkeeping (`Controller::request_buffer`, `Controller::owned`) covers.
const RECOVERY_RESERVE_MS: u64 = 3000;
/// Least transfer window for a SCSI command issued inside a maintenance
/// step (INQUIRY, TEST UNIT READY, READ CAPACITY during a re-enumeration):
/// CBW + a 1 s DATA slice + 3 s for the CSW. These commands touch no flash
/// and answer at once on a working device; a budget that cannot hold this
/// window is refused up front (`command_budget`).
const MIN_COMMAND_WINDOW_MS: u64 = CBW_TIMEOUT_MS + 1000 + 3000;
/// The single retry after a recovery is a full READ/WRITE: it needs the
/// whole transfer window, so it only happens after a fast failure.
const RETRY_MIN_MS: u64 = TRANSFER_WINDOW_MS;
const CONTROL_TIMEOUT_MS: u64 = 1000;
const COMMAND_TIMEOUT_MS: u64 = 500;
/// EP0 recovery after a failed control transfer (a Reset or Stop Endpoint
/// and a Set TR Dequeue), reserved inside the deadline `control()` gets.
const EP0_RECOVERY_MS: u64 = 2 * COMMAND_TIMEOUT_MS;
/// Hard bound of one idle-loop maintenance step: the class reset, or a
/// GET_STATUS poll plus the halt clearing.
const MAINTENANCE_STEP_MS: u64 = 3000;
/// Shortest wait between two idle-loop maintenance steps (ADR 0044). A
/// step that is due may be unable to progress until the controller
/// completes a command (a re-enumeration postponed behind an outstanding
/// Disable Slot, a Disable Slot refused for lack of time): it is retried
/// at this pace, the one the loop had before it waited for deadlines,
/// instead of in a busy loop.
const MAINTENANCE_PACE_MS: u64 = 10;
/// Requests served per round, from both endpoints together, between two
/// looks at the event ring; a budget used up makes the next round follow
/// at once (ADR 0044).
const REQUESTS_PER_ROUND: usize = 8;
/// Exit code when the end-of-round wait fails for a reason other than the
/// service endpoints being gone: looping on it would spin (ADR 0044).
const EXIT_WAIT_FAILED: u64 = 10;
/// Hard bound of a re-enumeration step (the longest): a port reset (500)
/// and the TEST UNIT READY wait (`REENUMERATE_READY_WAIT_MS`, 500) leave
/// `MIN_COMMAND_WINDOW_MS` for the last SCSI command; the step is aborted
/// and counted as a failed attempt when it passes.
const REENUMERATE_STEP_MS: u64 = 6000;
/// Reset Recovery (BOT 5.3.4): GET_STATUS polling period after the class
/// reset and the longest wait for the device to answer (Linux
/// `usb-storage` waits 6 s unconditionally).
const RESET_POLL_MS: u64 = 200;
const RESET_WAIT_MS: u64 = 6000;
/// USB 2.0 7.1.7.3: a (re)connected device gets 100 ms of debounce before
/// its port is reset.
const DEBOUNCE_MS: u64 = 100;
/// Consecutive re-enumerations before the storage is given up until the
/// port sees a new connection.
const REENUMERATE_ATTEMPTS: u8 = 3;
/// How long TEST UNIT READY may say "not ready": at boot (a stick spinning
/// up) and when enumerating again (within `REENUMERATE_STEP_MS`).
const BOOT_READY_WAIT_MS: u64 = 2000;
const REENUMERATE_READY_WAIT_MS: u64 = 500;
/// Bound of the boot-time enumeration of one port.
const BOOT_ENUMERATION_MS: u64 = 10_000;
/// Wait before a HID recovery attempt, times the attempts already made
/// (the backoff of `Transport::Reenumerate`, `RESET_POLL_MS`): a device
/// that keeps failing is given time to settle instead of being given up
/// within milliseconds (incident 0001 audit 5 M3).
const HID_RETRY_MS: u64 = RESET_POLL_MS;
/// Failures of one HID endpoint printed and recorded individually; later
/// ones are counted (`Endpoint::faults`) and the count printed when the
/// endpoint recovers or is given up, so a babbling device cannot flood the
/// console or the event ring (like `Controller::may_report`).
const HID_FAULT_REPORTS: u32 = 8;
/// Recovery attempts of a HID interrupt endpoint without a good report in
/// between; once spent the endpoint is given up (loudly, `usb hid lost`)
/// for the rest of the boot: HID devices are not enumerated again.
const HID_RECOVERY_ATTEMPTS: u8 = 3;
/// Size of the Output Device Context and of the Input Context, in
/// contexts (slot + 31 endpoints; the Input Control Context on top).
const DEVICE_CONTEXTS: usize = 32;
const INPUT_CONTEXTS: usize = 33;

fn say(msg: &[u8]) {
    sys::print(b"usb-input: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(v, &mut b));
}

fn hex(v: u64) {
    let mut b = [0u8; 16];
    let width = ((64 - v.leading_zeros() as usize).div_ceil(4)).max(1);
    sys::print(b"0x");
    sys::print(aricode_rt::fmt_hex(v, width, &mut b));
}

fn hex4(v: u64) {
    let mut b = [0u8; 16];
    sys::print(aricode_rt::fmt_hex(v, 4, &mut b));
}

/// Prints a completion code with its name.
fn code_text(code: u8) {
    sys::print(b"completion code ");
    num(code as u64);
    sys::print(b" (");
    sys::print(completion::name(code).as_bytes());
    sys::print(b")");
}

/// A fault event in the kernel ring (ADR 0028; `diag` lists it).
fn record(name: &str, code: u64) {
    let _ = sys::event_record(code, name);
}

/// The device's reason for a failed command as a fault event
/// (`usb sense K A Q`, the ASC / ASCQ in hex).
fn record_sense(sense: &scsi::Sense) {
    let mut name = [0u8; fault::NAME_LEN];
    if let Some(text) = fault::sense_name(sense, &mut name) {
        record(text, sense.key as u64);
    }
}

/// The fault event of a block request (`usb [inj] r|w lba N`, ADR 0028)
/// with `code`: the completion code of a transport failure, the bytes
/// short of a short data stage.
fn record_lba(write: bool, injected: bool, lba: u64, code: u64) {
    let mut name = [0u8; fault::NAME_LEN];
    if let Some(text) = fault::lba_name(fault::lba_prefix(write, injected), lba, &mut name) {
        record(text, code);
    }
}

/// `K/0xAA/0xQQ`: sense key in decimal, ASC and ASCQ in hex (SPC-4 4.5).
fn print_sense(sense: &scsi::Sense) {
    num(sense.key as u64);
    sys::print(b"/");
    hex(sense.asc as u64);
    sys::print(b"/");
    hex(sense.ascq as u64);
}

struct Clock(Handle);

impl Clock {
    fn now(&self) -> u64 {
        let mut b = [0u8; 8];
        match sys::system_info(self.0, abi::sysinfo::UPTIME, &mut b) {
            Ok(8) => u64::from_le_bytes(b),
            _ => 0,
        }
    }
}

/// Linear allocator inside the driver's DMA buffer (64-byte alignment).
struct Arena {
    region: DmaRegion,
    next: u64,
}

impl Arena {
    fn alloc(&mut self, len: u64, align: u64) -> Option<(u64, u64)> {
        let off = self.next.next_multiple_of(align);
        if off + len > self.region.len {
            return None;
        }
        self.next = off + len;
        self.region.slice_mut(off, len).fill(0);
        Some((self.region.phys + off, self.region.virt + off))
    }
}

fn write_trb(virt: u64, trb: &Trb) {
    // SAFETY: `virt` is a TRB inside the driver's DMA buffer.
    unsafe { (virt as *mut [u8; Trb::LEN]).write_volatile(trb.to_bytes()) };
}

fn read_trb(virt: u64) -> Trb {
    // SAFETY: same as above.
    Trb::from_bytes(&unsafe { (virt as *const [u8; Trb::LEN]).read_volatile() })
}

/// Producer ring with its memory. Copyable so that the memory of a
/// device being enumerated again can be handed to its new incarnation.
#[derive(Clone, Copy)]
struct Ring {
    state: ProducerRing,
    virt: u64,
}

impl Ring {
    fn new(arena: &mut Arena, trbs: usize) -> Option<Self> {
        let (phys, virt) = arena.alloc((trbs * Trb::LEN) as u64, 64)?;
        let state = ProducerRing::new(phys, trbs);
        let (li, link) = state.initial_link();
        write_trb(virt + (li * Trb::LEN) as u64, &link);
        Some(Self { state, virt })
    }

    /// Enqueues a TRB and returns its physical address.
    fn push(&mut self, trb: Trb) -> u64 {
        let e = self.state.push(trb);
        write_trb(self.virt + (e.index * Trb::LEN) as u64, &e.trb);
        if let Some((li, link)) = e.link {
            write_trb(self.virt + (li * Trb::LEN) as u64, &link);
        }
        e.phys
    }

    /// Empties the ring (the endpoint must be Stopped or dropped: the
    /// controller must not be following it).
    fn reset(&mut self) {
        for i in 0..self.state.size {
            write_trb(self.virt + (i * Trb::LEN) as u64, &Trb::default());
        }
        self.state.reset();
        let (li, link) = self.state.initial_link();
        write_trb(self.virt + (li * Trb::LEN) as u64, &link);
    }

    /// Where the producer writes next, with its cycle: the pointer a Set TR
    /// Dequeue gives the controller to skip everything before it.
    fn enqueue_pointer(&self) -> (u64, bool) {
        (
            self.state.phys + (self.state.enqueue * Trb::LEN) as u64,
            self.state.cycle,
        )
    }
}

/// A failed transfer on a HID interrupt endpoint, waiting for its
/// recovery from the idle loop (`maintain_hid`).
#[derive(Clone, Copy)]
struct HidFault {
    /// Completion code of the Transfer Event.
    code: u8,
    /// The report TRB it pointed at.
    trb: u64,
    /// `usb_fault=...:hid`: the report arrived and is treated as failed.
    injected: bool,
    /// Reset Endpoint ran; `CLEAR_FEATURE(ENDPOINT_HALT)` must still reach
    /// the device before reports are queued again (USB 2.0 9.4.5).
    clear_halt: bool,
    /// No recovery attempt before this time (ms): `HID_RETRY_MS` times the
    /// attempts already made.
    not_before: u64,
}

/// Interrupt IN endpoint of a HID interface.
struct Endpoint {
    dci: u8,
    /// USB endpoint address (bit 7 = IN), for `CLEAR_FEATURE`.
    address: u8,
    ring: Ring,
    /// The first report has arrived (diagnostic trace).
    seen: bool,
    /// Reports: (TRB on the ring or 0 when none is, physical buffer,
    /// virtual buffer).
    reports: [(u64, u64, u64); REPORTS_IN_FLIGHT],
    report_len: u16,
    fault: Option<HidFault>,
    /// Recovery attempts since the last good report.
    failures: u8,
    /// Failed transfers in this boot (`HID_FAULT_REPORTS`).
    faults: u32,
    /// Another transfer failed while a fault was already pending: the
    /// recovery attempt in progress (or the next one) must not take the
    /// endpoint as recovered and re-queue reports on it; it is recovered
    /// again from its state instead.
    refault: bool,
    /// Recovery attempts spent (`usb hid lost`): later failures, stray
    /// ones included, are only counted and never re-arm a recovery.
    given_up: bool,
}

impl Endpoint {
    /// A transfer on this endpoint of `slot` failed with `code` (event
    /// `ev`; `injected` by `usb_fault=...:hid`): printed and recorded as a
    /// fault event (ADR 0028) up to `HID_FAULT_REPORTS` times, then only
    /// counted; the recovery is left to the idle loop, after
    /// `HID_RETRY_MS` times the attempts already made.
    fn failed(&mut self, slot: u8, code: u8, ev: Trb, injected: bool, now: u64) {
        self.faults = self.faults.saturating_add(1);
        if self.given_up {
            return;
        }
        if self.faults <= HID_FAULT_REPORTS {
            sys::print(b"usb-input: HID report on slot ");
            num(slot as u64);
            sys::print(b" dci ");
            num(self.dci as u64);
            sys::print(b" failed: ");
            code_text(code);
            if injected {
                sys::print(b", injected");
            }
            sys::print(b", residual ");
            num(ev.residual() as u64);
            sys::print(b", trb ");
            hex(ev.parameter);
            sys::print(b"; recovered from the idle loop\n");
            record(
                if injected {
                    "usb inj hid"
                } else {
                    "usb hid err"
                },
                code as u64,
            );
            if self.faults == HID_FAULT_REPORTS {
                sys::print(b"usb-input: HID slot ");
                num(slot as u64);
                sys::print(b" dci ");
                num(self.dci as u64);
                sys::print(b": further failures are counted, not printed\n");
            }
        }
        if self.fault.is_some() {
            self.refault = true;
            return;
        }
        self.fault = Some(HidFault {
            code,
            trb: ev.parameter,
            injected,
            clear_halt: false,
            not_before: now + HID_RETRY_MS * self.failures as u64,
        });
    }
}

/// DMA memory of a device slot: Output Device Context (physical,
/// virtual), EP0 transfer ring and Input Context (physical, virtual,
/// bytes). Reused when the device is enumerated again on the same slot
/// (Reset Device) or, once a Disable Slot completed, by the next slot
/// (`Controller::spare_slot`).
#[derive(Clone, Copy)]
struct SlotMemory {
    dev_ctx: (u64, u64),
    ep0: Ring,
    input_ctx: (u64, u64, u64),
}

struct UsbDevice {
    slot: u8,
    /// Contexts and EP0 ring. The Output Device Context holds the
    /// controller's slot and endpoint states and dequeue pointers, read
    /// before every recovery step.
    mem: SlotMemory,
    endpoints: [Option<Endpoint>; MAX_DEVICES],
    /// Per endpoint: keyboard or mouse.
    decoders: [Decoder; MAX_DEVICES],
    /// Slot context written at addressing, to configure endpoints later.
    slot_ctx: SlotContext,
}

/// What the storage device said it is at enumeration, and the speed its
/// port negotiated: printed then, and again in every failure dump so that
/// it survives on the console.
#[derive(Clone, Copy)]
struct Identity {
    vendor: u16,
    product: u16,
    usb_version: u16,
    max_power_ma: u16,
    /// PSIV of the port (1 full, 2 low, 3 high, 4 super).
    speed: u8,
}

impl Identity {
    fn print(&self, port: u8) {
        sys::print(b"device ");
        hex4(self.vendor as u64);
        sys::print(b":");
        hex4(self.product as u64);
        sys::print(b" bcdUSB ");
        hex4(self.usb_version as u64);
        sys::print(b" bMaxPower ");
        num(self.max_power_ma as u64);
        sys::print(b" mA port ");
        num(port as u64);
        sys::print(b" speed ");
        num(self.speed as u64);
        sys::print(b" (");
        sys::print(speed::name(self.speed).as_bytes());
        sys::print(b")");
    }
}

/// Bulk endpoint of a storage device.
struct Bulk {
    dci: u8,
    /// USB endpoint address (bit 7 = IN), for `CLEAR_FEATURE`.
    address: u8,
    ring: Ring,
    max_packet: u16,
}

/// State of the storage transport (BOT 5.3). Anything but `Ready` or
/// `Enumerating` answers block requests with `Io` at once; the idle loop
/// advances the recovery one bounded step per iteration (`maintain_storage`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// A CBW may be sent.
    Ready,
    /// INQUIRY / TEST UNIT READY / READ CAPACITY in progress: a failed
    /// transfer only recovers its endpoint and propagates to the
    /// enumeration, which counts attempts.
    Enumerating,
    /// Reset Recovery due (BOT 5.3.4): the class reset goes out when idle.
    ResetPending,
    /// Bulk-Only Mass Storage Reset sent at `since`; GET_STATUS is polled
    /// from `next_poll` until the device answers (`RESET_WAIT_MS` at most),
    /// then both bulk halts are cleared.
    ResetSent {
        since: u64,
        next_poll: u64,
        failed_polls: u8,
    },
    /// The device must be enumerated again (slot, address, configuration,
    /// endpoints) from `not_before`; `reset_port` resets the port first
    /// (the last resort).
    Reenumerate { not_before: u64, reset_port: bool },
    /// No usable device on the port: requests fail at once until a new
    /// connection is seen.
    Gone,
}

impl Transport {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Ready => b"ready",
            Self::Enumerating => b"enumerating",
            Self::ResetPending => b"reset pending",
            Self::ResetSent { .. } => b"reset sent",
            Self::Reenumerate { .. } => b"re-enumerate",
            Self::Gone => b"gone",
        }
    }
}

/// DMA memory of the storage device besides its slot: both bulk rings and
/// the CBW, CSW and small-response buffers. Handed from a storage being
/// enumerated again to its new incarnation (`Controller::spare_storage`).
#[derive(Clone, Copy)]
struct StorageMemory {
    ring_in: Ring,
    ring_out: Ring,
    cbw: (u64, u64),
    csw: (u64, u64),
    small: (u64, u64),
}

impl StorageMemory {
    fn alloc(arena: &mut Arena) -> Option<Self> {
        Some(Self {
            ring_in: Ring::new(arena, XFER_RING_TRBS)?,
            ring_out: Ring::new(arena, XFER_RING_TRBS)?,
            cbw: arena.alloc(64, 64)?,
            csw: arena.alloc(64, 64)?,
            small: arena.alloc(64, 64)?,
        })
    }

    fn of(st: &Storage) -> Self {
        Self {
            ring_in: st.bulk_in.ring,
            ring_out: st.bulk_out.ring,
            cbw: st.cbw,
            csw: st.csw,
            small: st.small,
        }
    }
}

/// Bulk-only mass storage device (at most one).
struct Storage {
    /// Index in `devices`; `None` once its slot was disabled.
    device: Option<usize>,
    slot: u8,
    port: u8,
    /// Interface number (Bulk-Only Mass Storage Reset).
    interface: u8,
    bulk_in: Bulk,
    bulk_out: Bulk,
    tag: u32,
    /// CBW, CSW and small responses (INQUIRY, sense, capacity) in the
    /// driver's DMA buffer: (physical, virtual).
    cbw: (u64, u64),
    csw: (u64, u64),
    small: (u64, u64),
    /// 512-byte sectors (0 until READ CAPACITY succeeded).
    capacity: u64,
    /// The device supports `SYNCHRONIZE CACHE`; if it rejects it as
    /// unsupported, the flush has nothing to write back.
    sync_cache: bool,
    /// I/O errors reported (capped so as not to flood the console).
    errors_reported: u8,
    transport: Transport,
    /// Re-enumerations since the last successful command or connection.
    reenumerations: u8,
    /// A connect status change was seen on the port since the slot was
    /// addressed: the device that slot addressed is gone (xHCI 4.19.2), so
    /// the slot is disabled rather than reset.
    reconnected: bool,
    identity: Identity,
}

/// Why a SCSI command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScsiFail {
    /// A bulk transfer failed, with the xHCI completion code
    /// (`completion::NO_EVENT` for a timeout or a refused transfer) and,
    /// when the CSW read after the failure said Command Failed and the
    /// device answered REQUEST SENSE, its own reason.
    Transport(u8, Option<scsi::Sense>),
    /// The CSW was invalid, for another command or a phase error.
    Csw,
    /// The transport is not Ready: no CBW was sent (BOT 5.3).
    NotReady,
    /// The device rejected the command (`REQUEST SENSE` says why).
    Failed,
}

impl ScsiFail {
    fn code(self) -> u8 {
        match self {
            Self::Transport(code, _) => code,
            Self::Csw | Self::NotReady | Self::Failed => completion::NO_EVENT,
        }
    }
}

/// How a bulk transfer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkFail {
    /// Not started: the transfer window of the request was over. Nothing
    /// is on the ring.
    Refused,
    /// Completion code of the failure (`completion::NO_EVENT`: timeout,
    /// the TD was retired).
    Failed(u8),
}

impl BulkFail {
    fn code(self) -> u8 {
        match self {
            Self::Refused => completion::NO_EVENT,
            Self::Failed(code) => code,
        }
    }
}

/// What `usb_fault` injects (tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Injection {
    /// A USB Transaction Error reported on the CBW without touching the
    /// controller (the endpoint stays Running).
    Cbw,
    /// A CSW read with no command outstanding: the device STALLs its bulk
    /// IN pipe, a real halt.
    Csw,
    /// The CSW read of a complete command is made to look as if the device
    /// moved only the signature and the tag (8 bytes), the previous
    /// command's residue and status still in the buffer: an invalid CSW
    /// (BOT 5.2, 6.3), followed by a real Reset Recovery (incident 0007).
    ShortCsw,
    /// The data stage of a complete command is reported one sector (512
    /// bytes) short of what was asked for: the command passed but did not
    /// move everything, so the request answers Io, never a success, and
    /// the transport stays Ready (incident 0010).
    ShortData,
    /// The endpoint is given up as if no command put it back: Disable
    /// Slot and the whole re-enumeration run.
    Reenum,
    /// Like `Reenum`, but the Disable Slot completion is read as Context
    /// State Error (xHCI 6.4.5): a slot the controller did not release, so
    /// it may still own the request's buffer (incident 0017).
    Unreleased,
    /// The device is treated as not answering any more: the request
    /// answers Io and the idle loop resets the port and enumerates the
    /// device again on its slot (Reset Device, xHCI 4.6.11).
    Reset,
    /// A HID report that arrived is treated as a USB Transaction Error;
    /// the idle loop stops the interrupt endpoint (a real Stopped state)
    /// and recovers it.
    Hid,
}

/// What happens to the client buffer of the block request being served
/// once it is answered (incident 0017).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferFate {
    /// Every TD on it completed or was retired, or its slot was released:
    /// the handle is closed.
    Release,
    /// A TD on it could not be retired and the Disable Slot of this slot
    /// has not completed: kept (`Controller::owned`) until it does.
    Hold(u8),
    /// The Disable Slot completed without releasing the slot (xHCI 4.6.4):
    /// the controller may own the buffer for good, and with no controller
    /// reset after boot nothing will say otherwise. Never closed.
    ForGood,
}

/// Outcome of a command TRB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Issued {
    /// Completion code and slot.
    Done(u8, u8),
    /// Enqueued and rung, no completion by the deadline: the TRB (its
    /// physical address) is still the controller's.
    Pending(u64),
    /// Not enqueued: the deadline was (nearly) over.
    Refused,
}

#[derive(Clone, Copy)]
enum Decoder {
    None,
    Keyboard(HidBootKeyboard),
    Mouse(HidBootMouse),
}

/// The block request a strict halt dumps.
#[derive(Clone, Copy)]
struct HaltedRequest {
    write: bool,
    lba: u64,
    count: u16,
}

/// What a failed bulk transfer looked like (the decision table line).
struct TransferFault {
    stage: Stage,
    dir_in: bool,
    dci: u8,
    code: u8,
    event: Option<Trb>,
    start: u64,
    injected: bool,
}

struct Controller {
    /// `strict` boot flag: halt on the first transport failure.
    strict: bool,
    /// `usb_fault=N|idle[:cbw|:csw|:short-csw|:short-data|:reenum|:unreleased|:reset|:hid]`
    /// (tests): an injected failure.
    fault_inject: FaultInject,
    /// The injected failure due in the current request, until it is used.
    inject_now: Option<Injection>,
    /// What happens to the client buffer of the block request being
    /// served (incident 0017).
    request_buffer: BufferFate,
    /// Disable Slot command TRB issued without a completion yet, and the
    /// slot it disables.
    pending_disable: Option<(u64, u8)>,
    /// A slot whose Disable Slot was refused for lack of time: the idle
    /// loop issues it before anything else.
    slot_to_disable: Option<u8>,
    /// Client buffers of earlier requests whose TD could not be retired,
    /// kept until the outstanding Disable Slot completes: released when it
    /// released the slot, kept for good otherwise (incident 0017). Kept
    /// here, not in `Storage`, so that a re-enumeration does not lose them.
    owned: DeviceOwned<2>,
    /// The slot whose Disable Slot decides the fate of `owned`.
    owned_slot: Option<u8>,
    /// `usb_fault=...:unreleased` (tests): the next Disable Slot
    /// completion is read as Context State Error.
    disable_unreleased: bool,
    /// Client buffers mapped for the controller's IOMMU domain (ADR 0038).
    client_buffers: ClientBuffers,
    /// Last block request and last HID report (ms), for `usb_fault=idle`.
    last_request_ms: u64,
    last_report_ms: u64,
    op: Mmio,
    runtime: Mmio,
    doorbells: Mmio,
    /// HCIVERSION (BCD): Soft Retry needs 1.1.
    version: u16,
    params: Params,
    arena: Arena,
    /// Arena bytes in use once the boot enumeration ended: a
    /// re-enumeration must not add to it (incident 0001 audit 5 M4).
    arena_after_boot: u64,
    cmd: Ring,
    events: EventRing,
    events_virt: u64,
    dcbaa_virt: u64,
    devices: [Option<UsbDevice>; MAX_DEVICES],
    storage: Option<Storage>,
    /// Input events decoded from HID reports, waiting for the client.
    pending: Pending,
    /// Memory of the slot whose Disable Slot is outstanding (refused or
    /// pending): it becomes `spare_slot` only when that command completes
    /// releasing the slot (`completion::disable_slot_released`, xHCI
    /// 4.6.4), and is abandoned otherwise.
    disabling: Option<(u8, SlotMemory)>,
    /// Slot memory of a device whose slot was released, reused by the next
    /// slot: the linear arena does not grow with re-enumerations.
    spare_slot: Option<SlotMemory>,
    /// Storage memory handed to the storage being enumerated again.
    spare_storage: Option<StorageMemory>,
    /// Descriptor buffer (512 bytes) of every enumeration: (physical,
    /// virtual). Enumerations never overlap.
    desc_buf: (u64, u64),
    clock: Clock,
    keyboards: u8,
    mice: u8,
    /// The ports were routed from the EHCI controllers to the xHCI (Intel):
    /// devices need more time to show up.
    ports_switched: bool,
}

/// Input events decoded from HID reports, waiting for the client.
struct Pending {
    queue: EventQueue,
    /// The total of dropped events last printed.
    dropped_reported: u64,
}

impl Pending {
    /// Queues `ev`; when full the oldest event is dropped, never silently:
    /// the first drop of the boot is printed and recorded (`usb hid evt
    /// dropped`), and the running total is printed after the client next
    /// reads (`report_drops`).
    fn push(&mut self, ev: Event, at: u64) {
        let wire = WireEvent {
            kind: ev.kind,
            code: ev.code,
            value: ev.value,
        };
        if self.queue.push(wire, at) && self.queue.dropped() == 1 {
            say(b"input event queue full: the oldest events are dropped");
            record("usb hid evt dropped", QUEUE_LEN as u64);
        }
    }

    fn tell_gone_reader(&mut self) {
        if self.queue.first_gone_reader() {
            say(b"a reader died with its read waiting; its events wait for the next read");
        }
    }

    fn report_drops(&mut self) {
        let dropped = self.queue.dropped();
        if dropped > self.dropped_reported {
            self.dropped_reported = dropped;
            sys::print(b"usb-input: ");
            num(dropped);
            sys::print(b" input events dropped in this boot (queue full)\n");
        }
    }
}

/// Initialization exit codes (stable for the scenarios).
mod exit {
    pub const MAP_BAR: u64 = 10;
    pub const NOT_XHCI: u64 = 11;
    pub const DMA: u64 = 12;
    pub const RESET_TIMEOUT: u64 = 13;
    pub const START_TIMEOUT: u64 = 14;
    /// `strict`: a transport failure halted the driver (ADR 0028).
    pub const TRANSPORT_FAULT: u64 = 15;
}

impl Controller {
    /// The controller on the heap (incident 0018): built in `main`'s frame
    /// it took 20 KiB of the 64 KiB stack. Out of line, so the copy an
    /// unoptimized build may make on the way into the `Box` is popped
    /// before the service loop.
    #[inline(never)]
    fn open(mask: u64, clock: Clock) -> Result<Box<Self>, u64> {
        let Some(bar) = bar_handle(mask, 0) else {
            say(b"no BAR 0 or no IRQ among the capabilities");
            return Err(exit::MAP_BAR);
        };
        let base = Mmio(sys::mmio_map(bar).map_err(|_| {
            say(b"could not map BAR 0");
            exit::MAP_BAR
        })?);
        let cfg = Mmio(sys::mmio_map(CFG).map_err(|_| {
            say(b"could not map the configuration space");
            exit::MAP_BAR
        })?);
        // The capability registers are read as 32-bit words: QEMU does not
        // serve 16-bit accesses to HCIVERSION.
        let cap0 = base.r32(cap::CAPLENGTH);
        let caplength = (cap0 & 0xFF) as u64;
        let version = (cap0 >> 16) as u16;
        if caplength < 0x20 || version < 0x0100 || version == 0xFFFF {
            sys::print(b"usb-input: CAPLENGTH ");
            num(caplength);
            sys::print(b" HCIVERSION ");
            num(version as u64);
            sys::print(b" HCSPARAMS1 ");
            num(base.r32(cap::HCSPARAMS1) as u64);
            sys::print(b"\n");
            return Err(exit::NOT_XHCI);
        }
        let params = Params::decode(
            base.r32(cap::HCSPARAMS1),
            base.r32(cap::HCSPARAMS2),
            base.r32(cap::HCCPARAMS1),
        );
        let op = Mmio(base.0 + caplength);
        let runtime = Mmio(base.0 + (base.r32(cap::RTSOFF) & !0x1F) as u64);
        let doorbells = Mmio(base.0 + (base.r32(cap::DBOFF) & !0x3) as u64);
        let region = DmaRegion::alloc(params.dma_arena_bytes()).map_err(|_| exit::DMA)?;
        let mut arena = Arena { region, next: 0 };

        // The firmware may still own the controller (SMM):
        // ask it to hand it over before touching it.
        bios_handoff(&base, &clock);

        // Stop and reset the controller.
        op.w32(op::USBCMD, op.r32(op::USBCMD) & !op::CMD_RUN);
        wait_bits(&op, op::USBSTS, op::STS_HALTED, true, &clock, 200).ok_or(exit::RESET_TIMEOUT)?;
        op.w32(op::USBCMD, op::CMD_RESET);
        wait_bits(&op, op::USBCMD, op::CMD_RESET, false, &clock, 500).ok_or(exit::RESET_TIMEOUT)?;
        wait_bits(&op, op::USBSTS, op::STS_NOT_READY, false, &clock, 500)
            .ok_or(exit::RESET_TIMEOUT)?;
        // Reset done: only now may the controller reach memory (ADR 0038).
        sys::pci::enable_bus_master(CFG).map_err(|_| {
            say(b"could not enable bus mastering");
            exit::MAP_BAR
        })?;

        // Structures: DCBAA, scratchpads, command ring, event ring and ERST.
        let slots = params.max_slots as u64;
        let (dcbaa_phys, dcbaa_virt) = arena.alloc((slots + 1) * 8, 64).ok_or(exit::DMA)?;
        if params.scratchpad_buffers > 0 {
            let n = params.scratchpad_buffers as u64;
            let (arr_phys, arr_virt) = arena.alloc(n * 8, 64).ok_or(exit::DMA)?;
            for i in 0..n {
                let (page_phys, _) = arena.alloc(4096, 4096).ok_or(exit::DMA)?;
                // SAFETY: scratchpad array entry inside the DMA buffer.
                unsafe { ((arr_virt + 8 * i) as *mut u64).write_volatile(page_phys) };
            }
            // SAFETY: DCBAA entry 0.
            unsafe { (dcbaa_virt as *mut u64).write_volatile(arr_phys) };
        }
        let cmd = Ring::new(&mut arena, CMD_RING_TRBS).ok_or(exit::DMA)?;
        let (ev_phys, ev_virt) = arena
            .alloc((EVENT_RING_TRBS * Trb::LEN) as u64, 64)
            .ok_or(exit::DMA)?;
        let (erst_phys, erst_virt) = arena.alloc(16, 64).ok_or(exit::DMA)?;
        // SAFETY: ERST entry inside the DMA buffer.
        unsafe {
            (erst_virt as *mut [u8; 16]).write_volatile(erst_entry(ev_phys, EVENT_RING_TRBS as u16))
        };
        let events = EventRing::new(ev_phys, EVENT_RING_TRBS);
        let desc_buf = arena.alloc(512, 64).ok_or(exit::DMA)?;

        op.w32(op::CONFIG, params.max_slots as u32);
        op.w64(op::DCBAAP, dcbaa_phys);
        op.w64(op::CRCR, cmd.state.phys | op::CRCR_RCS);
        runtime.w32(rt::ERSTSZ, 1);
        runtime.w64(rt::ERDP, ev_phys);
        runtime.w64(rt::ERSTBA, erst_phys);
        runtime.w32(rt::IMAN, rt::IMAN_ENABLE | rt::IMAN_PENDING);
        let ports_switched = intel_route_ports(&cfg);
        op.w32(op::USBCMD, op::CMD_RUN | op::CMD_INTE);
        wait_bits(&op, op::USBSTS, op::STS_HALTED, false, &clock, 200)
            .ok_or(exit::START_TIMEOUT)?;

        Ok(Box::new(Self {
            strict: false,
            fault_inject: FaultInject::None,
            inject_now: None,
            request_buffer: BufferFate::Release,
            pending_disable: None,
            slot_to_disable: None,
            owned: DeviceOwned::new(),
            owned_slot: None,
            disable_unreleased: false,
            client_buffers: ClientBuffers::new(),
            last_request_ms: 0,
            last_report_ms: 0,
            op,
            runtime,
            doorbells,
            version,
            params,
            arena,
            arena_after_boot: 0,
            cmd,
            events,
            events_virt: ev_virt,
            dcbaa_virt,
            devices: [None, None, None, None],
            storage: None,
            pending: Pending {
                queue: EventQueue::new(),
                dropped_reported: 0,
            },
            disabling: None,
            spare_slot: None,
            spare_storage: None,
            desc_buf,
            clock,
            keyboards: 0,
            mice: 0,
            ports_switched,
        }))
    }

    fn portsc(&self, port: u8) -> u32 {
        self.op
            .r32(op::PORT_BASE + op::PORT_STRIDE * (port as u64 - 1))
    }

    fn write_portsc(&self, port: u8, v: u32) {
        self.op
            .w32(op::PORT_BASE + op::PORT_STRIDE * (port as u64 - 1), v)
    }

    fn ring_doorbell(&self, slot: u8, target: u32) {
        self.doorbells.w32(4 * slot as u64, target);
    }

    /// `now + ms`: the deadline of a step outside any request.
    fn after(&self, ms: u64) -> u64 {
        self.clock.now() + ms
    }

    /// Deadline of a step that may take `ms` within `deadline`; 0 (every
    /// step refuses it) when `deadline` is (nearly) over.
    fn until(&self, deadline: u64, ms: u64) -> u64 {
        step_deadline(self.clock.now(), deadline, ms).unwrap_or(0)
    }

    /// Sleeps (interrupts still wake the loop) until the clock reads `t`.
    fn wait_until(&self, t: u64) {
        while self.clock.now() < t {
            let _ = sys::irq_wait(IRQ, 10);
        }
    }

    /// Next event in the ring, if any; updates ERDP.
    fn next_event(&mut self) -> Option<Trb> {
        let slot = self.events_virt + (self.events.dequeue * Trb::LEN) as u64;
        // The cycle dword first, the whole TRB after it (`EventRing::take`):
        // two volatile loads, which x86 keeps in order.
        let ev = self.events.take(
            // SAFETY: the last dword of an event ring slot, inside the
            // driver's DMA arena (as for `read_trb`).
            || u32::from_le(unsafe { ((slot + 12) as *const u32).read_volatile() }),
            || read_trb(slot),
        )?;
        self.runtime.w64(rt::ERDP, self.events.dequeue_pointer());
        Some(ev)
    }

    /// Sends a command and waits for its completion until `deadline`;
    /// while a Stop Endpoint on `stopping` (slot, DCI) is
    /// in flight, consumes the Stopped Transfer Event the controller
    /// generates for the interrupted TD before completing it (xHCI 4.6.9).
    /// Nothing is enqueued when `deadline` is within `MIN_START_MS`: a
    /// command never waited for would act on rings the driver has moved on
    /// from.
    fn command_on(&mut self, trb: Trb, deadline: u64, stopping: Option<(u8, u8)>) -> Issued {
        if deadline < self.clock.now() + MIN_START_MS {
            sys::print(b"usb-input: command type ");
            num(trb.trb_type() as u64);
            sys::print(b" refused: no time left\n");
            return Issued::Refused;
        }
        let phys = self.cmd.push(trb);
        self.ring_doorbell(0, 0);
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::COMMAND_COMPLETION && ev.parameter == phys {
                    return Issued::Done(ev.completion_code(), ev.slot_id());
                }
                if ev.trb_type() == trb_type::TRANSFER_EVENT
                    && stopping == Some((ev.slot_id(), ev.endpoint_id()))
                    && completion::is_stopped(ev.completion_code())
                {
                    sys::print(b"usb-input: dci ");
                    num(ev.endpoint_id() as u64);
                    sys::print(b": TD stopped at trb ");
                    hex(ev.parameter);
                    sys::print(b", ");
                    code_text(ev.completion_code());
                    sys::print(b"\n");
                    continue;
                }
                self.dispatch(ev);
            }
            if self.clock.now() >= deadline {
                return Issued::Pending(phys);
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    /// A recovery command, bounded by `deadline` and `COMMAND_TIMEOUT_MS`:
    /// its outcome is always printed (the decision table of incident 0001
    /// reads the codes) and anything but Success is a fault event
    /// (`usb cmd fail`). True on Success.
    fn command_checked(&mut self, trb: Trb, name: &[u8], deadline: u64) -> bool {
        self.command_ok(trb, name, deadline, COMMAND_TIMEOUT_MS)
    }

    /// `command_timed`, true on Success.
    fn command_ok(&mut self, trb: Trb, name: &[u8], deadline: u64, timeout_ms: u64) -> bool {
        matches!(
            self.command_timed(trb, name, deadline, timeout_ms),
            Issued::Done(completion::SUCCESS, _)
        )
    }

    /// A command bounded by `deadline` and its own `timeout_ms`, refused
    /// without touching the ring when less than `MIN_START_MS` is left
    /// (`step_deadline`). Its outcome is always printed and anything but
    /// Success is a fault event (`report_command`, incident 0001 audit 5
    /// M2: the enumeration's commands too).
    fn command_timed(&mut self, trb: Trb, name: &[u8], deadline: u64, timeout_ms: u64) -> Issued {
        let issued = match step_deadline(self.clock.now(), deadline, timeout_ms) {
            Some(deadline) => self.command_on(trb, deadline, None),
            None => Issued::Refused,
        };
        self.report_command(name, trb, issued);
        issued
    }

    /// Stop Endpoint (xHCI 4.6.9): the TD in progress is retired with a
    /// Stopped Transfer Event (consumed here) and the endpoint is Stopped.
    fn stop_endpoint(&mut self, slot: u8, dci: u8, deadline: u64) -> bool {
        let trb = Trb::stop_endpoint(slot, dci);
        let issued = match step_deadline(self.clock.now(), deadline, COMMAND_TIMEOUT_MS) {
            Some(deadline) => self.command_on(trb, deadline, Some((slot, dci))),
            None => Issued::Refused,
        };
        self.report_command(b"Stop Endpoint", trb, issued)
    }

    fn report_command(&self, name: &[u8], trb: Trb, issued: Issued) -> bool {
        sys::print(b"usb-input: ");
        sys::print(name);
        sys::print(b" slot ");
        // Enable Slot names no slot: the completion carries the new one.
        num(match issued {
            Issued::Done(_, slot) if trb.slot_id() == 0 => slot,
            _ => trb.slot_id(),
        } as u64);
        if trb.endpoint_id() != 0 {
            sys::print(b" dci ");
            num(trb.endpoint_id() as u64);
        }
        sys::print(b": ");
        let ok = match issued {
            Issued::Done(code, _) => {
                code_text(code);
                if code != completion::SUCCESS {
                    record("usb cmd fail", code as u64);
                }
                code == completion::SUCCESS
            }
            Issued::Pending(_) => {
                sys::print(b"no completion event (still pending)");
                record("usb cmd pending", trb.trb_type() as u64);
                false
            }
            Issued::Refused => {
                sys::print(b"refused: no time left");
                record("usb cmd refused", trb.trb_type() as u64);
                false
            }
        };
        sys::print(b"\n");
        ok
    }

    /// Events that arrive while waiting for something else, and the main
    /// loop's: reports go to `self.pending` wherever they arrive (incident
    /// 0001 audit 5 M5: key events seen inside a command or transfer wait
    /// used to be dropped), port changes are handled, a late
    /// Disable Slot releases the client buffer it was holding, anything
    /// else unexpected is printed.
    fn dispatch(&mut self, ev: Trb) {
        match ev.trb_type() {
            trb_type::TRANSFER_EVENT => {
                if !self.handle_transfer_event(ev) && !self.stray_on_hid(ev) {
                    self.stray_transfer(ev);
                }
            }
            trb_type::PORT_STATUS_CHANGE => self.port_change(ev.port_id()),
            trb_type::COMMAND_COMPLETION => {
                sys::print(b"usb-input: late command completion for trb ");
                hex(ev.parameter);
                sys::print(b": ");
                code_text(ev.completion_code());
                sys::print(b"\n");
                if let Some((_, slot)) = self
                    .pending_disable
                    .filter(|&(phys, _)| phys == ev.parameter)
                {
                    self.pending_disable = None;
                    if ev.completion_code() != completion::SUCCESS {
                        record("usb cmd fail", ev.completion_code() as u64);
                    }
                    self.slot_disabled(slot, ev.completion_code());
                }
            }
            _ => {}
        }
    }

    /// The Disable Slot of `slot` completed with `code` (xHCI 4.6.4: on
    /// Success the controller owns none of the slot's TDs, contexts or
    /// rings any more). The slot was released only on Success or Slot Not
    /// Enabled (`completion::disable_slot_released`; Linux frees the virt
    /// device likewise only then). Client buffers kept for its TDs are
    /// released then, and kept for good otherwise (incident 0017; the
    /// caller recorded a code other than Success). The slot's memory
    /// becomes the spare and its DCBAA entry is cleared only when the slot
    /// was released; otherwise the controller may still use it and it is
    /// abandoned (incident 0001 audit 5 M1).
    fn slot_disabled(&mut self, slot: u8, code: u8) {
        let released = completion::disable_slot_released(code);
        if self.owned_slot == Some(slot) {
            self.settle_owned(released, code);
        }
        if self.request_buffer == BufferFate::Hold(slot) {
            self.request_buffer = if released {
                BufferFate::Release
            } else {
                BufferFate::ForGood
            };
        }
        let mem = match self.disabling.take() {
            Some((s, mem)) if s == slot => mem,
            other => {
                self.disabling = other;
                return;
            }
        };
        if !released {
            self.abandon_slot_memory(b"Disable Slot did not release the slot", code);
            return;
        }
        self.set_dcbaa(slot, 0);
        if self.spare_slot.is_some() {
            self.abandon_slot_memory(b"a spare is already held", code);
            return;
        }
        self.spare_slot = Some(mem);
    }

    /// The Disable Slot of `owned_slot` completed (`released` or not, with
    /// `code`) or was superseded: the client buffers kept for its TDs are
    /// released, or kept for good (printed and recorded either way).
    fn settle_owned(&mut self, released: bool, code: u8) {
        self.owned_slot = None;
        if released {
            let n = self.owned.release();
            if n > 0 {
                say(b"slot released: the client buffer kept for it is released (xHCI 4.6.4)");
                record("usb buf released", n as u64);
            }
        } else {
            let n = self.owned.abandon();
            if n > 0 {
                say(b"slot not released: the client buffer kept for it is kept for good");
                record("usb buf for good", code as u64);
            }
        }
    }

    /// Hands back the handle of the client buffer of the block request
    /// just answered, as `request_buffer` says (incident 0017): closed,
    /// kept until the slot's Disable Slot completes, or kept for good.
    fn settle_request_buffer(&mut self, buffer: Handle) {
        match core::mem::replace(&mut self.request_buffer, BufferFate::Release) {
            BufferFate::Release => {
                let _ = sys::handle_close(buffer);
            }
            BufferFate::Hold(slot) if self.owned_slot.is_none_or(|s| s == slot) => {
                match self.owned.keep(buffer) {
                    Kept::Held => {
                        self.owned_slot = Some(slot);
                        say(b"client buffer kept until its slot is disabled: a TD on it could not be retired")
                    }
                    Kept::ForGood => say(b"client buffer kept for good: no room to track it until its slot is disabled"),
                }
                record("usb buf kept", slot as u64);
            }
            BufferFate::Hold(slot) => {
                // Buffers of another slot wait for their own Disable Slot.
                self.owned.keep_for_good(buffer);
                say(b"client buffer kept for good: buffers of another slot are still held");
                record("usb buf for good", slot as u64);
            }
            BufferFate::ForGood => {
                self.owned.keep_for_good(buffer);
                say(b"client buffer kept for good: its slot was not released");
                record("usb buf for good", 0);
            }
        }
    }

    /// Slot memory that is never reused: printed with what stays
    /// allocated, recorded (`usb slot mem abandoned`).
    fn abandon_slot_memory(&self, why: &[u8], code: u8) {
        sys::print(b"usb-input: slot memory abandoned: ");
        sys::print(why);
        sys::print(b" (");
        code_text(code);
        sys::print(b"); ");
        num(
            ((DEVICE_CONTEXTS + INPUT_CONTEXTS) * self.params.context_bytes()
                + XFER_RING_TRBS * Trb::LEN) as u64,
        );
        sys::print(b" bytes of the DMA arena stay allocated\n");
        record("usb slot mem abandoned", code as u64);
    }

    /// DCBAA entry of `slot`: its Output Device Context, or 0 once the
    /// slot is released.
    fn set_dcbaa(&self, slot: u8, phys: u64) {
        // SAFETY: DCBAA entry `slot` inside the driver's DMA buffer.
        unsafe { ((self.dcbaa_virt + 8 * slot as u64) as *mut u64).write_volatile(phys) };
    }

    /// Disable Slot (xHCI 4.6.4) for `slot`, bounded by `deadline`, with
    /// the slot's memory `mem` (`None` when it was handed over before, for
    /// a retried command): a completion decides the fate of the client
    /// buffers kept for the slot and of its memory (`slot_disabled`); a
    /// pending one is matched later in `dispatch`; a refused one is retried
    /// by the idle loop first thing.
    fn issue_disable_slot(&mut self, slot: u8, mem: Option<SlotMemory>, deadline: u64) -> Issued {
        if let Some(mem) = mem {
            if let Some((other, _)) = self.disabling.replace((slot, mem)) {
                sys::print(b"usb-input: Disable Slot of slot ");
                num(other as u64);
                sys::print(b" still outstanding\n");
                self.abandon_slot_memory(
                    b"another Disable Slot is outstanding",
                    completion::NO_EVENT,
                );
                // Nothing will confirm that slot released either.
                if self.owned_slot == Some(other) {
                    self.settle_owned(false, completion::NO_EVENT);
                }
                if self.request_buffer == BufferFate::Hold(other) {
                    self.request_buffer = BufferFate::ForGood;
                }
            }
        }
        let trb = Trb::disable_slot(slot);
        let mut issued = match step_deadline(self.clock.now(), deadline, COMMAND_TIMEOUT_MS) {
            Some(deadline) => self.command_on(trb, deadline, None),
            None => Issued::Refused,
        };
        let unreleased = core::mem::take(&mut self.disable_unreleased);
        if let (Issued::Done(_, s), true) = (issued, unreleased) {
            say(b"injected fault: the Disable Slot completion is read as Context State Error");
            issued = Issued::Done(completion::CONTEXT_STATE_ERROR, s);
        }
        self.report_command(b"Disable Slot", trb, issued);
        match issued {
            Issued::Done(code, _) => self.slot_disabled(slot, code),
            Issued::Pending(phys) => self.pending_disable = Some((phys, slot)),
            Issued::Refused => self.slot_to_disable = Some(slot),
        }
        issued
    }

    /// Deadline for a control transfer that may take `transfer_ms`, with
    /// the EP0 recovery reserve on top, within `deadline`.
    fn control_step(&self, deadline: u64, transfer_ms: u64) -> u64 {
        self.until(deadline, transfer_ms + EP0_RECOVERY_MS)
    }

    /// A Transfer Event that belongs to no TD in progress (a late
    /// completion of a TD that was already retired, or Panther Point's
    /// spurious Success after a Short Packet, audit 1 F15): printed; an
    /// error code is also recorded (`usb stray err`, audit 5 L3).
    fn stray_transfer(&self, ev: Trb) {
        sys::print(b"usb-input: stray transfer event: slot ");
        num(ev.slot_id() as u64);
        sys::print(b" dci ");
        num(ev.endpoint_id() as u64);
        sys::print(b" ");
        code_text(ev.completion_code());
        sys::print(b" trb ");
        hex(ev.parameter);
        sys::print(b" residual ");
        num(ev.residual() as u64);
        sys::print(b"\n");
        let code = ev.completion_code();
        if !completion::is_success(code) && !completion::is_stopped(code) {
            record("usb stray err", code as u64);
        }
    }

    /// Port Status Change: PORTSC is printed before its change bits are
    /// cleared. A connect, reset or warm reset change on the storage port
    /// means its slot addresses nothing any more (xHCI 4.19.2; USB 2.0
    /// 9.1): the storage is unusable until enumerated again. A new
    /// connection (CSC with CCS) starts the re-enumeration count afresh.
    fn port_change(&mut self, port: u8) {
        if port < 1 || port > self.params.max_ports {
            return;
        }
        let sc = self.portsc(port);
        let mut text = [0u8; 96];
        sys::print(b"usb-input: port ");
        num(port as u64);
        sys::print(b" status change: PORTSC ");
        hex(sc as u64);
        sys::print(b" ");
        sys::print(op::describe_portsc(sc, &mut text).as_bytes());
        sys::print(b"\n");
        self.write_portsc(
            port,
            (sc & op::PORTSC_POWER) | (sc & op::PORTSC_CHANGE_BITS),
        );
        let now = self.clock.now();
        let Some(st) = self.storage.as_mut() else {
            return;
        };
        if st.port != port || sc & op::PORTSC_DEVICE_LOST == 0 {
            return;
        }
        record("usb dev lost", (sc & 0xFF) as u64);
        if sc & op::PORTSC_CCS != 0 {
            say(b"the storage device reconnected or was reset: re-enumerating");
            if sc & op::PORTSC_CSC != 0 {
                st.reenumerations = 0;
                st.reconnected = true;
            }
            st.transport = Transport::Reenumerate {
                not_before: now + DEBOUNCE_MS,
                reset_port: false,
            };
        } else {
            say(b"the storage device disconnected: requests fail until it is back");
            st.transport = Transport::Gone;
        }
    }

    /// Control transfer on EP0 of `slot_index`: `Ok(residual)` or the
    /// completion code of the stage that failed (`completion::NO_EVENT`
    /// on timeout, or when nothing was sent because the transfer window
    /// was already within `MIN_START_MS`). `EP0_RECOVERY_MS` of `deadline`
    /// are reserved: the transfer ends by `deadline - EP0_RECOVERY_MS` and,
    /// after a failure, EP0 is recovered like any endpoint (xHCI 4.10.2.1
    /// halts control endpoints too) by `deadline`, so the next request can
    /// be sent and no step overruns. An error event may point at the Setup
    /// or Data Stage TRB, not only the Status Stage.
    fn control(
        &mut self,
        slot_index: usize,
        setup: SetupPacket,
        data: Option<(u64, u16)>,
        deadline: u64,
    ) -> Result<u32, u8> {
        let transfer_end = deadline.saturating_sub(EP0_RECOVERY_MS);
        if transfer_end < self.clock.now() + MIN_START_MS {
            sys::print(b"usb-input: control transfer ");
            hex(setup.request_type as u64);
            sys::print(b"/");
            hex(setup.request as u64);
            sys::print(b" refused: no time left\n");
            return Err(completion::NO_EVENT);
        }
        let (slot, td, n) = {
            let dev = self.devices[slot_index]
                .as_mut()
                .ok_or(completion::NO_EVENT)?;
            let mut td = [0u64; 3];
            td[0] = dev.mem.ep0.push(Trb::setup_stage(&setup));
            let mut n = 1;
            if let Some((phys, len)) = data {
                td[n] = dev.mem.ep0.push(Trb::data_stage(phys, len, setup.is_in()));
                n += 1;
            }
            td[n] = dev
                .mem
                .ep0
                .push(Trb::status_stage(data.is_none() || !setup.is_in()));
            n += 1;
            (dev.slot, td, n)
        };
        self.ring_doorbell(slot, 1);
        let start = self.clock.now();
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::TRANSFER_EVENT
                    && ev.slot_id() == slot
                    && ev.endpoint_id() == 1
                {
                    if !td[..n].contains(&ev.parameter) {
                        self.stray_transfer(ev);
                        continue;
                    }
                    match ev.completion_code() {
                        completion::SUCCESS | completion::SHORT_PACKET
                            if ev.parameter == td[n - 1] =>
                        {
                            return Ok(ev.residual())
                        }
                        completion::SUCCESS | completion::SHORT_PACKET => continue,
                        code => {
                            self.report_control_failure(setup, code, start, Some(ev));
                            self.recover_endpoint(slot_index, 1, code, deadline);
                            return Err(code);
                        }
                    }
                }
                self.dispatch(ev);
            }
            if self.clock.now() >= transfer_end {
                self.report_control_failure(setup, completion::NO_EVENT, start, None);
                self.recover_endpoint(slot_index, 1, completion::NO_EVENT, deadline);
                return Err(completion::NO_EVENT);
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    fn report_control_failure(&self, setup: SetupPacket, code: u8, start: u64, ev: Option<Trb>) {
        sys::print(b"usb-input: control transfer ");
        hex(setup.request_type as u64);
        sys::print(b"/");
        hex(setup.request as u64);
        sys::print(b" failed: ");
        code_text(code);
        sys::print(b" after ");
        num(self.clock.now() - start);
        sys::print(b" ms");
        if let Some(ev) = ev {
            sys::print(b", stage trb ");
            hex(ev.parameter);
            sys::print(b" residual ");
            num(ev.residual() as u64);
        }
        sys::print(b"\n");
    }

    /// Resets port `port` if it is not enabled or `force_reset` (a device
    /// that stopped answering: the last resort), clears its change bits and
    /// consumes the Port Status Change events this produced, within
    /// `deadline`. Returns the port speed (PSIV).
    fn prepare_port(
        &mut self,
        port: u8,
        force_reset: bool,
        deadline: u64,
    ) -> Result<u8, &'static str> {
        let sc = self.portsc(port);
        if sc & op::PORTSC_CCS == 0 {
            return Err("no device");
        }
        if sc & op::PORTSC_PED == 0 || force_reset {
            // USB 2: the port must be reset; USB 3 enables itself. Not
            // started without time to see it through (USB 2.0 7.1.7.5:
            // the reset itself lasts at least 10 ms).
            let Some(reset_deadline) = step_deadline(self.clock.now(), deadline, 500) else {
                return Err("no time left to reset the port");
            };
            let keep = sc & op::PORTSC_POWER;
            self.write_portsc(port, keep | op::PORTSC_RESET);
            while self.portsc(port) & op::PORTSC_PED == 0 {
                if self.clock.now() >= reset_deadline {
                    return Err("the port is not enabled after the reset");
                }
                let _ = sys::irq_wait(IRQ, 5);
            }
        }
        // Clear the port change bits.
        let sc = self.portsc(port);
        self.write_portsc(
            port,
            (sc & op::PORTSC_POWER) | (sc & op::PORTSC_CHANGE_BITS),
        );
        let spd = op::port_speed(self.portsc(port));
        // Pending Port Status Change events (the reset's) are consumed now.
        while let Some(ev) = self.next_event() {
            self.dispatch(ev);
        }
        Ok(spd)
    }

    /// Enumerates a port with a device attached on a new slot: port reset,
    /// Enable Slot, then `enumerate_slot`, all within `deadline`. A failure
    /// after Enable Slot disables the slot again (xHCI 4.6.4), so no slot
    /// or device entry is left behind.
    fn enumerate_port(
        &mut self,
        port: u8,
        force_reset: bool,
        ready_wait_ms: u64,
        deadline: u64,
    ) -> Result<(), &'static str> {
        let spd = self.prepare_port(port, force_reset, deadline)?;
        let slot = match self.command_timed(Trb::enable_slot(), b"Enable Slot", deadline, 500) {
            Issued::Done(completion::SUCCESS, slot) if slot != 0 => slot,
            _ => return Err("Enable Slot failed"),
        };
        let result = self
            .add_device(slot)
            .and_then(|index| self.enumerate_slot(index, port, spd, ready_wait_ms, deadline));
        if result.is_err() {
            self.release_slot(slot, deadline);
        }
        result
    }

    /// Enumerates the device of `devices[index]` again on its own slot
    /// (incident 0001, audit 1 F10): the port is reset if `force_reset` or
    /// disabled, the slot is taken back to the Default state as its state
    /// requires (`reset_slot`), and `enumerate_slot` addresses and
    /// configures the device with the same Output Device Context, EP0 ring
    /// and Input Context. Within `deadline`; any failure disables the slot
    /// (xHCI 4.6.4) and leaves the next attempt to a new slot.
    fn reenumerate_in_place(
        &mut self,
        index: usize,
        port: u8,
        force_reset: bool,
        ready_wait_ms: u64,
        deadline: u64,
    ) -> Result<(), &'static str> {
        let Some(slot) = self.devices[index].as_ref().map(|d| d.slot) else {
            return Err("no device entry");
        };
        let result = self
            .reset_slot(index, slot, port, force_reset, deadline)
            .and_then(|spd| self.enumerate_slot(index, port, spd, ready_wait_ms, deadline));
        if result.is_err() {
            self.release_slot(slot, deadline);
        }
        result
    }

    /// Port reset (if due), then the slot of `devices[index]` back to the
    /// Default state with Reset Device (xHCI 4.6.11, which the section has
    /// software issue once the device itself was reset on its port). Only
    /// an Addressed or Configured slot accepts it; any other state is a
    /// failure and the caller replaces the slot. Its EP0 ring and endpoint
    /// entries are emptied. Returns the port speed.
    fn reset_slot(
        &mut self,
        index: usize,
        slot: u8,
        port: u8,
        force_reset: bool,
        deadline: u64,
    ) -> Result<u8, &'static str> {
        let spd = self.prepare_port(port, force_reset, deadline)?;
        let state = self.slot_state_of(index).ok_or("no device entry")?;
        let plan = slot_reuse(state);
        sys::print(b"usb-input: slot ");
        num(slot as u64);
        sys::print(b" is ");
        sys::print(slot_state::name(state).as_bytes());
        sys::print(match plan {
            SlotReuse::ResetDevice => b": Reset Device (xHCI 4.6.11), then Address Device\n",
            SlotReuse::Replace => b": Reset Device does not apply, the slot is replaced\n",
        });
        match plan {
            SlotReuse::ResetDevice => {
                if !self.command_checked(Trb::reset_device(slot), b"Reset Device", deadline) {
                    return Err("Reset Device failed");
                }
            }
            SlotReuse::Replace => return Err("slot not addressed"),
        }
        // Reset Device aborted every transfer and disabled every endpoint
        // but EP0, which nothing rings until Address Device hands the
        // controller its ring again (4.6.5 takes the TR Dequeue Pointer and
        // DCS from the Input Context): the EP0 ring is emptied in place.
        let dev = self.devices[index].as_mut().ok_or("no device entry")?;
        dev.mem.ep0.reset();
        dev.endpoints = [None, None, None, None];
        dev.decoders = [Decoder::None; MAX_DEVICES];
        Ok(spd)
    }

    /// A new device entry for `slot` with its slot memory, the DCBAA
    /// pointing at its Output Device Context. Returns its index.
    fn add_device(&mut self, slot: u8) -> Result<usize, &'static str> {
        let index = self
            .devices
            .iter()
            .position(|d| d.is_none())
            .ok_or("too many devices")?;
        let mem = self.slot_memory().ok_or("no DMA")?;
        self.set_dcbaa(slot, mem.dev_ctx.0);
        self.devices[index] = Some(UsbDevice {
            slot,
            mem,
            endpoints: [None, None, None, None],
            decoders: [Decoder::None; MAX_DEVICES],
            slot_ctx: SlotContext::default(),
        });
        Ok(index)
    }

    /// Slot memory for a new slot: the spare of a released slot (it only
    /// becomes the spare once its Disable Slot released the slot, so the
    /// controller owns none of it, xHCI 4.6.4), with its Output Device
    /// Context zeroed (4.3.3) and its EP0 ring emptied; otherwise a new
    /// allocation.
    fn slot_memory(&mut self) -> Option<SlotMemory> {
        let ctx_bytes = self.params.context_bytes();
        if let Some(mut mem) = self.spare_slot.take() {
            say(b"reusing the DMA memory of a released slot");
            // SAFETY: the Output Device Context inside the driver's DMA
            // buffer, owned by no slot now.
            unsafe {
                core::slice::from_raw_parts_mut(
                    mem.dev_ctx.1 as *mut u8,
                    DEVICE_CONTEXTS * ctx_bytes,
                )
                .fill(0)
            };
            mem.ep0.reset();
            return Some(mem);
        }
        let dev_ctx = self.arena.alloc((DEVICE_CONTEXTS * ctx_bytes) as u64, 64)?;
        let ep0 = Ring::new(&mut self.arena, XFER_RING_TRBS)?;
        let in_len = (INPUT_CONTEXTS * ctx_bytes) as u64;
        let (in_phys, in_virt) = self.arena.alloc(in_len, 64)?;
        Some(SlotMemory {
            dev_ctx,
            ep0,
            input_ctx: (in_phys, in_virt, in_len),
        })
    }

    /// The device entry of `slot` goes away and the slot is disabled
    /// (xHCI 4.6.4), once, within `deadline`, with its memory handed to
    /// `issue_disable_slot`; a refused or pending Disable Slot is finished
    /// by the idle loop.
    fn release_slot(&mut self, slot: u8, deadline: u64) {
        let mem = self
            .devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.slot == slot))
            .and_then(|i| self.devices[i].take())
            .map(|dev| dev.mem);
        self.issue_disable_slot(slot, mem, deadline);
    }

    /// Slot State of `devices[index]`, as the controller wrote it in the
    /// Output Slot Context.
    fn slot_state_of(&self, index: usize) -> Option<u8> {
        let dev = self.devices[index].as_ref()?;
        // SAFETY: Output Slot Context of the slot, inside the driver's DMA
        // buffer; the controller writes it, so it is read volatile.
        let bytes = unsafe { (dev.mem.dev_ctx.1 as *const [u8; 16]).read_volatile() };
        slot_status(&bytes)
    }

    /// Everything after the slot is Enabled or Default (xHCI 4.3): address,
    /// descriptors, configuration, and the HID or storage interfaces of the
    /// device in `devices[index]`, on its own slot memory.
    fn enumerate_slot(
        &mut self,
        index: usize,
        port: u8,
        spd: u8,
        ready_wait_ms: u64,
        deadline: u64,
    ) -> Result<(), &'static str> {
        let ctx_bytes = self.params.context_bytes();
        let (slot, ep0_phys, (in_phys, in_virt, in_len)) = {
            let dev = self.devices[index].as_ref().ok_or("no device entry")?;
            (dev.slot, dev.mem.ep0.state.phys, dev.mem.input_ctx)
        };
        let slot_ctx = SlotContext {
            route_string: 0,
            speed: spd,
            context_entries: 1,
            root_hub_port: port,
        };
        let ep0_ctx = EndpointContext::control(speed::default_ep0_max_packet(spd), ep0_phys);
        write_input_context(
            // SAFETY: input context inside the DMA buffer.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            0,
            0b11,
            &slot_ctx,
            &[(1, ep0_ctx)],
        );
        if !self.command_ok(
            Trb::address_device(in_phys, slot),
            b"Address Device",
            deadline,
            1000,
        ) {
            return Err("Address Device failed");
        }
        if let Some(dev) = self.devices[index].as_mut() {
            dev.slot_ctx = slot_ctx;
        }

        // Device descriptor in two steps (xHCI 4.3, step 7): first only
        // 8 bytes, which fit in any EP0, to learn bMaxPacketSize0; if it
        // differs from the assumed value the context is fixed with
        // Evaluate Context and only then is the full descriptor requested.
        let (buf_phys, buf_virt) = self.desc_buf;
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_DEVICE, 0, 8),
            Some((buf_phys, 8)),
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        )
        .map_err(|_| "GET_DESCRIPTOR(device, 8 bytes) failed")?;
        // SAFETY: the driver's 512-byte DMA buffer.
        let raw = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, 512) };
        let max_packet0 = raw[7] as u16;
        if raw[1] != aricode_xhci::request::DESC_DEVICE || max_packet0 == 0 {
            return Err("invalid device descriptor header");
        }
        if max_packet0 != speed::default_ep0_max_packet(spd) && spd != speed::SUPER {
            let ep0_ctx = EndpointContext::control(max_packet0, ep0_ctx.ring_phys);
            write_input_context(
                // SAFETY: same as above.
                unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
                ctx_bytes,
                0,
                0b10,
                &slot_ctx,
                &[(1, ep0_ctx)],
            );
            if !self.command_ok(
                Trb::evaluate_context(in_phys, slot),
                b"Evaluate Context",
                deadline,
                500,
            ) {
                return Err("Evaluate Context (EP0 packet size) failed");
            }
        }
        self.control(
            index,
            SetupPacket::get_descriptor(
                aricode_xhci::request::DESC_DEVICE,
                0,
                DeviceDescriptor::LEN,
            ),
            Some((buf_phys, DeviceDescriptor::LEN)),
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        )
        .map_err(|_| "GET_DESCRIPTOR(device) failed")?;
        let desc = DeviceDescriptor::parse(raw).ok_or("invalid device descriptor")?;
        // Full configuration.
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, 9),
            Some((buf_phys, 9)),
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        )
        .map_err(|_| "GET_DESCRIPTOR(config) failed")?;
        let total = u16::from_le_bytes([raw[2], raw[3]]).clamp(9, 512);
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, total),
            Some((buf_phys, total)),
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        )
        .map_err(|_| "GET_DESCRIPTOR(full config) failed")?;
        let mut hids = [None; 4];
        let (config_value, n) =
            aricode_xhci::parse_configuration(&raw[..total as usize], &mut hids)
                .ok_or("invalid configuration")?;
        if n == 0 {
            // No HID: bulk-only mass storage (ADR 0023)?
            let Some((value, iface)) = parse_storage_interface(&raw[..total as usize]) else {
                return Err("no boot HID or storage interfaces");
            };
            if self.storage.is_some() {
                return Err("a storage device is already present; ignoring");
            }
            // What the device says it is, and the speed the port negotiated
            // (PSIV), for the incident record; repeated in every dump.
            let identity = Identity {
                vendor: desc.vendor,
                product: desc.product,
                usb_version: desc.usb_version,
                max_power_ma: raw[8] as u16 * 2,
                speed: spd,
            };
            sys::print(b"usb-input: storage ");
            identity.print(port);
            sys::print(b"\n");
            self.control(
                index,
                SetupPacket::set_configuration(value),
                None,
                self.control_step(deadline, CONTROL_TIMEOUT_MS),
            )
            .map_err(|_| "SET_CONFIGURATION failed")?;
            return self.configure_storage(index, port, iface, identity, ready_wait_ms, deadline);
        }
        self.control(
            index,
            SetupPacket::set_configuration(config_value),
            None,
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        )
        .map_err(|_| "SET_CONFIGURATION failed")?;

        // Each HID interface: boot protocol, interrupt endpoint
        // configured and several reports in flight.
        let mut max_dci = 1u8;
        let mut eps: [(u8, EndpointContext); 4] = [(0, EndpointContext::default()); 4];
        let mut prepared = 0;
        for hid in hids.iter().flatten() {
            self.control(
                index,
                SetupPacket::hid_set_protocol(hid.interface, 0),
                None,
                self.control_step(deadline, CONTROL_TIMEOUT_MS),
            )
            .map_err(|_| "SET_PROTOCOL failed")?;
            // Optional: a keyboard may reject SET_IDLE and still work.
            let _ = self.control(
                index,
                SetupPacket::hid_set_idle(hid.interface),
                None,
                self.control_step(deadline, CONTROL_TIMEOUT_MS),
            );
            let d = dci(hid.endpoint, true);
            let ring = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("no DMA")?;
            let report_len = hid.max_packet_size.clamp(1, 64);
            let mut reports = [(0u64, 0u64, 0u64); REPORTS_IN_FLIGHT];
            for r in reports.iter_mut() {
                let (p, v) = self.arena.alloc(64, 64).ok_or("no DMA")?;
                *r = (0, p, v);
            }
            let ctx = EndpointContext::interrupt_in(
                hid.max_packet_size,
                aricode_xhci::interrupt_interval(spd, hid.b_interval),
                ring.state.phys,
            );
            eps[prepared] = (d, ctx);
            prepared += 1;
            max_dci = max_dci.max(d);
            let dev = self.devices[index].as_mut().ok_or("device")?;
            let ep_index = dev
                .endpoints
                .iter()
                .position(|e| e.is_none())
                .ok_or("endpoints")?;
            dev.decoders[ep_index] = match hid.protocol {
                1 => Decoder::Keyboard(HidBootKeyboard::new()),
                2 => Decoder::Mouse(HidBootMouse::new()),
                _ => Decoder::None,
            };
            dev.endpoints[ep_index] = Some(Endpoint {
                dci: d,
                address: 0x80 | hid.endpoint,
                ring,
                seen: false,
                reports,
                report_len,
                fault: None,
                failures: 0,
                faults: 0,
                refault: false,
                given_up: false,
            });
            match hid.protocol {
                1 => self.keyboards += 1,
                2 => self.mice += 1,
                _ => {}
            }
            self.report_interface(hid, port);
        }
        let mut add_flags = 1u32;
        for (d, _) in &eps[..prepared] {
            add_flags |= 1 << d;
        }
        let slot_ctx = SlotContext {
            context_entries: max_dci,
            ..slot_ctx
        };
        write_input_context(
            // SAFETY: same as above.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            0,
            add_flags,
            &slot_ctx,
            &eps[..prepared],
        );
        if !self.command_ok(
            Trb::configure_endpoint(in_phys, slot),
            b"Configure Endpoint",
            deadline,
            1000,
        ) {
            return Err("Configure Endpoint failed");
        }
        // Reports in flight.
        let dev = self.devices[index].as_mut().ok_or("device")?;
        dev.slot_ctx = slot_ctx;
        for ep in dev.endpoints.iter_mut().flatten() {
            for r in ep.reports.iter_mut() {
                r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
            }
            self.doorbells.w32(4 * slot as u64, ep.dci as u32);
        }
        Ok(())
    }

    /// Configures the bulk endpoints of a storage interface, identifies
    /// the device (INQUIRY), waits for it to become ready and reads its
    /// capacity. Only 512-byte blocks are supported. The transport is
    /// `Enumerating` meanwhile and becomes `Ready` at the end unless a port
    /// change moved it elsewhere. If the device does not answer the SCSI
    /// commands the storage is kept, its slot released by the caller, and
    /// a re-enumeration scheduled instead of dropping the device.
    fn configure_storage(
        &mut self,
        index: usize,
        port: u8,
        iface: StorageInterface,
        identity: Identity,
        ready_wait_ms: u64,
        deadline: u64,
    ) -> Result<(), &'static str> {
        // The memory of the storage this one replaces, if any: its bulk
        // endpoints are Disabled (Reset Device or a completed Disable Slot,
        // xHCI 4.6.11 / 4.6.4), so the controller follows neither ring and
        // both are emptied in place.
        let mem = match self.spare_storage.take() {
            Some(mut mem) => {
                mem.ring_in.reset();
                mem.ring_out.reset();
                mem
            }
            None => StorageMemory::alloc(&mut self.arena).ok_or("no DMA")?,
        };
        let StorageMemory {
            ring_in,
            ring_out,
            cbw,
            csw,
            small,
        } = mem;
        let (dci_in, dci_out) = (dci(iface.bulk_in, true), dci(iface.bulk_out, false));
        let eps = [
            (
                dci_in,
                EndpointContext::bulk(iface.max_packet_in, ring_in.state.phys, true),
            ),
            (
                dci_out,
                EndpointContext::bulk(iface.max_packet_out, ring_out.state.phys, false),
            ),
        ];
        let (slot, (in_phys, in_virt, in_len), slot_ctx) = {
            let dev = self.devices[index].as_ref().ok_or("device")?;
            (dev.slot, dev.mem.input_ctx, dev.slot_ctx)
        };
        let slot_ctx = SlotContext {
            context_entries: dci_in.max(dci_out),
            ..slot_ctx
        };
        let ctx_bytes = self.params.context_bytes();
        write_input_context(
            // SAFETY: the device input context inside the DMA buffer.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            0,
            1 | 1 << dci_in | 1 << dci_out,
            &slot_ctx,
            &eps,
        );
        if !self.command_ok(
            Trb::configure_endpoint(in_phys, slot),
            b"Configure Endpoint",
            deadline,
            1000,
        ) {
            return Err("Configure Endpoint (storage) failed");
        }
        if let Some(dev) = self.devices[index].as_mut() {
            dev.slot_ctx = slot_ctx;
        }
        self.storage = Some(Storage {
            device: Some(index),
            slot,
            port,
            interface: iface.interface,
            bulk_in: Bulk {
                dci: dci_in,
                address: 0x80 | iface.bulk_in,
                ring: ring_in,
                max_packet: iface.max_packet_in,
            },
            bulk_out: Bulk {
                dci: dci_out,
                address: iface.bulk_out,
                ring: ring_out,
                max_packet: iface.max_packet_out,
            },
            tag: 0,
            cbw,
            csw,
            small,
            capacity: 0,
            sync_cache: true,
            errors_reported: 0,
            transport: Transport::Enumerating,
            reenumerations: 0,
            reconnected: false,
            identity,
        });
        if let Err(why) = self.storage_init(port, ready_wait_ms, deadline) {
            // The caller disables the slot; the storage stays, to be
            // enumerated again from the idle loop.
            let not_before = self.after(1000);
            if let Some(st) = self.storage.as_mut() {
                st.device = None;
                st.transport = Transport::Reenumerate {
                    not_before,
                    reset_port: true,
                };
            }
            return Err(why);
        }
        if !self.transition(Transport::Enumerating, Transport::Ready) {
            say(b"the port changed during the enumeration: recovering again");
        }
        Ok(())
    }

    /// INQUIRY, TEST UNIT READY (with retries while the device starts up,
    /// `ready_wait_ms` at most) and READ CAPACITY(10), within `deadline`.
    fn storage_init(
        &mut self,
        port: u8,
        ready_wait_ms: u64,
        deadline: u64,
    ) -> Result<(), &'static str> {
        let (small_phys, small_virt) = self.storage.as_ref().ok_or("storage")?.small;
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        let small = || unsafe { core::slice::from_raw_parts(small_virt as *const u8, 64) };
        let budget = self
            .command_budget(deadline)
            .ok_or("no time left for INQUIRY")?;
        self.scsi(
            scsi::inquiry(),
            Some((small_phys, scsi::INQUIRY_LEN as u32, true)),
            budget,
        )
        .map_err(|_| "INQUIRY failed")?;
        let inq = scsi::Inquiry::parse(small()).ok_or("invalid INQUIRY")?;
        if inq.peripheral_type != 0 {
            return Err("not a direct access device (disk)");
        }
        let ready_until = self.after(ready_wait_ms).min(deadline);
        loop {
            let budget = self
                .command_budget(deadline)
                .ok_or("no time left for TEST UNIT READY")?;
            match self.scsi(scsi::test_unit_ready(), None, budget) {
                Ok(_) => break,
                Err(ScsiFail::Failed) => {
                    let sense = self.sense(budget);
                    if !matches!(
                        sense.map(|s| s.key),
                        Some(scsi::Sense::UNIT_ATTENTION | scsi::Sense::NOT_READY)
                    ) {
                        return Err("TEST UNIT READY rejected");
                    }
                }
                Err(_) => return Err("TEST UNIT READY: transport failure"),
            }
            if self.clock.now() >= ready_until {
                return Err("the device never becomes ready in time");
            }
            self.wait_until(self.after(100));
        }
        let budget = self
            .command_budget(deadline)
            .ok_or("no time left for READ CAPACITY")?;
        self.scsi(
            scsi::read_capacity10(),
            Some((small_phys, scsi::READ_CAPACITY_LEN as u32, true)),
            budget,
        )
        .map_err(|_| "READ CAPACITY failed")?;
        let (last_lba, block_len) =
            scsi::parse_read_capacity10(small()).ok_or("invalid READ CAPACITY")?;
        if block_len != 512 {
            return Err("blocks other than 512 bytes: not supported");
        }
        let capacity = last_lba as u64 + 1;
        if let Some(st) = self.storage.as_mut() {
            st.capacity = capacity;
        }
        sys::print(b"usb-input: USB storage on port ");
        num(port as u64);
        sys::print(b": ");
        sys::print(inq.vendor.trim_ascii());
        sys::print(b" ");
        sys::print(inq.product.trim_ascii());
        sys::print(b", ");
        num(capacity);
        sys::print(b" sectors\n");
        Ok(())
    }

    /// Budget of one SCSI command issued inside a maintenance or boot step
    /// ending at `deadline`: the request budget cut to the step, with a
    /// transfer window of at least `MIN_COMMAND_WINDOW_MS` (the reserve
    /// shrinks to fit); `None` refuses the command up front.
    fn command_budget(&self, deadline: u64) -> Option<Budget> {
        Budget::within(
            self.clock.now(),
            deadline,
            REQUEST_BUDGET_MS,
            RECOVERY_RESERVE_MS,
            MIN_COMMAND_WINDOW_MS,
        )
    }

    /// Endpoint Context state and TR Dequeue Pointer of `dci`, as the
    /// controller wrote them in the Output Device Context.
    fn ep_status(&self, slot_index: usize, dci: u8) -> Option<(u8, u64)> {
        let dev = self.devices[slot_index].as_ref()?;
        let base = dev.mem.dev_ctx.1 + (dci as usize * self.params.context_bytes()) as u64;
        // SAFETY: Output Endpoint Context of the slot, inside the driver's
        // DMA buffer; the controller writes it, so it is read volatile.
        let bytes = unsafe { (base as *const [u8; 16]).read_volatile() };
        endpoint_status(&bytes)
    }

    fn transport(&self) -> Option<Transport> {
        self.storage.as_ref().map(|s| s.transport)
    }

    /// Moves the transport to `to` only if it still is `from`: a Port Status
    /// Change seen during a recovery step takes precedence over its result.
    fn transition(&mut self, from: Transport, to: Transport) -> bool {
        match self.storage.as_mut() {
            Some(st) if st.transport == from => {
                st.transport = to;
                true
            }
            _ => false,
        }
    }

    fn bulk_mut(&mut self, dir_in: bool) -> Option<&mut Bulk> {
        let st = self.storage.as_mut()?;
        Some(if dir_in {
            &mut st.bulk_in
        } else {
            &mut st.bulk_out
        })
    }

    /// Transfer ring of endpoint `dci` of device `slot_index` (EP0, a
    /// storage bulk endpoint or a HID interrupt endpoint).
    fn ring_mut(&mut self, slot_index: usize, dci: u8) -> Option<&mut Ring> {
        let dev = self.devices[slot_index].as_mut()?;
        if dci == 1 {
            return Some(&mut dev.mem.ep0);
        }
        if let Some(st) = self
            .storage
            .as_mut()
            .filter(|s| s.device == Some(slot_index))
        {
            if st.bulk_in.dci == dci {
                return Some(&mut st.bulk_in.ring);
            }
            if st.bulk_out.dci == dci {
                return Some(&mut st.bulk_out.ring);
            }
        }
        dev.endpoints
            .iter_mut()
            .flatten()
            .find(|e| e.dci == dci)
            .map(|e| &mut e.ring)
    }

    /// Set TR Dequeue Pointer (xHCI 4.6.10) to the software enqueue
    /// pointer: everything the controller had not finished is skipped.
    fn set_dequeue(&mut self, slot_index: usize, slot: u8, dci: u8, deadline: u64) -> bool {
        let Some((pointer, cycle)) = self.ring_mut(slot_index, dci).map(|r| r.enqueue_pointer())
        else {
            return false;
        };
        self.command_checked(
            Trb::set_tr_dequeue(slot, dci, pointer, cycle),
            b"Set TR Dequeue",
            deadline,
        )
    }

    /// Puts endpoint `dci` of device `slot_index` back in service after a
    /// transfer ended with `code` (`completion::NO_EVENT`: timeout). The
    /// plan follows the Endpoint Context state (xHCI 4.8.3): Reset Endpoint
    /// only when Halted (4.6.8; Context State Error otherwise), Stop
    /// Endpoint when a TD is still Running (4.6.9), Set TR Dequeue past the
    /// abandoned TD (4.6.10) only once the endpoint left Halted, and, on a
    /// bulk endpoint, `CLEAR_FEATURE(ENDPOINT_HALT)` for every halting code
    /// (USB 2.0 9.4.5: it is what restarts the device's data toggle at
    /// DATA0 together with the host's; EP0 is exempt, 8.5.3.4). Every
    /// command is bounded by `deadline`. Returns whether the endpoint
    /// accepts doorbells again.
    fn recover_endpoint(&mut self, slot_index: usize, dci: u8, code: u8, deadline: u64) -> bool {
        let Some((slot, plan)) = self.recovery_plan(slot_index, dci, code) else {
            return false;
        };
        let Some(mut ok) = self.host_recovery(slot_index, slot, dci, plan, deadline) else {
            return false;
        };
        if plan == EndpointRecovery::ResetHalted {
            let address = self
                .storage
                .as_ref()
                .filter(|st| st.device == Some(slot_index))
                .and_then(|st| {
                    [&st.bulk_in, &st.bulk_out]
                        .into_iter()
                        .find(|b| b.dci == dci)
                        .map(|b| b.address)
                });
            if let Some(address) = address {
                ok = self.clear_feature_halt(slot_index, address, deadline) && ok;
            }
        }
        ok
    }

    /// The slot of device `slot_index` and the recovery plan of its
    /// endpoint `dci` after a transfer ended with `code`, from the Endpoint
    /// Context state (xHCI 4.8.3), printed. `None` without a device entry.
    fn recovery_plan(
        &self,
        slot_index: usize,
        dci: u8,
        code: u8,
    ) -> Option<(u8, EndpointRecovery)> {
        let slot = self.devices[slot_index].as_ref()?.slot;
        let (state, dequeue) = self.ep_status(slot_index, dci)?;
        let plan = endpoint_recovery(code, state);
        sys::print(b"usb-input: recovering dci ");
        num(dci as u64);
        sys::print(b" (state ");
        num(state as u64);
        sys::print(b" ");
        sys::print(ep_state::name(state).as_bytes());
        sys::print(b", dequeue ");
        hex(dequeue);
        sys::print(b"): ");
        sys::print(plan.name().as_bytes());
        sys::print(b"\n");
        Some((slot, plan))
    }

    /// The controller side of `plan` for endpoint `dci` (the device side,
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` after a Reset Endpoint, is the
    /// caller's), every command bounded by `deadline`: Reset Endpoint
    /// with TSP = 0 only when Halted (4.6.8), Stop Endpoint when a TD is
    /// still Running (4.6.9), Set TR Dequeue past the abandoned TDs
    /// (4.6.10). `None` when Reset Endpoint failed: the endpoint is still
    /// Halted and Set TR Dequeue would fail too (4.6.10), so nothing else
    /// may be sent. Otherwise whether the endpoint accepts doorbells again.
    fn host_recovery(
        &mut self,
        slot_index: usize,
        slot: u8,
        dci: u8,
        plan: EndpointRecovery,
        deadline: u64,
    ) -> Option<bool> {
        Some(match plan {
            EndpointRecovery::Nothing => true,
            EndpointRecovery::StopAndRetire => {
                self.stop_endpoint(slot, dci, deadline)
                    && self.set_dequeue(slot_index, slot, dci, deadline)
            }
            EndpointRecovery::SetDequeue => self.set_dequeue(slot_index, slot, dci, deadline),
            EndpointRecovery::ResetHalted => {
                if !self.command_checked(
                    Trb::reset_endpoint(slot, dci, false),
                    b"Reset Endpoint",
                    deadline,
                ) {
                    return None;
                }
                self.set_dequeue(slot_index, slot, dci, deadline)
            }
            EndpointRecovery::Reenumerate => false,
        })
    }

    /// `CLEAR_FEATURE(ENDPOINT_HALT)` (USB 2.0 9.4.5): the device leaves
    /// the halt and restarts its data toggle at DATA0, as the host did
    /// with Reset Endpoint (TSP = 0). Bounded by `deadline` and its own
    /// timeout. The result is always printed; a failure is a fault event.
    fn clear_feature_halt(&mut self, slot_index: usize, address: u8, deadline: u64) -> bool {
        let result = self.control(
            slot_index,
            SetupPacket::clear_endpoint_halt(address),
            None,
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        );
        sys::print(b"usb-input: CLEAR_FEATURE(ENDPOINT_HALT) endpoint ");
        hex(address as u64);
        match result {
            Ok(_) => sys::print(b": ok\n"),
            Err(code) => {
                sys::print(b": failed, ");
                code_text(code);
                sys::print(b"\n");
                record("usb clrhalt fail", code as u64);
            }
        }
        result.is_ok()
    }

    /// Soft Retry (xHCI 4.6.8.1, controllers 1.1 and later): a USB
    /// Transaction Error halted the endpoint; Reset Endpoint with TSP = 1
    /// clears the halt and keeps the data toggle, and the doorbell makes
    /// the controller execute the same TD again. False when it does not
    /// apply or the command failed: the TSP = 0 path follows.
    fn soft_retry(
        &mut self,
        slot_index: usize,
        slot: u8,
        dci: u8,
        code: u8,
        deadline: u64,
    ) -> bool {
        let Some((state, _)) = self.ep_status(slot_index, dci) else {
            return false;
        };
        if !soft_retry_applies(self.version, code, state) {
            return false;
        }
        say(b"soft retry: Reset Endpoint with TSP = 1 and doorbell");
        self.command_checked(
            Trb::reset_endpoint(slot, dci, true),
            b"Reset Endpoint (TSP = 1)",
            deadline,
        )
    }

    /// Synchronous bulk transfer on the storage IN or OUT endpoint for
    /// `stage`, bounded by `timeout_ms` and the transfer window of
    /// `budget`. `Ok(bytes moved)` (`td_bytes_moved`: the events carry
    /// residuals, never this count), `Refused` when the window
    /// is over (nothing touched), or the completion code of the failure
    /// (`completion::NO_EVENT` on timeout). Only events whose TRB pointer
    /// lies in this TD count; others are printed and ignored. Every failure
    /// is printed with the controller's state before anything is recovered.
    fn bulk(
        &mut self,
        stage: Stage,
        dir_in: bool,
        phys: u64,
        len: u32,
        timeout_ms: u64,
        budget: Budget,
    ) -> Result<u32, BulkFail> {
        let start = self.clock.now();
        let (slot_index, slot, dci) = {
            let st = self
                .storage
                .as_ref()
                .ok_or(BulkFail::Failed(completion::NO_EVENT))?;
            let ep = if dir_in { &st.bulk_in } else { &st.bulk_out };
            (
                st.device.ok_or(BulkFail::Failed(completion::NO_EVENT))?,
                st.slot,
                ep.dci,
            )
        };
        let mut fault = TransferFault {
            stage,
            dir_in,
            dci,
            code: completion::USB_TRANSACTION_ERROR,
            event: None,
            start,
            injected: false,
        };
        if stage == Stage::Cbw && self.inject_now == Some(Injection::Cbw) {
            // Test injection: the CBW is reported as failed with a USB
            // Transaction Error without reaching the controller (the
            // endpoint stays Running: the recovery must see that and not
            // reset it).
            self.inject_now = None;
            fault.injected = true;
            self.report_transfer_failure(&fault, &[]);
            return Err(BulkFail::Failed(completion::USB_TRANSACTION_ERROR));
        }
        let Some(end) = budget.transfer(start, timeout_ms) else {
            sys::print(b"usb-input: ");
            sys::print(stage.name().as_bytes());
            sys::print(if dir_in { b" IN dci " } else { b" OUT dci " });
            num(dci as u64);
            sys::print(b" refused: the request's transfer window is over\n");
            return Err(BulkFail::Refused);
        };
        let (td, lens, n) = {
            let ep = self
                .bulk_mut(dir_in)
                .ok_or(BulkFail::Failed(completion::NO_EVENT))?;
            let mut trbs = [Trb::noop_command(); 4];
            let n = bulk_trbs(phys, len, ep.max_packet, dir_in, &mut trbs)
                .ok_or(BulkFail::Failed(completion::NO_EVENT))?;
            let mut td = [0u64; 4];
            let mut lens = [0u32; 4];
            for ((slot, len), t) in td.iter_mut().zip(lens.iter_mut()).zip(&trbs[..n]) {
                *slot = ep.ring.push(*t);
                *len = t.transfer_length();
            }
            (td, lens, n)
        };
        self.ring_doorbell(slot, dci as u32);
        let mut soft_retried = false;
        // The bytes moved, from the Success or Short Packet event that ends
        // the TD (`td_bytes_moved`). Only the last TRB has IOC and ISP
        // (`Trb::normal_part`), so the one event is the last TRB's and it
        // closes the transfer. Trap: were ISP ever set on earlier TRBs, a
        // mid-TD short packet would produce an event here first; the first
        // event decides and the last TRB's still closes, so the loop copes,
        // but "ISP on the last TRB only" and "the last TRB's event closes"
        // must be changed together.
        let mut moved = None;
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::TRANSFER_EVENT
                    && ev.slot_id() == slot
                    && ev.endpoint_id() == dci
                {
                    let Some(index) = td[..n].iter().position(|&t| t == ev.parameter) else {
                        self.stray_transfer(ev);
                        continue;
                    };
                    match ev.completion_code() {
                        completion::SUCCESS | completion::SHORT_PACKET => {
                            let done = *moved.get_or_insert_with(|| {
                                td_bytes_moved(&lens[..n], index, ev.residual())
                            });
                            if index == n - 1 {
                                return Ok(done);
                            }
                        }
                        code => {
                            fault.code = code;
                            fault.event = Some(ev);
                            self.report_transfer_failure(&fault, &td[..n]);
                            if !soft_retried
                                && self.soft_retry(slot_index, slot, dci, code, budget.deadline)
                            {
                                soft_retried = true;
                                moved = None;
                                self.ring_doorbell(slot, dci as u32);
                                continue;
                            }
                            return Err(BulkFail::Failed(code));
                        }
                    }
                }
                self.dispatch(ev);
            }
            if self.clock.now() >= end {
                fault.code = completion::NO_EVENT;
                self.report_transfer_failure(&fault, &td[..n]);
                return Err(BulkFail::Failed(completion::NO_EVENT));
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    /// The failure line of the decision table (incident 0001): stage,
    /// direction, DCI, completion code, event TRB pointer against the TD,
    /// residual, elapsed time, then the endpoint states, PORTSC and USBSTS.
    fn report_transfer_failure(&self, f: &TransferFault, td: &[u64]) {
        sys::print(b"usb-input: ");
        sys::print(f.stage.name().as_bytes());
        sys::print(if f.dir_in { b" IN dci " } else { b" OUT dci " });
        num(f.dci as u64);
        sys::print(b" failed: ");
        code_text(f.code);
        sys::print(b" after ");
        num(self.clock.now() - f.start);
        sys::print(b" ms");
        if f.injected {
            sys::print(b", injected");
        }
        match f.event {
            Some(ev) => {
                sys::print(b", residual ");
                num(ev.residual() as u64);
                sys::print(b", event trb ");
                hex(ev.parameter);
            }
            None => sys::print(b", no event"),
        }
        if let (Some(first), Some(last)) = (td.first(), td.last()) {
            sys::print(b", td ");
            hex(*first);
            sys::print(b"..");
            hex(*last);
        }
        sys::print(b"\n");
        self.dump_storage_state();
    }

    /// Endpoint states and dequeue pointers of EP0 and both bulk endpoints,
    /// PORTSC of the storage port decoded and USBSTS.
    fn dump_storage_state(&self) {
        let Some(st) = self.storage.as_ref() else {
            return;
        };
        sys::print(b"  ");
        st.identity.print(st.port);
        sys::print(b"\n  ");
        for d in [1u8, st.bulk_in.dci, st.bulk_out.dci] {
            sys::print(b"ep ");
            num(d as u64);
            match st.device.and_then(|device| self.ep_status(device, d)) {
                Some((state, dequeue)) => {
                    sys::print(b" ");
                    sys::print(ep_state::name(state).as_bytes());
                    sys::print(b" dequeue ");
                    hex(dequeue);
                }
                None => sys::print(b" (slot disabled)"),
            }
            sys::print(b"; ");
        }
        sys::print(b"\n");
        self.dump_port(st.port);
    }

    /// `  port N PORTSC ... decoded; USBSTS ... HSE= HCE=` (one line).
    fn dump_port(&self, port: u8) {
        let sc = self.portsc(port);
        let sts = self.op.r32(op::USBSTS);
        let mut text = [0u8; 96];
        sys::print(b"  port ");
        num(port as u64);
        sys::print(b" PORTSC ");
        hex(sc as u64);
        sys::print(b" ");
        sys::print(op::describe_portsc(sc, &mut text).as_bytes());
        sys::print(b"; USBSTS ");
        hex(sts as u64);
        sys::print(b" HSE=");
        num((sts & op::STS_HSE != 0) as u64);
        sys::print(b" HCE=");
        num((sts & op::STS_HCE != 0) as u64);
        sys::print(b"\n");
    }

    /// One SCSI command over the bulk-only transport: CBW, data (if any, as
    /// `(physical, bytes, towards the host)`) and CSW (BOT 5.3), within
    /// `budget`. No CBW goes out unless the transport is Ready (or being
    /// enumerated). Any failed transfer goes through `transfer_failure`; a
    /// CSW that does not close the command (BOT 6.3, 6.7) asks for a Reset
    /// Recovery. A passed command returns the bytes it did not move
    /// (`bytes_not_moved`; 0 when complete).
    fn scsi(
        &mut self,
        cdb: Cdb,
        data: Option<(u64, u32, bool)>,
        budget: Budget,
    ) -> Result<u32, ScsiFail> {
        let (tag, cbw_phys, cbw_virt) = {
            let st = self.storage.as_mut().ok_or(ScsiFail::NotReady)?;
            if !matches!(st.transport, Transport::Ready | Transport::Enumerating) {
                return Err(ScsiFail::NotReady);
            }
            st.tag = st.tag.wrapping_add(1);
            (st.tag, st.cbw.0, st.cbw.1)
        };
        if let Some(kind @ (Injection::Reenum | Injection::Unreleased)) = self.inject_now {
            // Test injection: the endpoint is given up as after a failed
            // retire; Disable Slot and the re-enumeration from the idle
            // loop follow (the request answers Io).
            self.inject_now = None;
            self.disable_unreleased = kind == Injection::Unreleased;
            say(b"injected fault: the endpoint is given up");
            self.give_up_endpoint(budget.deadline);
            return Err(ScsiFail::Transport(completion::NO_EVENT, None));
        }
        if self.inject_now == Some(Injection::Reset) {
            // Test injection: the device is treated as not answering, as
            // when it rejects the class reset (`send_class_reset`): the
            // request answers Io and the idle loop resets the port and
            // enumerates the device again on its own slot.
            self.inject_now = None;
            say(b"injected fault: the device is re-enumerated in place after a port reset");
            let now = self.clock.now();
            self.transition(
                Transport::Ready,
                Transport::Reenumerate {
                    not_before: now,
                    reset_port: true,
                },
            );
            return Err(ScsiFail::Transport(completion::NO_EVENT, None));
        }
        if self.inject_now == Some(Injection::Csw) {
            // Test injection: a CSW read with no command outstanding. The
            // device answers with a STALL on its bulk IN pipe, which takes
            // the CSW-stage recovery path (BOT 6.7.2) on a real halt.
            self.inject_now = None;
            say(b"injected fault: reading a CSW with no command outstanding");
            return Err(match self.read_csw(budget) {
                Ok(_) => {
                    say(b"the device delivered a CSW for no command");
                    self.request_reset_recovery();
                    ScsiFail::Csw
                }
                Err(fail) => self.transfer_failure(Stage::Csw, true, fail, tag, budget),
            });
        }
        let cbw = Cbw {
            tag,
            data_len: data.map_or(0, |d| d.1),
            data_in: data.is_some_and(|d| d.2),
            lun: 0,
            cdb,
        };
        // SAFETY: CBW in the driver's DMA buffer.
        unsafe { (cbw_virt as *mut [u8; Cbw::LEN]).write_volatile(cbw.encode()) };
        if let Err(fail) = self.bulk(
            Stage::Cbw,
            false,
            cbw_phys,
            Cbw::LEN as u32,
            CBW_TIMEOUT_MS,
            budget,
        ) {
            return Err(self.transfer_failure(Stage::Cbw, false, fail, tag, budget));
        }
        let mut moved = 0;
        if let Some((phys, len, dir_in)) = data {
            match self.bulk(Stage::Data, dir_in, phys, len, DATA_TIMEOUT_MS, budget) {
                Ok(n) => moved = n,
                Err(fail) => {
                    return Err(self.transfer_failure(Stage::Data, dir_in, fail, tag, budget))
                }
            }
            if self.inject_now == Some(Injection::ShortData) {
                // Test injection: the transport reports one sector less than
                // the command asked for; the command itself passed.
                self.inject_now = None;
                say(b"injected fault: the data stage is reported one sector (512 bytes) short");
                moved = moved.saturating_sub(512);
            }
        }
        let (csw_moved, raw) = match self.read_csw(budget) {
            Ok(read) => read,
            Err(fail) => return Err(self.transfer_failure(Stage::Csw, true, fail, tag, budget)),
        };
        match check_csw_transfer(csw_moved, &raw, tag) {
            // The bytes the command did not move: the CSW's residue (BOT
            // 6.7) or what the data stage fell short of, whichever is more;
            // the caller never takes either for a success (incident 0010).
            Ok(Csw {
                status: CswStatus::Passed,
                residue,
                ..
            }) => Ok(bytes_not_moved(residue, data.map_or(0, |d| d.1), moved)),
            Ok(_) => Err(ScsiFail::Failed),
            Err(e) => {
                // BOT 6.3, 6.5 and 6.7: an invalid CSW (a wrong length
                // included), one for another command or a phase error need a
                // Reset Recovery before any new CBW; with `strict` the caller
                // halts before it runs.
                self.report_csw_error(e, tag);
                self.request_reset_recovery();
                Err(ScsiFail::Csw)
            }
        }
    }

    /// Reads the CSW of the command in progress into the storage's CSW
    /// buffer within `budget`: the bytes the device moved and the buffer.
    /// The buffer is filled with 0xFF first (no valid bCSWStatus), so a
    /// previous command's status never survives a short reply; the caller
    /// still rejects any count but 13 (`check_csw_transfer`, BOT 5.2,
    /// incident 0007). `usb_fault=...:short-csw` (tests): the device is
    /// made to look as if it moved only the signature and the tag, over the
    /// zero residue and Passed status of a previous command.
    fn read_csw(&mut self, budget: Budget) -> Result<(usize, [u8; Csw::LEN]), BulkFail> {
        let (phys, virt) = self
            .storage
            .as_ref()
            .map(|st| st.csw)
            .ok_or(BulkFail::Failed(completion::NO_EVENT))?;
        // SAFETY: CSW in the driver's DMA buffer.
        unsafe { (virt as *mut [u8; Csw::LEN]).write_volatile([0xFF; Csw::LEN]) };
        let moved = self.bulk(
            Stage::Csw,
            true,
            phys,
            Csw::LEN as u32,
            CSW_TIMEOUT_MS,
            budget,
        )?;
        // SAFETY: as above, once the transfer completed.
        let mut raw = unsafe { (virt as *const [u8; Csw::LEN]).read_volatile() };
        if self.inject_now == Some(Injection::ShortCsw) {
            self.inject_now = None;
            say(b"injected fault: the CSW read moved 8 bytes; the previous command's status stays in the buffer");
            raw[8..].fill(0);
            return Ok((8, raw));
        }
        Ok((moved as usize, raw))
    }

    /// `CSW` failures are printed with what was received (BOT 6.3) and
    /// recorded (`usb csw invalid`, or `usb csw length` with the bytes moved
    /// as the code; ADR 0028).
    fn report_csw_error(&self, e: CswError, tag: u32) {
        let (name, code) = e.event();
        record(name, code);
        let mut text = [0u8; CSW_TEXT_LEN];
        say(e.describe(tag, &mut text).as_bytes());
    }

    /// A bulk transfer at `stage` failed. A transfer refused for lack of
    /// time touched nothing: after the CBW went out the command is
    /// outstanding and only a Reset Recovery closes it (BOT 5.3). Otherwise
    /// the endpoint that reported the failure is recovered (xHCI) within
    /// the budget's reserve; then, unless the device is being enumerated
    /// (the enumeration fails and counts the attempt), the transport
    /// follows the Bulk-Only rule for the stage (BOT 5.3, 6.6.1, 6.7): a
    /// CBW failure needs a Reset Recovery; a data or CSW failure is
    /// followed by one CSW read, which closes the command if it is valid,
    /// else by a Reset Recovery. The Reset Recovery itself runs from the
    /// idle loop. Returns the failure to report.
    fn transfer_failure(
        &mut self,
        stage: Stage,
        dir_in: bool,
        fail: BulkFail,
        tag: u32,
        budget: Budget,
    ) -> ScsiFail {
        let code = fail.code();
        let enumerating = self.transport() == Some(Transport::Enumerating);
        if fail == BulkFail::Refused {
            if stage != Stage::Cbw && !enumerating {
                self.request_reset_recovery();
            }
            return ScsiFail::Transport(code, None);
        }
        let Some((device, dci, in_dci)) = self.storage.as_ref().and_then(|st| {
            let ep = if dir_in { &st.bulk_in } else { &st.bulk_out };
            Some((st.device?, ep.dci, st.bulk_in.dci))
        }) else {
            return ScsiFail::Transport(code, None);
        };
        if !self.recover_endpoint(device, dci, code, budget.deadline) {
            self.give_up_endpoint(budget.deadline);
            return ScsiFail::Transport(code, None);
        }
        if enumerating {
            return ScsiFail::Transport(code, None);
        }
        let mut sense = None;
        match after_transfer_failure(stage) {
            Next::ResetRecovery => self.request_reset_recovery(),
            Next::ReadCsw => {
                match self.read_csw(budget) {
                    Ok((moved, raw)) => {
                        match check_csw_transfer(moved, &raw, tag) {
                            Ok(csw) => {
                                sys::print(b"usb-input: CSW received after the failure: status ");
                                num(csw.status as u64);
                                sys::print(b" residue ");
                                num(csw.residue as u64);
                                sys::print(b"; transport ready\n");
                                if csw.status == CswStatus::Failed {
                                    // The device closed the command as failed:
                                    // its reason (SPC-4 REQUEST SENSE) is asked
                                    // for now, within the budget, before
                                    // anything else touches the transport.
                                    sense = self.sense(budget);
                                    if sense.is_none() {
                                        say(b"REQUEST SENSE after the failed command gave no sense data");
                                    }
                                }
                            }
                            Err(e) => {
                                self.report_csw_error(e, tag);
                                self.request_reset_recovery();
                            }
                        }
                    }
                    Err(BulkFail::Refused) => self.request_reset_recovery(),
                    Err(BulkFail::Failed(csw_code)) => {
                        record("usb csw2 fail", csw_code as u64);
                        if self.recover_endpoint(device, in_dci, csw_code, budget.deadline) {
                            self.request_reset_recovery();
                        } else {
                            self.give_up_endpoint(budget.deadline);
                        }
                    }
                }
            }
        }
        ScsiFail::Transport(code, sense)
    }

    /// No command puts the endpoint back. Outside an enumeration (whose
    /// failure releases the slot itself) Disable Slot (xHCI 4.6.4) stops
    /// every transfer of the device at once, so a TD the controller still
    /// owns can never write into a client buffer released after a Disable
    /// Slot that released the slot. Until its completion says so the
    /// request's buffer is kept (`request_buffer`), and for good if the
    /// slot was not released (incident 0017). The
    /// device is then enumerated again after a port reset (the last resort).
    fn give_up_endpoint(&mut self, deadline: u64) {
        say(b"endpoint not recoverable by commands: disabling the slot, port reset and re-enumeration");
        record("usb ep lost", 0);
        if self.transport() == Some(Transport::Enumerating) {
            return;
        }
        // The slot the request's TDs are on: the storage device's, or the
        // one whose Disable Slot is already outstanding. With neither, the
        // slot was disabled earlier and that completion decided.
        let slot = self
            .storage
            .as_ref()
            .and_then(|st| st.device)
            .and_then(|d| self.devices[d].as_ref())
            .map(|dev| dev.slot)
            .or(self.pending_disable.map(|(_, s)| s))
            .or(self.slot_to_disable);
        if let Some(slot) = slot {
            self.request_buffer = BufferFate::Hold(slot);
        }
        self.disable_storage_slot(deadline);
        let now = self.clock.now();
        if let Some(st) = self.storage.as_mut() {
            if st.transport != Transport::Gone {
                st.transport = Transport::Reenumerate {
                    not_before: now,
                    reset_port: true,
                };
            }
        }
    }

    /// Disable Slot (xHCI 4.6.4) of the storage device, if it still has
    /// one (`None` otherwise): the device entry goes away now, whatever
    /// the command's fate (`issue_disable_slot` tracks a pending or refused
    /// one and decides when its memory may be reused).
    fn disable_storage_slot(&mut self, deadline: u64) -> Option<Issued> {
        let device = self.storage.as_mut().and_then(|st| st.device.take())?;
        let dev = self.devices[device].take()?;
        Some(self.issue_disable_slot(dev.slot, Some(dev.mem), deadline))
    }

    /// Reset Recovery (BOT 5.3.4) is due: the class reset and what follows
    /// run from the idle loop; requests answer `Io` meanwhile. A port
    /// change that already moved the transport elsewhere wins.
    fn request_reset_recovery(&mut self) {
        if self.transition(Transport::Ready, Transport::ResetPending) {
            say(b"reset recovery (BOT 5.3.4) pending: the class reset is sent when idle");
        }
    }

    /// Idle-loop maintenance: one step of the storage recovery in progress
    /// per iteration, bounded by `MAINTENANCE_STEP_MS` (class reset,
    /// GET_STATUS poll plus halt clearing) or `REENUMERATE_STEP_MS` (a
    /// whole re-enumeration), so that no block request ever waits for a
    /// class reset, a device that is re-initialising or a re-enumeration,
    /// and no request queues behind more than the longer bound.
    fn maintain_storage(&mut self) {
        let now = self.clock.now();
        let step = now + MAINTENANCE_STEP_MS;
        // A slot whose Disable Slot was refused for lack of time comes
        // first: until it is disabled a held client buffer stays held and
        // no re-enumeration starts.
        if let Some(slot) = self.slot_to_disable.take() {
            self.issue_disable_slot(slot, None, step);
            return;
        }
        let Some(before) = self.transport() else {
            return;
        };
        match before {
            Transport::Ready | Transport::Enumerating | Transport::Gone => {}
            Transport::ResetPending => self.send_class_reset(step),
            Transport::ResetSent {
                since,
                next_poll,
                failed_polls,
            } if now >= next_poll => self.poll_after_reset(before, since, failed_polls, step),
            Transport::ResetSent { .. } => {}
            Transport::Reenumerate {
                not_before,
                reset_port,
            } if now >= not_before => {
                self.reenumerate_storage(reset_port, now + REENUMERATE_STEP_MS)
            }
            Transport::Reenumerate { .. } => {}
        }
    }

    /// Reset Recovery step 1 (BOT 5.3.4, 3.1): the Bulk-Only Mass Storage
    /// Reset makes the device drop the command it holds.
    fn send_class_reset(&mut self, deadline: u64) {
        let Some((device, interface)) = self
            .storage
            .as_ref()
            .and_then(|s| Some((s.device?, s.interface)))
        else {
            self.give_up_endpoint(deadline);
            return;
        };
        say(b"reset recovery (BOT 5.3.4): Bulk-Only Mass Storage Reset");
        let now = self.clock.now();
        match self.control(
            device,
            SetupPacket::mass_storage_reset(interface),
            None,
            self.control_step(deadline, CONTROL_TIMEOUT_MS),
        ) {
            Ok(_) => {
                self.transition(
                    Transport::ResetPending,
                    Transport::ResetSent {
                        since: now,
                        next_poll: now,
                        failed_polls: 0,
                    },
                );
            }
            Err(code) => {
                sys::print(b"usb-input: the device did not accept the class reset (");
                code_text(code);
                sys::print(b"): port reset and re-enumeration\n");
                record("usb reset fail", code as u64);
                self.transition(
                    Transport::ResetPending,
                    Transport::Reenumerate {
                        not_before: self.clock.now(),
                        reset_port: true,
                    },
                );
            }
        }
    }

    /// Reset Recovery step 2: the device may be re-initialising after the
    /// class reset (the stick of incident 0001 reboots on it), so EP0 is
    /// polled with GET_STATUS every `RESET_POLL_MS` until it answers,
    /// `RESET_WAIT_MS` at most; then both bulk halts are cleared (steps 2
    /// and 3 of BOT 5.3.4), all within `deadline`.
    fn poll_after_reset(&mut self, before: Transport, since: u64, failed_polls: u8, deadline: u64) {
        let Some((device, small_phys)) = self
            .storage
            .as_ref()
            .and_then(|s| Some((s.device?, s.small.0)))
        else {
            self.give_up_endpoint(deadline);
            return;
        };
        let poll_start = self.clock.now();
        match self.control(
            device,
            SetupPacket::get_device_status(),
            Some((small_phys, 2)),
            self.control_step(deadline, RESET_POLL_MS),
        ) {
            Ok(_) => {
                sys::print(b"usb-input: the device answers GET_STATUS ");
                num(self.clock.now() - since);
                sys::print(b" ms after the class reset\n");
                if self.clear_bulk_halts(deadline) {
                    if self.transition(before, Transport::Ready) {
                        say(b"reset recovery done: transport ready");
                    }
                } else {
                    say(b"reset recovery failed: port reset and re-enumeration");
                    self.transition(
                        before,
                        Transport::Reenumerate {
                            not_before: self.clock.now(),
                            reset_port: true,
                        },
                    );
                }
            }
            Err(code) => {
                if failed_polls == 0 {
                    record("usb no answer", code as u64);
                }
                if poll_start >= since + RESET_WAIT_MS {
                    say(b"the device does not answer after the class reset: port reset and re-enumeration");
                    record("usb reset timeout", (poll_start - since) / 1000);
                    self.transition(
                        before,
                        Transport::Reenumerate {
                            not_before: self.clock.now(),
                            reset_port: true,
                        },
                    );
                } else {
                    self.transition(
                        before,
                        Transport::ResetSent {
                            since,
                            next_poll: poll_start + RESET_POLL_MS,
                            failed_polls: failed_polls.saturating_add(1),
                        },
                    );
                }
            }
        }
    }

    /// BOT 5.3.4 steps 2 and 3 with the host side first, so that each bulk
    /// endpoint ends with an empty ring and its data toggle at DATA0 on
    /// both sides: a Halted endpoint takes Reset Endpoint (xHCI 4.6.8) and
    /// Set TR Dequeue (4.6.10); a Running or Stopped one cannot take Reset
    /// Endpoint (Context State Error) and is stopped (4.6.9) and
    /// reconfigured with Drop + Add (4.6.6), which the 4.6.8 note gives as
    /// the way to reset its toggle. The ring is only emptied once the
    /// controller has stopped following it. Then `CLEAR_FEATURE(ENDPOINT_HALT)`
    /// resets the device side (USB 2.0 9.4.5). Each step gets the lesser of
    /// `deadline` and its own timeout and refuses to start when that is
    /// over; every result is printed.
    fn clear_bulk_halts(&mut self, deadline: u64) -> bool {
        for dir_in in [true, false] {
            let Some((device, slot, dci, address)) = self.storage.as_ref().and_then(|st| {
                let ep = if dir_in { &st.bulk_in } else { &st.bulk_out };
                Some((st.device?, st.slot, ep.dci, ep.address))
            }) else {
                return false;
            };
            let Some((state, _)) = self.ep_status(device, dci) else {
                return false;
            };
            sys::print(b"usb-input: clearing the halt of dci ");
            num(dci as u64);
            sys::print(b" (state ");
            sys::print(ep_state::name(state).as_bytes());
            sys::print(b")\n");
            let host_ok = match state {
                ep_state::HALTED => {
                    self.command_checked(
                        Trb::reset_endpoint(slot, dci, false),
                        b"Reset Endpoint",
                        deadline,
                    ) && {
                        self.reset_ring(dir_in);
                        self.set_dequeue(device, slot, dci, deadline)
                    }
                }
                ep_state::RUNNING | ep_state::STOPPED => {
                    (state != ep_state::RUNNING || self.stop_endpoint(slot, dci, deadline)) && {
                        self.reset_ring(dir_in);
                        self.reconfigure_bulk(dir_in, deadline)
                    }
                }
                _ => false,
            };
            if !host_ok || !self.clear_feature_halt(device, address, deadline) {
                return false;
            }
        }
        true
    }

    fn reset_ring(&mut self, dir_in: bool) {
        if let Some(ep) = self.bulk_mut(dir_in) {
            ep.ring.reset();
        }
    }

    /// Configure Endpoint with Drop + Add of one bulk endpoint on its
    /// emptied ring (xHCI 4.6.6): the controller forgets its state and
    /// data toggle and starts the ring from the beginning with DCS = 1.
    fn reconfigure_bulk(&mut self, dir_in: bool, deadline: u64) -> bool {
        let Some((slot, dci, ctx, (in_phys, in_virt, in_len), slot_ctx)) =
            self.storage.as_ref().and_then(|st| {
                let ep = if dir_in { &st.bulk_in } else { &st.bulk_out };
                let dev = self.devices[st.device?].as_ref()?;
                Some((
                    dev.slot,
                    ep.dci,
                    EndpointContext::bulk(ep.max_packet, ep.ring.state.phys, dir_in),
                    dev.mem.input_ctx,
                    dev.slot_ctx,
                ))
            })
        else {
            return false;
        };
        write_input_context(
            // SAFETY: the device input context inside the DMA buffer.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            self.params.context_bytes(),
            1 << dci,
            1 | 1 << dci,
            &slot_ctx,
            &[(dci, ctx)],
        );
        self.command_checked(
            Trb::configure_endpoint(in_phys, slot),
            b"Configure Endpoint (drop + add)",
            deadline,
        )
    }

    /// The storage device is enumerated again on its port (xHCI 4.3)
    /// within `deadline`, the port reset first if `reset_port` (the last
    /// resort for a device that stopped answering). A device still on the
    /// port keeps its slot: Reset Device (4.6.11) and Address Device on the
    /// same Output Device Context, EP0 ring and Input Context, its bulk
    /// rings and buffers emptied and reused (`reenumerate_in_place`). The
    /// slot is disabled (4.6.4) instead when the device is gone, a new
    /// connection replaced it, the attempts are spent, or a previous
    /// attempt already released it (a failed Reset Device or addressing);
    /// a new slot then reuses the released memory. Either way the arena
    /// does not grow. It is served again only if it is the same medium
    /// (same capacity; a device that never answered READ CAPACITY has none
    /// to compare). Consecutive attempts are counted across recoveries
    /// (`Storage::reenumerations`), a run out of time counts too, and the
    /// storage is given up after `REENUMERATE_ATTEMPTS`.
    fn reenumerate_storage(&mut self, reset_port: bool, deadline: u64) {
        if self.pending_disable.is_some() {
            // The old slot is still being disabled: no new slot and no
            // attempt counted until its completion is seen; looked at again
            // after RESET_POLL_MS.
            say(b"re-enumeration waits for the pending Disable Slot");
            let not_before = self.after(RESET_POLL_MS);
            if let Some(st) = self.storage.as_mut() {
                st.transport = Transport::Reenumerate {
                    not_before,
                    reset_port,
                };
            }
            return;
        }
        if let Some(st) = self.storage.as_mut() {
            st.reenumerations = st.reenumerations.saturating_add(1);
        }
        let Some((port, attempt, reconnected)) = self
            .storage
            .as_ref()
            .map(|s| (s.port, s.reenumerations, s.reconnected))
        else {
            return;
        };
        sys::print(b"usb-input: re-enumerating the storage device on port ");
        num(port as u64);
        if reset_port {
            sys::print(b" after a port reset");
        }
        sys::print(b" (attempt ");
        num(attempt as u64);
        sys::print(b")\n");
        record("usb reenum", attempt as u64);
        let present = self.portsc(port) & op::PORTSC_CCS != 0;
        if attempt > REENUMERATE_ATTEMPTS || !present || reconnected {
            // The device the slot addressed is gone or given up (xHCI
            // 4.6.4, 4.19.2): its slot is released.
            self.disable_storage_slot(deadline);
            if self.pending_disable.is_some() || self.slot_to_disable.is_some() {
                // Disable Slot not completed or refused: it is finished by
                // the idle loop before the next attempt; this one is not
                // counted.
                if let Some(st) = self.storage.as_mut() {
                    st.reenumerations -= 1;
                }
                say(b"re-enumeration postponed until the old slot is disabled");
                return;
            }
        }
        let Some(mut old) = self.storage.take() else {
            return;
        };
        if attempt > REENUMERATE_ATTEMPTS {
            say(b"storage given up until the port sees a new connection");
            record("usb given up", attempt as u64);
            old.transport = Transport::Gone;
            self.storage = Some(old);
            return;
        }
        if !present {
            say(b"no device on the storage port: requests fail until it is connected again");
            old.transport = Transport::Gone;
            self.storage = Some(old);
            return;
        }
        // From here on the old storage owns no slot: on success the new one
        // does, on failure the slot was released.
        let device = old.device.take();
        old.reconnected = false;
        self.spare_storage = Some(StorageMemory::of(&old));
        let result = match device {
            Some(index) => self.reenumerate_in_place(
                index,
                port,
                reset_port,
                REENUMERATE_READY_WAIT_MS,
                deadline,
            ),
            None => self.enumerate_port(port, reset_port, REENUMERATE_READY_WAIT_MS, deadline),
        };
        // Not taken when the enumeration failed before the storage was
        // configured: the memory stays with `old`.
        self.spare_storage = None;
        match result {
            Ok(()) => match self.storage.as_mut() {
                Some(st) if old.capacity == 0 || st.capacity == old.capacity => {
                    st.sync_cache = old.sync_cache;
                    st.errors_reported = old.errors_reported;
                    st.reenumerations = old.reenumerations;
                    if st.transport == Transport::Ready {
                        say(b"storage device back in service");
                        record("usb reenum ok", attempt as u64);
                    } else {
                        say(b"storage device enumerated, but its port changed again");
                        record("usb reenum fail", attempt as u64);
                    }
                }
                Some(st) => {
                    sys::print(b"usb-input: a different medium (");
                    num(st.capacity);
                    sys::print(b" sectors, was ");
                    num(old.capacity);
                    sys::print(b"): not served\n");
                    st.transport = Transport::Gone;
                    record("usb reenum fail", attempt as u64);
                }
                None => {
                    say(b"the device on the storage port is not a storage device any more");
                    record("usb reenum fail", attempt as u64);
                    old.transport = Transport::Gone;
                    self.storage = Some(old);
                }
            },
            Err(why) => {
                sys::print(b"usb-input: re-enumeration failed: ");
                sys::print(why.as_bytes());
                sys::print(b"\n");
                record("usb reenum fail", attempt as u64);
                old.transport = Transport::Reenumerate {
                    not_before: self.after(1000),
                    reset_port: true,
                };
                self.storage = Some(old);
            }
        }
        self.report_arena();
    }

    /// `DMA arena: N of M bytes used, D more than after the boot
    /// enumeration`: the evidence that re-enumerations reuse their memory.
    fn report_arena(&self) {
        sys::print(b"usb-input: DMA arena: ");
        num(self.arena.next);
        sys::print(b" of ");
        num(self.arena.region.len);
        sys::print(b" bytes used, ");
        num(self.arena.next.saturating_sub(self.arena_after_boot));
        sys::print(b" more than after the boot enumeration\n");
    }

    /// `REQUEST SENSE` after a rejected command; a transport failure here
    /// is recorded (`usb sense fail`).
    fn sense(&mut self, budget: Budget) -> Option<scsi::Sense> {
        let (phys, virt) = self.storage.as_ref()?.small;
        if let Err(fail) = self.scsi(
            scsi::request_sense(),
            Some((phys, scsi::SENSE_LEN as u32, true)),
            budget,
        ) {
            record("usb sense fail", fail.code() as u64);
            return None;
        }
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        scsi::Sense::parse(unsafe { core::slice::from_raw_parts(virt as *const u8, 64) })
    }

    /// `strict` (ADR 0028): dump what a block request's failure (`why`: a
    /// transport failure, a short data stage) looked like and stop; init
    /// does not restart the driver, the state stays inspectable. The xHCI
    /// endpoint recovery has run; the BOT Reset Recovery has not.
    fn halt(
        &mut self,
        why: &[u8],
        request: HaltedRequest,
        code: u8,
        attempt: usize,
        sense: Option<scsi::Sense>,
    ) -> ! {
        sys::print(b"usb-input: strict: halting on a ");
        sys::print(why);
        sys::print(b"\n");
        sys::print(b"  command   ");
        sys::print(if request.write {
            b"WRITE(10)"
        } else {
            b"READ(10)"
        });
        sys::print(b" lba ");
        num(request.lba);
        sys::print(b" count ");
        num(request.count as u64);
        sys::print(b"\n  completion code ");
        num(code as u64);
        sys::print(b" attempt ");
        num(attempt as u64);
        sys::print(b"\n  device sense ");
        match sense {
            Some(sense) => print_sense(&sense),
            None => sys::print(b"not available (no CSW with Command Failed, or no sense data)"),
        }
        if let Some(st) = self.storage.as_ref() {
            sys::print(b"\n  slot ");
            num(st.slot as u64);
            sys::print(b" bulk in dci ");
            num(st.bulk_in.dci as u64);
            sys::print(b" enqueue ");
            num(st.bulk_in.ring.state.enqueue as u64);
            sys::print(b" cycle ");
            num(st.bulk_in.ring.state.cycle as u64);
            sys::print(b"; bulk out dci ");
            num(st.bulk_out.dci as u64);
            sys::print(b" enqueue ");
            num(st.bulk_out.ring.state.enqueue as u64);
            sys::print(b" cycle ");
            num(st.bulk_out.ring.state.cycle as u64);
            sys::print(b"; tag ");
            num(st.tag as u64);
            sys::print(b"; transport ");
            sys::print(st.transport.name());
        }
        sys::print(b"\n");
        self.dump_storage_state();
        sys::process_exit(exit::TRANSPORT_FAULT)
    }

    /// Reads or writes `count` sectors in the client's DMA buffer
    /// (`buffer` is its handle) within `REQUEST_BUDGET_MS`. A transport
    /// that is not Ready answers `Io` at once. A rejection due to unit
    /// attention is retried once (SPC-4 4.5.6). A transport failure is
    /// recorded, its endpoint recovered, and the command retried once only
    /// if the transport is still Ready (the CSW closed the failed command)
    /// with transfer window left; nothing is ever resent blindly (BOT 5.3).
    /// If a TD could not be retired the buffer is kept until the slot is
    /// disabled (`request_buffer`, settled by `serve_block`).
    fn block_io(&mut self, write: bool, lba: u64, count: u16, phys: u64) -> bool {
        let Ok(lba32) = u32::try_from(lba) else {
            return false;
        };
        let cdb = if write {
            scsi::write10(lba32, count)
        } else {
            scsi::read10(lba32, count)
        };
        let name: &[u8] = if write { b"WRITE(10)" } else { b"READ(10)" };
        let start = self.clock.now();
        let budget = Budget::new(start, REQUEST_BUDGET_MS, RECOVERY_RESERVE_MS);
        self.inject_now = self.fault_inject.due(start, self.last_request_ms, false);
        self.last_request_ms = start;
        if self.transport() != Some(Transport::Ready) {
            self.inject_now = None;
            self.report_unavailable(name, lba);
            return false;
        }
        let mut unit_attention_retried = false;
        let mut transport_failures = 0usize;
        loop {
            let injected = self.inject_now.is_some();
            match self.scsi(cdb, Some((phys, count as u32 * 512, !write)), budget) {
                Ok(0) => {
                    if transport_failures > 0 {
                        say(b"transfer recovered");
                    }
                    if let Some(st) = self.storage.as_mut() {
                        st.reenumerations = 0;
                    }
                    return true;
                }
                Ok(short) => {
                    // Part of the sectors were not moved (BOT 6.7.2, 6.7.3):
                    // a READ would hand the client stale bytes, a WRITE did
                    // not reach the medium whole. Never a success (incident
                    // 0010), and not a transport failure either: the
                    // transport is Ready, nothing to recover, the request
                    // answers Io. Recorded as the kind and as the request
                    // (`usb [inj] r|w lba N`, code = the bytes short).
                    sys::print(b"usb-input: ");
                    sys::print(name);
                    sys::print(b" lba ");
                    num(lba);
                    sys::print(b": ");
                    num(short as u64);
                    sys::print(
                        b" bytes not transferred: short data stage (BOT 6.7.2), the request answers Io\n",
                    );
                    record(fault::SHORT_DATA, short as u64);
                    record_lba(write, injected, lba, short as u64);
                    if self.strict {
                        self.halt(
                            b"short data stage",
                            HaltedRequest { write, lba, count },
                            completion::SUCCESS,
                            transport_failures,
                            None,
                        );
                    }
                    return false;
                }
                Err(ScsiFail::Failed) => {
                    let sense = self.sense(budget);
                    if let Some(sense) = sense {
                        record_sense(&sense);
                    }
                    if sense.map(|s| s.key) == Some(scsi::Sense::UNIT_ATTENTION)
                        && !unit_attention_retried
                    {
                        unit_attention_retried = true;
                        continue;
                    }
                    self.report_error(name, lba, sense);
                    return false;
                }
                Err(ScsiFail::NotReady) => {
                    self.report_unavailable(name, lba);
                    return false;
                }
                Err(fail) => {
                    let code = fail.code();
                    // Never hidden: every failed transfer is an event
                    // (ADR 0028), retried or not.
                    record_lba(write, injected, lba, code as u64);
                    let sense = match fail {
                        ScsiFail::Transport(_, sense) => sense,
                        _ => None,
                    };
                    if let Some(sense) = sense {
                        // The device's own verdict on the command whose
                        // transfer failed, always printed and recorded.
                        sys::print(b"usb-input: ");
                        sys::print(name);
                        sys::print(b" lba ");
                        num(lba);
                        sys::print(b": the device reports the command failed, sense ");
                        print_sense(&sense);
                        sys::print(b"\n");
                        record_sense(&sense);
                    }
                    if self.strict {
                        self.halt(
                            b"transport failure",
                            HaltedRequest { write, lba, count },
                            code,
                            transport_failures,
                            sense,
                        );
                    }
                    transport_failures += 1;
                    if transport_failures == 1
                        && self.transport() == Some(Transport::Ready)
                        && budget.fits_transfer(self.clock.now(), RETRY_MIN_MS)
                    {
                        say(b"retrying once after the recovery");
                        continue;
                    }
                    self.report_error(name, lba, None);
                    return false;
                }
            }
        }
    }

    /// Makes writes durable (`SYNCHRONIZE CACHE`). A device that does
    /// not implement the command has no cache to write back: it is noted
    /// once and the flush becomes immediate.
    fn flush(&mut self) -> bool {
        if !self.storage.as_ref().is_some_and(|s| s.sync_cache) {
            return self.storage.is_some();
        }
        if self.transport() != Some(Transport::Ready) {
            self.report_unavailable(b"SYNCHRONIZE CACHE", 0);
            return false;
        }
        let budget = Budget::new(self.clock.now(), REQUEST_BUDGET_MS, RECOVERY_RESERVE_MS);
        match self.scsi(scsi::synchronize_cache10(), None, budget) {
            Ok(_) => true,
            Err(ScsiFail::Failed) => {
                let sense = self.sense(budget);
                if let Some(sense) = sense {
                    record_sense(&sense);
                }
                if sense.is_some_and(|s| s.command_unsupported()) {
                    if let Some(st) = self.storage.as_mut() {
                        st.sync_cache = false;
                    }
                    say(b"the device does not support SYNCHRONIZE CACHE; flush is a no-op");
                    true
                } else {
                    self.report_error(b"SYNCHRONIZE CACHE", 0, sense);
                    false
                }
            }
            Err(ScsiFail::NotReady) => {
                self.report_unavailable(b"SYNCHRONIZE CACHE", 0);
                false
            }
            Err(fail) => {
                record("usb flush fail", fail.code() as u64);
                if let ScsiFail::Transport(_, Some(sense)) = fail {
                    record_sense(&sense);
                }
                self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                false
            }
        }
    }

    /// The first few reports of a failed command, with its sense data.
    fn report_error(&mut self, what: &[u8], lba: u64, sense: Option<scsi::Sense>) {
        if !self.may_report() {
            return;
        }
        sys::print(b"usb-input: ");
        sys::print(what);
        sys::print(b" lba ");
        num(lba);
        match sense {
            Some(s) => {
                sys::print(b" rejected: sense ");
                print_sense(&s);
                sys::print(b"\n");
            }
            None => sys::print(b" transport failure\n"),
        }
    }

    /// A request refused because the transport is not Ready (first few).
    fn report_unavailable(&mut self, what: &[u8], lba: u64) {
        let Some(transport) = self.transport() else {
            return;
        };
        if !self.may_report() {
            return;
        }
        sys::print(b"usb-input: ");
        sys::print(what);
        sys::print(b" lba ");
        num(lba);
        sys::print(b" refused: storage transport ");
        sys::print(transport.name());
        sys::print(b"\n");
    }

    fn may_report(&mut self) -> bool {
        let Some(st) = self.storage.as_mut() else {
            return false;
        };
        if st.errors_reported >= 4 {
            return false;
        }
        st.errors_reported += 1;
        true
    }

    fn report_interface(&self, hid: &HidInterface, port: u8) {
        sys::print(b"usb-input: port ");
        num(port as u64);
        sys::print(match hid.protocol {
            1 => b": HID keyboard (boot protocol, ",
            2 => b": HID mouse (boot protocol, ",
            _ => b": unknown HID interface (",
        });
        num(hid.max_packet_size as u64);
        sys::print(b" bytes per report, endpoint ");
        num(hid.endpoint as u64);
        sys::print(b")\n");
    }

    /// A Transfer Event from a HID interrupt endpoint. The event retires
    /// its report TRB (xHCI 4.10.1). A completed report (Success or Short
    /// Packet) is decoded into `pending` and its TRB queued again, unless
    /// the endpoint waits for a recovery, which queues every report again
    /// itself. Any other code is a failed transfer (4.10.2: STALL, USB
    /// Transaction Error, Babble and the like halt the endpoint): it is
    /// printed, recorded as a fault event (ADR 0028) and its recovery left
    /// to the idle loop (`maintain_hid`), because this runs inside other
    /// waits (commands, control and bulk transfers) that must not nest
    /// recovery commands. A Stopped code is the TD a Stop Endpoint of that
    /// recovery retired (4.6.9). False if no report TRB matches the event.
    fn handle_transfer_event(&mut self, ev: Trb) -> bool {
        let now = self.clock.now();
        let code = ev.completion_code();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        for dev in self.devices.iter_mut().flatten() {
            if dev.slot != ev.slot_id() {
                continue;
            }
            for (ep, decoder) in dev.endpoints.iter_mut().zip(dev.decoders.iter_mut()) {
                let Some(ep) = ep else { continue };
                if ep.dci != ev.endpoint_id() {
                    continue;
                }
                let Some(r) = ep.reports.iter_mut().find(|r| r.0 == ev.parameter) else {
                    continue;
                };
                r.0 = 0;
                if completion::is_stopped(code) {
                    sys::print(b"usb-input: HID slot ");
                    num(dev.slot as u64);
                    sys::print(b" dci ");
                    num(ep.dci as u64);
                    sys::print(b": TD stopped at trb ");
                    hex(ev.parameter);
                    sys::print(b", ");
                    code_text(code);
                    sys::print(b"\n");
                    return true;
                }
                let injected = completion::is_success(code)
                    && self
                        .fault_inject
                        .due(now, self.last_report_ms, true)
                        .is_some();
                if completion::is_success(code) {
                    self.last_report_ms = now;
                }
                if injected || !completion::is_success(code) {
                    let code = if injected {
                        completion::USB_TRANSACTION_ERROR
                    } else {
                        code
                    };
                    ep.failed(dev.slot, code, ev, injected, now);
                    return true;
                }
                let got = (ep.report_len as u32).saturating_sub(ev.residual()) as usize;
                if !ep.seen {
                    ep.seen = true;
                    sys::print(b"usb-input: first report from slot ");
                    num(dev.slot as u64);
                    sys::print(b" (");
                    num(got as u64);
                    sys::print(b" bytes, code ");
                    num(code as u64);
                    sys::print(b")\n");
                }
                // SAFETY: the driver's report buffer (64 bytes).
                let report = unsafe { core::slice::from_raw_parts(r.2 as *const u8, got.min(64)) };
                let n = match decoder {
                    Decoder::Keyboard(k) => k.feed(report, &mut out),
                    Decoder::Mouse(m) => m.feed(report, &mut out),
                    Decoder::None => 0,
                };
                for e in &out[..n] {
                    self.pending.push(*e, now);
                }
                ep.failures = 0;
                if ep.fault.is_none() {
                    r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
                    self.doorbells.w32(4 * dev.slot as u64, ep.dci as u32);
                }
                return true;
            }
        }
        false
    }

    /// Idle-loop recovery of one failed HID interrupt endpoint per
    /// iteration, within `MAINTENANCE_STEP_MS`. In `strict` mode the first
    /// failure halts the driver with a dump before anything is touched
    /// (ADR 0028). The plan follows the Endpoint Context state (xHCI
    /// 4.8.3, `endpoint_recovery`): Halted takes Reset Endpoint with TSP =
    /// 0 (4.6.8) and Set TR Dequeue (4.6.10), then
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` to the device (USB 2.0 9.4.5: the
    /// halt feature is mandatory on interrupt endpoints, and only that
    /// request restarts the device's data toggle at DATA0 with the host's);
    /// Stopped takes Set TR Dequeue; Running (an error that did not halt
    /// it) needs no command. When the plan moved the dequeue pointer every
    /// report TRB is queued again, otherwise only those retired. Attempts
    /// are bounded by `HID_RECOVERY_ATTEMPTS` without a good report in
    /// between; then the endpoint is given up loudly. Every command result
    /// is printed and a failure recorded (`command_checked`).
    /// When the idle-loop maintenance next has a step to take (ADR 0044):
    /// a refused Disable Slot or a due class reset at once, the next
    /// GET_STATUS poll after a reset, a re-enumeration's `not_before`, and
    /// each faulted HID endpoint's `not_before` (at once in strict mode,
    /// which halts on it). `None` with nothing to recover, so the loop
    /// sleeps until a request or an event.
    fn next_maintenance(&self) -> Option<u64> {
        let mut next = Deadline::NONE;
        if self.slot_to_disable.is_some() {
            next.include(0);
        }
        match self.transport() {
            Some(Transport::ResetPending) => next.include(0),
            Some(Transport::ResetSent { next_poll, .. }) => next.include(next_poll),
            Some(Transport::Reenumerate { not_before, .. }) => next.include(not_before),
            Some(Transport::Ready | Transport::Enumerating | Transport::Gone) | None => {}
        }
        for ep in self
            .devices
            .iter()
            .flatten()
            .flat_map(|d| d.endpoints.iter().flatten())
        {
            if let Some(f) = ep.fault {
                next.include(if self.strict { 0 } else { f.not_before });
            }
        }
        next.get()
    }

    fn maintain_hid(&mut self) {
        let now = self.clock.now();
        let Some((index, e, fault)) = self.devices.iter().enumerate().find_map(|(i, d)| {
            d.as_ref()?
                .endpoints
                .iter()
                .enumerate()
                .find_map(|(e, ep)| {
                    let f = ep.as_ref()?.fault?;
                    (self.strict || now >= f.not_before).then_some((i, e, f))
                })
        }) else {
            return;
        };
        if self.strict {
            self.halt_hid(index, e, fault);
        }
        let step = self.after(MAINTENANCE_STEP_MS);
        let Some((slot, dci, address, failures, faults)) =
            self.devices[index].as_ref().and_then(|d| {
                let ep = d.endpoints[e].as_ref()?;
                Some((d.slot, ep.dci, ep.address, ep.failures, ep.faults))
            })
        else {
            return;
        };
        if failures >= HID_RECOVERY_ATTEMPTS {
            sys::print(b"usb-input: HID endpoint dci ");
            num(dci as u64);
            sys::print(b" of slot ");
            num(slot as u64);
            sys::print(b" given up after ");
            num(failures as u64);
            sys::print(b" recovery attempts (");
            num(faults as u64);
            sys::print(b" failed transfers): no more reports from it in this boot\n");
            record("usb hid lost", dci as u64);
            if let Some(ep) = self.devices[index]
                .as_mut()
                .and_then(|d| d.endpoints[e].as_mut())
            {
                ep.fault = None;
                ep.given_up = true;
            }
            return;
        }
        if let Some(ep) = self.devices[index]
            .as_mut()
            .and_then(|d| d.endpoints[e].as_mut())
        {
            ep.failures += 1;
            // Failures seen before this attempt are covered by it: it reads
            // the endpoint state afresh. Only those during it count.
            ep.refault = false;
        }
        let mut fault = fault;
        if fault.injected {
            // Test injection: the report arrived, so the endpoint is still
            // Running. Stop Endpoint (4.6.9) retires the TD in flight and
            // leaves it Stopped, so that the recovery below works from a
            // state the controller wrote, not from the injected code. Once:
            // a later attempt starts from whatever state this left.
            self.stop_endpoint(slot, dci, step);
            fault.injected = false;
            self.set_hid_fault(index, e, Some(fault));
        }
        let Some((_, plan)) = self.recovery_plan(index, dci, fault.code) else {
            return;
        };
        let host_ok = match self.host_recovery(index, slot, dci, plan, step) {
            Some(ok) => {
                fault.clear_halt |= plan == EndpointRecovery::ResetHalted;
                ok
            }
            None => false,
        };
        if host_ok && fault.clear_halt && self.clear_feature_halt(index, address, step) {
            fault.clear_halt = false;
        }
        // Another transfer failed while this attempt ran (its commands and
        // CLEAR_FEATURE wait for events): the endpoint may be Halted again,
        // so it is not taken as recovered and nothing is queued on it; the
        // next attempt recovers it from its state.
        let refault = self.devices[index]
            .as_ref()
            .and_then(|d| d.endpoints[e].as_ref())
            .is_some_and(|ep| ep.refault);
        if !host_ok || fault.clear_halt || refault {
            sys::print(b"usb-input: HID endpoint dci ");
            num(dci as u64);
            sys::print(b" of slot ");
            num(slot as u64);
            sys::print(b": recovery attempt ");
            num(failures as u64 + 1);
            if refault {
                sys::print(b" overtaken by another failed transfer");
            } else {
                sys::print(b" failed");
            }
            sys::print(b"; tried again from the idle loop in ");
            let wait = HID_RETRY_MS * (failures as u64 + 1);
            num(wait);
            sys::print(b" ms\n");
            fault.not_before = self.clock.now() + wait;
            self.set_hid_fault(index, e, Some(fault));
            return;
        }
        self.set_hid_fault(index, e, None);
        self.requeue_reports(index, e, plan.abandons_ring());
        sys::print(b"usb-input: HID endpoint dci ");
        num(dci as u64);
        sys::print(b" of slot ");
        num(slot as u64);
        sys::print(b" recovered: reports queued again (");
        num(faults as u64);
        sys::print(b" failed transfers in this boot)\n");
    }

    /// A stray Transfer Event (no report TRB matches it) with an error code
    /// on a HID endpoint: the endpoint may have halted on a TD the driver
    /// no longer tracks, so it is handled like any failed report (printed
    /// and recorded once, as `usb hid err`, by `Endpoint::failed`, and
    /// marked for recovery unless given up; incident 0001 audit 5 L3).
    /// False when the event is not an error on a HID endpoint, for
    /// `stray_transfer` to print and record.
    fn stray_on_hid(&mut self, ev: Trb) -> bool {
        let code = ev.completion_code();
        if completion::is_success(code) || completion::is_stopped(code) {
            return false;
        }
        let now = self.clock.now();
        for dev in self.devices.iter_mut().flatten() {
            if dev.slot != ev.slot_id() {
                continue;
            }
            if let Some(ep) = dev
                .endpoints
                .iter_mut()
                .flatten()
                .find(|ep| ep.dci == ev.endpoint_id())
            {
                sys::print(b"usb-input: stray transfer event on a HID endpoint\n");
                ep.failed(dev.slot, code, ev, false, now);
                return true;
            }
        }
        false
    }

    fn set_hid_fault(&mut self, index: usize, e: usize, fault: Option<HidFault>) {
        if let Some(ep) = self.devices[index]
            .as_mut()
            .and_then(|d| d.endpoints[e].as_mut())
        {
            ep.fault = fault;
        }
    }

    /// Queues the report TRBs of endpoint `e` of `devices[index]` again:
    /// all of them when `all` (Set TR Dequeue skipped whatever was on the
    /// ring), otherwise those retired, and rings its doorbell.
    fn requeue_reports(&mut self, index: usize, e: usize, all: bool) {
        let Some(dev) = self.devices[index].as_mut() else {
            return;
        };
        let slot = dev.slot;
        let Some(ep) = dev.endpoints[e].as_mut() else {
            return;
        };
        for r in ep.reports.iter_mut() {
            if all || r.0 == 0 {
                r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
            }
        }
        let dci = ep.dci;
        self.ring_doorbell(slot, dci as u32);
    }

    /// `strict` (ADR 0028): dump what the HID transfer failure looked like
    /// and stop before any recovery; init does not restart the driver.
    fn halt_hid(&self, index: usize, e: usize, fault: HidFault) -> ! {
        sys::print(b"usb-input: strict: halting on a HID transfer failure\n");
        if let Some(dev) = self.devices[index].as_ref() {
            if let Some(ep) = dev.endpoints[e].as_ref() {
                sys::print(b"  slot ");
                num(dev.slot as u64);
                sys::print(b" dci ");
                num(ep.dci as u64);
                sys::print(b" endpoint ");
                hex(ep.address as u64);
                sys::print(b" ");
                code_text(fault.code);
                if fault.injected {
                    sys::print(b", injected");
                }
                sys::print(b" trb ");
                hex(fault.trb);
                sys::print(b"\n  ep ");
                num(ep.dci as u64);
                match self.ep_status(index, ep.dci) {
                    Some((state, dequeue)) => {
                        sys::print(b" ");
                        sys::print(ep_state::name(state).as_bytes());
                        sys::print(b" dequeue ");
                        hex(dequeue);
                    }
                    None => sys::print(b" (no context)"),
                }
                sys::print(b"; ring enqueue ");
                num(ep.ring.state.enqueue as u64);
                sys::print(b" cycle ");
                num(ep.ring.state.cycle as u64);
                sys::print(b"; reports on the ring ");
                num(ep.reports.iter().filter(|r| r.0 != 0).count() as u64);
                sys::print(b"\n");
            }
            self.dump_port(dev.slot_ctx.root_hub_port);
        }
        sys::process_exit(exit::TRANSPORT_FAULT)
    }

    /// Collects the ring events; reports go to `self.pending`.
    fn poll(&mut self) {
        // Acknowledge the interrupt of interrupter 0.
        let iman = self.runtime.r32(rt::IMAN);
        if iman & rt::IMAN_PENDING != 0 {
            self.runtime.w32(rt::IMAN, iman);
        }
        while let Some(ev) = self.next_event() {
            self.dispatch(ev);
        }
        // Clear Event Handler Busy even when no event was consumed here
        // (xHCI 4.17.2, 5.5.2.3.3; Linux's xhci_irq does the same): the
        // event whose interrupt woke this round may have been taken by an
        // earlier one (a request's own wait, or a round started by IPC)
        // before the controller set EHB for it, and while EHB stays 1 the
        // controller sends no more interrupts. The loop no longer polls
        // every 10 ms to heal that (ADR 0044).
        self.runtime.w64(rt::ERDP, self.events.dequeue_pointer());
    }
}

/// Controller handoff from the firmware (xHCI 4.22.1, USB Legacy Support
/// capability): the OS requests ownership, waits for the firmware to
/// release its own and turns off its SMIs. Without the capability (QEMU)
/// there is nothing to do.
fn bios_handoff(base: &Mmio, clock: &Clock) {
    const LEGACY_SUPPORT: u32 = 1;
    const BIOS_OWNED: u32 = 1 << 16;
    const OS_OWNED: u32 = 1 << 24;
    /// Reserved bits of the legacy control/status register that are preserved.
    const KEEP: u32 = 0x7 << 1 | 0xFF << 5 | 0x7 << 17;
    /// Status bits (writing 1 clears them).
    const EVENTS: u32 = 0x7 << 29;
    let mut off = ((base.r32(cap::HCCPARAMS1) >> 16) & 0xFFFF) as u64 * 4;
    for _ in 0..32 {
        if off == 0 {
            return;
        }
        let legsup = base.r32(off);
        if legsup & 0xFF == LEGACY_SUPPORT {
            if legsup & BIOS_OWNED != 0 {
                base.w32(off, legsup | OS_OWNED);
                let deadline = clock.now() + 1000;
                while base.r32(off) & BIOS_OWNED != 0 && clock.now() < deadline {
                    let _ = sys::irq_wait(IRQ, 5);
                }
                say(if base.r32(off) & BIOS_OWNED == 0 {
                    b"the firmware released the xHCI"
                } else {
                    b"the firmware does not release the xHCI; continuing"
                });
            }
            let ctl = base.r32(off + 4);
            base.w32(off + 4, (ctl & KEEP) | EVENTS);
            return;
        }
        let next = ((legsup >> 8) & 0xFF) as u64 * 4;
        if next == 0 {
            return;
        }
        off += next;
    }
}

/// Intel 7 series (Panther Point) and later: the shared USB 2 and 3
/// ports start out on the EHCI controllers and the xHCI only sees them if
/// the OS routes them (XUSB2PR and USB3_PSSEN registers in configuration
/// space, like `usb_enable_intel_xhci_ports` in Linux). Returns whether anything changed.
fn intel_route_ports(cfg: &Mmio) -> bool {
    const XUSB2PR: u64 = 0xD0;
    const USB2PRM: u64 = 0xD4;
    const USB3_PSSEN: u64 = 0xD8;
    const USB3PRM: u64 = 0xDC;
    if cfg.r32(0) & 0xFFFF != 0x8086 {
        return false;
    }
    let usb3 = cfg.r32(USB3PRM);
    let usb2 = cfg.r32(USB2PRM);
    if usb3 == 0 && usb2 == 0 {
        return false;
    }
    cfg.w32(USB3_PSSEN, usb3);
    cfg.w32(XUSB2PR, usb2);
    sys::print(b"usb-input: Intel ports routed to the xHCI (USB2 ");
    num(usb2 as u64);
    sys::print(b", USB3 ");
    num(usb3 as u64);
    sys::print(b")\n");
    true
}

/// One block protocol request on the USB storage.
fn serve_block(ctl: &mut Controller, msg: &[u8], handles: &[Handle]) {
    let reply_to = handles[0];
    ctl.request_buffer = BufferFate::Release;
    let capacity = ctl.storage.as_ref().map_or(0, |s| s.capacity);
    let invalid = BlockReply {
        status: BlockStatus::Invalid,
        value: 0,
    };
    let reply = match BlockRequest::decode(msg) {
        None => invalid,
        Some(_) if capacity == 0 => invalid,
        Some(BlockRequest {
            op: BlockOp::Info, ..
        }) => BlockReply {
            status: BlockStatus::Ok,
            value: capacity,
        },
        Some(BlockRequest {
            op: BlockOp::Flush, ..
        }) => BlockReply {
            status: if ctl.flush() {
                BlockStatus::Ok
            } else {
                BlockStatus::Io
            },
            value: 0,
        },
        Some(BlockRequest { op, count, lba }) => {
            let count = count.min(MAX_SECTORS);
            if handles.len() < 2 {
                BlockReply {
                    status: BlockStatus::BadHandles,
                    value: 0,
                }
            } else if count == 0
                || lba
                    .checked_add(count as u64)
                    .is_none_or(|end| end > capacity)
            {
                invalid
            } else {
                match map_client_buffer(&mut ctl.client_buffers, handles[1]) {
                    Some((phys, len)) if len >= count as u64 * 512 => {
                        if ctl.block_io(op == BlockOp::Write, lba, count, phys) {
                            BlockReply {
                                status: BlockStatus::Ok,
                                value: count as u64,
                            }
                        } else {
                            BlockReply {
                                status: BlockStatus::Io,
                                value: 0,
                            }
                        }
                    }
                    _ => BlockReply {
                        status: BlockStatus::BadHandles,
                        value: 0,
                    },
                }
            }
        }
    };
    // The buffer is released only now, and only if every TD on it has
    // completed, been retired with Stop Endpoint and Set TR Dequeue, or its
    // slot released; otherwise it is kept (incident 0017).
    if handles.len() >= 2 {
        ctl.settle_request_buffer(handles[1]);
    }
    if sys::ipc_send(reply_to, &reply.encode(), &[], 0).is_err() {
        // The client gave up waiting (its endpoint is closed): the work was
        // done for nobody, and that is a fault worth seeing in `diag`.
        say(b"block reply undelivered: the client stopped waiting");
        record("usb reply undelivered", reply.status as u64);
    }
    let _ = sys::handle_close(reply_to);
}

/// Bounded wait for `reg & mask` to equal `set`.
fn wait_bits(
    mmio: &Mmio,
    reg: u64,
    mask: u32,
    set: bool,
    clock: &Clock,
    timeout_ms: u64,
) -> Option<()> {
    let deadline = clock.now() + timeout_ms;
    loop {
        if (mmio.r32(reg) & mask != 0) == set {
            return Some(());
        }
        if clock.now() >= deadline {
            return None;
        }
        let _ = sys::irq_wait(IRQ, 2);
    }
}

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// Serves one request that arrived on endpoint `which` (0: the input
/// service, 1: the block service).
fn serve(ctl: &mut Controller, which: usize, msg: &[u8], handles: &[Handle], now: u64) {
    if handles.is_empty() {
        return;
    }
    if which == 1 {
        serve_block(ctl, msg, handles);
        return;
    }
    let reply_to = handles[0];
    match Request::decode(msg) {
        Some(Request { op: Op::Info, .. }) => {
            let reply = Reply::status(Status::Ok)
                .with_name(device_name(ctl))
                .unwrap_or(Reply::status(Status::Ok));
            answer(reply_to, reply);
        }
        Some(Request {
            op: Op::Read,
            timeout_ms,
        }) => {
            ctl.pending.queue.read(reply_to, timeout_ms, now);
            ctl.pending.report_drops();
            ctl.pending.tell_gone_reader();
        }
        None => answer(reply_to, Reply::status(Status::Invalid)),
    }
}

fn device_name(ctl: &Controller) -> &'static str {
    match (ctl.keyboards > 0, ctl.mice > 0) {
        (true, true) => "USB HID Keyboard+Mouse",
        (true, false) => "USB HID Keyboard",
        (false, true) => "USB HID Mouse",
        (false, false) => "USB HID (no devices)",
    }
}

/// Test fault injection (`usb_fault`, ADR 0028): `N` or `idle`, with an
/// optional kind: `cbw` (the default), `csw`, `short-csw`, `short-data`,
/// `reenum`, `unreleased`, `reset` or `hid` (`Injection`). `hid` counts HID reports, the
/// others block requests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultInject {
    None,
    /// Block requests (HID reports) left before the injected failure.
    Count(u32, Injection),
    /// The first block request (HID report) after two idle seconds (once).
    AfterIdle(Injection),
}

impl FaultInject {
    const IDLE_MS: u64 = 2000;

    fn parse(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::None;
        };
        let (when, stage) = value.split_once(':').unwrap_or((value, "cbw"));
        let stage = match stage {
            "cbw" => Injection::Cbw,
            "csw" => Injection::Csw,
            "short-csw" => Injection::ShortCsw,
            "short-data" => Injection::ShortData,
            "reenum" => Injection::Reenum,
            "unreleased" => Injection::Unreleased,
            "reset" => Injection::Reset,
            "hid" => Injection::Hid,
            _ => return Self::None,
        };
        match when {
            "idle" => Self::AfterIdle(stage),
            n => n.parse().map_or(Self::None, |n| Self::Count(n, stage)),
        }
    }

    /// The failure injected in this block request (`hid` false) or HID
    /// report (`hid` true), if any; advances the state only for an
    /// injection of that path. `last_ms` is the previous one's time.
    fn due(&mut self, now: u64, last_ms: u64, hid: bool) -> Option<Injection> {
        if let Self::Count(_, kind) | Self::AfterIdle(kind) = *self {
            if (kind == Injection::Hid) != hid {
                return None;
            }
        }
        match *self {
            Self::None => None,
            Self::Count(1, stage) => {
                *self = Self::None;
                Some(stage)
            }
            Self::Count(n, stage) => {
                *self = Self::Count(n - 1, stage);
                None
            }
            Self::AfterIdle(stage) if last_ms > 0 && now - last_ms >= Self::IDLE_MS => {
                *self = Self::None;
                Some(stage)
            }
            Self::AfterIdle(_) => None,
        }
    }
}

fn main(mask: u64) -> u64 {
    let clock = Clock(extra_handle(mask, 0));
    let mut args = [0u8; sys::CMDLINE_MAX];
    let (strict, fault_inject) = match sys::cmdline(clock.0, &mut args) {
        Ok(text) => {
            let c = aricode_cmdline::CmdLine::new(text);
            (
                c.has(aricode_cmdline::keys::STRICT),
                FaultInject::parse(c.get(aricode_cmdline::keys::USB_FAULT)),
            )
        }
        Err(_) => {
            say(b"cannot read the command line (too long or not text): boot flags ignored");
            record("usb cmdline unreadable", sys::CMDLINE_MAX as u64);
            (false, FaultInject::None)
        }
    };
    let mut ctl = match Controller::open(mask, clock) {
        Ok(mut c) => {
            c.strict = strict;
            c.fault_inject = fault_inject;
            if strict {
                say(b"strict mode: the first transport failure halts the driver");
            }
            c
        }
        Err(exit::NOT_XHCI) => {
            say(b"the controller is not xHCI; skipping");
            return 0;
        }
        Err(step) => {
            sys::print(b"usb-input: xHCI initialization failed: ");
            sys::print(match step {
                exit::MAP_BAR => b"capabilities or BAR mapping\n",
                exit::DMA => b"DMA memory (buffer too small)\n",
                exit::RESET_TIMEOUT => b"the controller does not complete the reset\n",
                exit::START_TIMEOUT => b"the controller does not start\n",
                _ => b"unknown step\n",
            });
            return step;
        }
    };
    sys::print(b"usb-input: xHCI ready (HCIVERSION ");
    hex4(ctl.version as u64);
    sys::print(b", ");
    num(ctl.params.max_ports as u64);
    sys::print(b" ports, ");
    num(ctl.params.max_slots as u64);
    sys::print(b" slots, contexts of ");
    num(ctl.params.context_bytes() as u64);
    sys::print(b" bytes, ");
    num(ctl.params.scratchpad_buffers as u64);
    sys::print(b" scratchpads)\n");
    // Give the ports time to see the device after the reset (more if
    // they have just switched controllers).
    let settle = ctl.after(if ctl.ports_switched { 500 } else { 100 });
    ctl.wait_until(settle);
    for port in 1..=ctl.params.max_ports {
        if ctl.portsc(port) & op::PORTSC_CCS == 0 {
            continue;
        }
        let deadline = ctl.after(BOOT_ENUMERATION_MS);
        match ctl.enumerate_port(port, false, BOOT_READY_WAIT_MS, deadline) {
            Ok(()) => {}
            Err(e) => {
                sys::print(b"usb-input: port ");
                num(port as u64);
                sys::print(b": ");
                sys::print(e.as_bytes());
                sys::print(b"\n");
            }
        }
    }
    sys::print(b"usb-input: device ready: ");
    sys::print(device_name(&ctl).as_bytes());
    sys::print(b"\n");
    ctl.arena_after_boot = ctl.arena.next;
    sys::print(b"usb-input: DMA arena: ");
    num(ctl.arena.next);
    sys::print(b" of ");
    num(ctl.arena.region.len);
    sys::print(b" bytes used after the boot enumeration\n");
    if ctl.storage.is_none() {
        say(b"no USB storage");
    }

    // Second extra (optional; init passes it, the kernel test bench
    // does not): block service endpoint (ADR 0023). Without it, that handle
    // number may be one created by the driver itself (DMA buffers), so
    // the object type is checked.
    let block_ep = extra_handle(mask, 1);
    let has_block = sys::object_kind(block_ep) == Ok(abi::kind::ENDPOINT);
    let endpoints: &[Handle] = if has_block {
        &[SERVICE, block_ep]
    } else {
        &[SERVICE]
    };
    let mut msg = [0u8; 32];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    // One wait for a request on either endpoint, an event (the
    // controller's interrupt) or the next deadline (ADR 0044).
    let mut members = wait::Members::new();
    for ep in endpoints {
        members.push(*ep);
    }
    members.push(IRQ);
    say(b"serving requests");
    loop {
        ctl.poll();
        ctl.maintain_storage();
        ctl.maintain_hid();
        let mut requests_left = true;
        for _ in 0..REQUESTS_PER_ROUND {
            let (which, n, nh) = match sys::ipc_receive_any(endpoints, &mut msg, &mut handles, 0) {
                Ok(r) => r,
                // Both endpoints closed, or one revoked (ADR 0030): nothing
                // will arrive again.
                Err(e) if e.endpoint_gone() => {
                    say(b"service endpoints closed; exiting");
                    sys::process_exit(9);
                }
                Err(_) => {
                    requests_left = false;
                    break;
                }
            };
            // The clock per request: a block request before this one may
            // have taken seconds, and a read's deadline counts from now.
            let now = ctl.clock.now();
            serve(&mut ctl, which, &msg[..n], &handles[..nh], now);
        }
        let now = ctl.clock.now();
        ctl.pending.queue.settle(now);
        ctl.pending.tell_gone_reader();
        let mut next = Deadline::NONE;
        next.include_opt(ctl.pending.queue.deadline());
        next.include_opt(
            ctl.next_maintenance()
                .map(|due| due.max(now + MAINTENANCE_PACE_MS)),
        );
        match wait::end_of_round(
            members.as_slice(),
            now,
            requests_left,
            next,
            &mut msg,
            &mut handles,
        ) {
            Ok(Woken::Message {
                index,
                len,
                handles: nh,
            }) => {
                // At the time it arrived: a read's deadline counts from
                // now, not from before the wait.
                let now = ctl.clock.now();
                serve(&mut ctl, index, &msg[..len], &handles[..nh], now);
            }
            Ok(Woken::Irq { .. } | Woken::Round) => {}
            Err(e) if e.endpoint_gone() => {
                say(b"service endpoints closed; exiting");
                sys::process_exit(9);
            }
            Err(e) => {
                aricode_rt::print_error(b"usb-input: end-of-round wait", e);
                sys::process_exit(EXIT_WAIT_FAILED);
            }
        }
    }
}

aricode_rt::entry!(main);
