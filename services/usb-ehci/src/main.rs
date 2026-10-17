// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! usb-ehci (ADR 0031): ring 3 driver for an EHCI 1.0 controller. Takes the
//! controller from the firmware (EHCI 5.1), resets and starts it (4.1),
//! enumerates the devices on its root ports and behind USB 2.0 hubs (the
//! rate matching hub Intel 6/7 series put behind each EHCI), and serves
//! boot HID keyboards and mice (periodic schedule, `InputServiceProtocol
//! v1`) and one bulk-only mass storage device (async schedule, block
//! protocol), like usbd does on the xHCI (ADR 0023).
//!
//! Contract with init (`start_driver`): `SERVICE`, cfg, hardware, the IRQ
//! if the function has one, BAR0, then `SYSTEM` (`READ`) and the block
//! service endpoint. Intel EHCI functions have neither MSI nor MSI-X, so
//! the driver never uses the IRQ: it polls the schedules between requests.
//!
//! Failures (ADR 0028): every failed transfer is printed with the qTD
//! token and recorded as a fault event; the controller's own three tries
//! per transaction (EHCI 4.15.1.1.1) are the only transaction retries; a
//! halted endpoint is recovered only as the specifications say
//! (`CLEAR_FEATURE(ENDPOINT_HALT)`, USB 2.0 9.4.5; Bulk-Only Reset
//! Recovery, BOT 5.3.4); with `strict` the first failure dumps the
//! controller state and stops the driver.
#![no_std]
#![no_main]

use aricode_ehci::{
    cap, decode_token, hub, interrupt_schedule, legacy, link, op, overlay_token, periodic_entry,
    portsc, set_address, split_transfer, token, tt_for, Outcome, Params, Pid, Qtd, QueueHead,
    Speed, TransferError, Tt, FRAME_LIST_LEN, HCI_VERSION_1_0,
};
use aricode_input::{Event, HidBootKeyboard, HidBootMouse, MAX_REPORT_EVENTS};
use aricode_proto::block::{
    Op as BlockOp, Reply as BlockReply, Request as BlockRequest, Status as BlockStatus, MAX_SECTORS,
};
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS};
use aricode_rt::sys::block::{map_client_buffer, REPLY_WAIT_MS};
use aricode_rt::sys::virtio_pci::{extra_handle, polled_bar_handle, DmaRegion, Mmio, CFG, SERVICE};
use aricode_rt::sys::{self, abi, abi::driver_arg, Handle};
use aricode_usb_storage::{
    after_transfer_failure, check_csw, fault, scsi, Cbw, Cdb, Csw, CswError, CswStatus, Next, Stage,
};
use aricode_xhci::{
    parse_configuration, parse_storage_interface, request, DeviceDescriptor, HidInterface,
    SetupPacket, StorageInterface,
};
use core::sync::atomic::{fence, Ordering};

const MAX_DEVICES: usize = 8;
const MAX_PIPES: usize = 24;
const MAX_HIDS: usize = 8;
const MAX_PENDING: usize = 32;
/// qTDs per queue head, plus one inactive qTD that stops the queue after a
/// short packet (`OnShort::Stop`).
const QTDS_PER_PIPE: usize = 4;
const ARENA_BYTES: u64 = 32 * 1024;
const CONTROL_BUFFER: u16 = 512;
const RING: usize = 128;

/// Main loop period: how long the driver waits for a request before it
/// looks at the schedules again.
const POLL_MS: u64 = 8;
/// While a transfer runs, the first milliseconds are polled with yields,
/// then with 1 ms sleeps.
const SPIN_MS: u64 = 2;
/// A control transfer completes within 5 s (USB 2.0 9.2.6.4).
const CONTROL_TIMEOUT_MS: u64 = 5000;
/// Bulk-Only command timeouts, as in usbd (ADR 0023, incident 0001): the
/// CBW 1 s; the DATA and CSW stages of a medium access (READ, WRITE,
/// SYNCHRONIZE CACHE) one 30 s window (Linux `sd`), those of a command
/// that touches no medium (INQUIRY, TEST UNIT READY, READ CAPACITY,
/// REQUEST SENSE) 5 s; at least 1 s for the CSW read after a failure
/// (BOT 6.7.2).
const CBW_TIMEOUT_MS: u64 = 1000;
const MEDIUM_WINDOW_MS: u64 = 30_000;
const NO_MEDIUM_WINDOW_MS: u64 = 5000;
const MIN_STAGE_MS: u64 = 1000;
/// Hard bounds of the driver's units of work. Every transfer inside one
/// (recovery, REQUEST SENSE, hub GetPortStatus, ClearTTBuffer included)
/// is cut to its deadline, and one that cannot start in time is refused
/// without touching the controller (`Fail::Refused`); deadlines nest by
/// their minimum (`with_deadline`). A block request gets the CBW, the 30 s
/// medium window and 5 s of recovery; recovery that does not fit is
/// postponed (a Reset Recovery runs before the next CBW, BOT 5.3.4). One
/// main loop iteration's housekeeping (HID reports, port changes, the hub
/// poll and at most one enumeration) shares one `HOUSEKEEPING_MS`
/// deadline and runs between batches of requests; the requests of one
/// batch are served back to back. A block client waits `REPLY_WAIT_MS`
/// (90 s) from its send; with at most two block clients (fsd and one of
/// init's checks: one request queued behind the one in progress) its wait
/// is bounded by one housekeeping round plus two requests: 15 + 36 + 36 =
/// 87 s.
const REQUEST_BUDGET_MS: u64 = CBW_TIMEOUT_MS + MEDIUM_WINDOW_MS + 5000;
const HOUSEKEEPING_MS: u64 = 15_000;
const ATTACH_STEP_MS: u64 = HOUSEKEEPING_MS;
const HUB_POLL_STEP_MS: u64 = 5000;
const _: () = assert!(HOUSEKEEPING_MS + 2 * REQUEST_BUDGET_MS < REPLY_WAIT_MS);
const _: () = assert!(HUB_POLL_STEP_MS <= HOUSEKEEPING_MS);
/// The input service's Info waits for the boot enumeration at most this
/// long from the process start. init's readiness probe (init
/// `probe_ready`) gives a driver about 3 s from its launch; held Info
/// requests are answered from inside any wait once this passes, so a slow
/// enumeration step cannot push the answer past the probe. The whole boot
/// enumeration is bounded: ports still pending after it are enumerated
/// later, as hot-plugged ones.
const INFO_WAIT_MS: u64 = 1500;
const BOOT_ENUMERATION_MS: u64 = 30_000;
/// Interrupt on Async Advance (EHCI 4.8.2): the controller sets it at its
/// next pass over the async schedule, well within this bound.
const IAA_TIMEOUT_MS: u64 = 100;
/// The controller halts within 16 micro-frames of Run/Stop = 0 (EHCI 2.3.1).
const HALT_TIMEOUT_MS: u64 = 20;
/// Bound of the host controller reset (HCRESET reads 0 when done, 2.3.1).
const HC_RESET_TIMEOUT_MS: u64 = 250;
/// Schedule status follows the enable bits within a few micro-frames (4.8.1).
const SCHEDULE_TIMEOUT_MS: u64 = 20;
/// Root port reset held 50 ms (USB 2.0 7.1.7.5, TDRSTR); the controller
/// ends it within 2 ms of the write (EHCI 2.3.9).
const ROOT_RESET_MS: u64 = 50;
const PORT_RESET_END_MS: u64 = 20;
/// Reset recovery before the first request (USB 2.0 7.1.7.5, TRSTRCY).
const RESET_RECOVERY_MS: u64 = 10;
/// Hub port reset: 10-20 ms (USB 2.0 11.5.1.5); bound of the wait for
/// C_PORT_RESET.
const HUB_RESET_TIMEOUT_MS: u64 = 500;
/// Connect debounce (USB 2.0 7.1.7.3).
const DEBOUNCE_MS: u64 = 100;
/// SetAddress recovery (USB 2.0 9.2.6.3).
const SET_ADDRESS_MS: u64 = 2;
/// Hub ports are polled with GetPortStatus (USB 2.0 11.24.2.7).
const HUB_POLL_MS: u64 = 1000;
/// How long TEST UNIT READY may answer "not ready" while a stick starts up,
/// and the pause between tries (SPC-4 4.5: NOT READY and UNIT ATTENTION
/// are transient).
const READY_WAIT_MS: u64 = 2000;
const NOT_READY_PAUSE_MS: u64 = 100;
/// After a queue head leaves the periodic schedule, two frames (16
/// micro-frames) pass before its memory is reused: the controller walks
/// the periodic list once per micro-frame (EHCI 4.6).
const PERIODIC_UNLINK_MICROFRAMES: u32 = 16;
const PERIODIC_UNLINK_TIMEOUT_MS: u64 = 20;
/// The firmware gets 1 s to release the controller (as Linux; EHCI 5.1
/// gives no bound).
const HANDOFF_TIMEOUT_MS: u64 = 1000;

fn say(msg: &[u8]) {
    sys::print(b"usb-ehci: ");
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

/// A fault event in the kernel ring (ADR 0028; `diag` lists it).
fn record(name: &str, code: u64) {
    let _ = sys::event_record(code, name);
}

/// `ehci <kind> <what>` as a fault event name (at most 24 bytes).
fn record_parts(kind: &str, what: &str, code: u64) {
    let mut name = [0u8; abi::sysinfo::EVENT_NAME_LEN];
    let mut len = 0;
    for part in ["ehci ", kind, " ", what] {
        let n = part.len().min(name.len() - len);
        name[len..len + n].copy_from_slice(&part.as_bytes()[..n]);
        len += n;
    }
    if let Ok(text) = core::str::from_utf8(&name[..len]) {
        record(text, code);
    }
}

/// The device's reason for a failed command as a fault event.
fn record_sense(sense: &scsi::Sense) {
    let mut name = [0u8; fault::NAME_LEN];
    if let Some(text) = fault::sense_name(sense, &mut name) {
        record(text, sense.key as u64);
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

fn w32(virt: u64, v: u32) {
    // SAFETY: `virt` is inside the driver's DMA arena or a mapped region.
    unsafe { (virt as *mut u32).write_volatile(v) }
}

fn r32(virt: u64) -> u32 {
    // SAFETY: same as above.
    unsafe { (virt as *const u32).read_volatile() }
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

/// Linear allocator inside the driver's DMA buffer.
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Schedule {
    Idle,
    Async,
    Periodic,
}

/// A queue head with its qTDs: one endpoint of one device.
#[derive(Clone, Copy)]
struct Pipe {
    qh: (u64, u64),
    qtds: (u64, u64),
    desc: QueueHead,
    schedule: Schedule,
    period: u16,
    in_use: bool,
    /// The controller may still reference it (an unlink was not
    /// confirmed): never reused.
    lost: bool,
}

/// Where a device is attached.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
    Root(u8),
    /// Port of the hub at device index `.0`.
    Hub(usize, u8),
}

#[derive(Clone, Copy)]
struct HubState {
    ports: u8,
    /// A hub request failed: its ports are no longer polled.
    failed: bool,
}

#[derive(Clone, Copy)]
struct Device {
    address: u8,
    speed: Speed,
    tt: Option<Tt>,
    parent: Parent,
    /// Root port and hub ports down to the device (`1.3`), `depth` of them.
    path: [u8; 7],
    depth: u8,
    ep0: usize,
    hub: Option<HubState>,
}

#[derive(Clone, Copy)]
enum Decoder {
    Keyboard(HidBootKeyboard),
    Mouse(HidBootMouse),
}

#[derive(Clone, Copy)]
struct Hid {
    device: usize,
    pipe: usize,
    endpoint: u8,
    decoder: Decoder,
    report: (u64, u64),
    len: u16,
    seen: bool,
    /// A STALL was cleared and no report has arrived since.
    stall_cleared: bool,
    /// The CLEAR_FEATURE(ENDPOINT_HALT) for a STALL is still owed (it was
    /// refused for lack of housekeeping time): retried on the next poll.
    clear_owed: bool,
    /// Not polled any more (a failure that the specification does not
    /// recover); the device works again after it is plugged in again.
    stopped: bool,
}

#[derive(Clone, Copy)]
struct Bulk {
    pipe: usize,
    /// USB endpoint address (bit 7 = IN), for `CLEAR_FEATURE`.
    address: u8,
}

struct Storage {
    device: usize,
    interface: u8,
    bulk_in: Bulk,
    bulk_out: Bulk,
    tag: u32,
    cbw: (u64, u64),
    csw: (u64, u64),
    small: (u64, u64),
    /// 512-byte sectors (0 until READ CAPACITY succeeded).
    capacity: u64,
    sync_cache: bool,
    /// The Reset Recovery failed: requests fail until the device is
    /// plugged in again.
    broken: bool,
    /// A Reset Recovery is owed (it did not fit in the request that needed
    /// it): it runs before the next CBW (BOT 5.3.4).
    needs_reset: bool,
    errors_reported: u8,
}

/// What the block service's clients were given: a device is served again
/// after an unplug only if it is the same: capacity, INQUIRY names, the
/// GPT disk GUID (LBA 1, UEFI 5.3.2) and the USB serial number string,
/// the last two when the device has them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Served {
    capacity: u64,
    vendor: [u8; 8],
    product: [u8; 16],
    disk_guid: Option<[u8; 16]>,
    serial: [u8; SERIAL_LEN],
}

/// Bytes of the serial number string descriptor kept for the identity.
const SERIAL_LEN: usize = 64;

/// How a transfer failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// The queue head halted; the token of the qTD that halted it.
    Error(TransferError, u32),
    /// Not done by the deadline: the queue head was taken out of the
    /// schedule (EHCI 4.8.2), its qTDs retired.
    Timeout,
    /// The buffer cannot be described by the qTDs (size, or above 4 GiB on
    /// a 32-bit controller).
    Buffer,
    /// The controller did not confirm it released the queue head.
    Lost,
    /// Not started: the deadline of the current unit of work had passed.
    /// Nothing reached the controller.
    Refused,
}

impl Fail {
    fn short(self) -> &'static str {
        match self {
            Self::Error(TransferError::Stall, _) => "stall",
            Self::Error(TransferError::Babble, _) => "babble",
            Self::Error(TransferError::DataBuffer, _) => "databuf",
            Self::Error(TransferError::Transaction, _) => "xact",
            Self::Error(TransferError::MissedMicroFrame, _) => "missed",
            Self::Timeout => "timeout",
            Self::Buffer => "buffer",
            Self::Lost => "lost",
            Self::Refused => "no time",
        }
    }

    fn code(self) -> u64 {
        match self {
            Self::Error(e, _) => e.code(),
            Self::Timeout => 6,
            Self::Buffer => 7,
            Self::Lost => 8,
            Self::Refused => 9,
        }
    }

    fn is_stall(self) -> bool {
        matches!(self, Self::Error(TransferError::Stall, _))
    }

    fn print(self) {
        match self {
            Self::Error(e, t) => {
                sys::print(e.name().as_bytes());
                sys::print(b" (token ");
                hex(t as u64);
                sys::print(b")");
            }
            Self::Timeout => sys::print(b"timeout"),
            Self::Buffer => sys::print(b"buffer not addressable by the qTDs"),
            Self::Lost => sys::print(b"the controller did not release the queue head"),
            Self::Refused => sys::print(b"not started: no time left in this unit of work"),
        }
    }
}

/// Why a SCSI command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScsiFail {
    /// A bulk transfer failed; with the device's sense when the CSW read
    /// after the failure said Command Failed.
    Transport(Fail, Option<scsi::Sense>),
    /// The CSW was invalid, for another command or a phase error.
    Csw,
    /// The device rejected the command (`REQUEST SENSE` says why).
    Failed,
    /// No usable storage (absent or its Reset Recovery failed).
    Broken,
    /// The device was unplugged during the command.
    Gone,
}

/// What the queue does after a short packet in a qTD (EHCI 4.10.2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnShort {
    /// The transfer ends (bulk and interrupt IN).
    Stop,
    /// The status stage still runs (control IN data).
    Status,
    /// No short packet is possible (SETUP, OUT, status).
    Continue,
}

#[derive(Clone, Copy)]
struct Step {
    pid: Pid,
    buffer: u64,
    bytes: u32,
    toggle: bool,
    on_short: OnShort,
}

/// Event pending delivery to the input client.
struct Pending {
    buf: [(Event, u64); RING],
    head: usize,
    len: usize,
}

impl Pending {
    fn push(&mut self, ev: Event, at: u64) {
        if self.len == RING {
            self.head = (self.head + 1) % RING;
            self.len -= 1;
        }
        self.buf[(self.head + self.len) % RING] = (ev, at);
        self.len += 1;
    }
    fn take(&mut self, out: &mut [WireEvent; MAX_EVENTS], times: &mut [u64; MAX_EVENTS]) -> usize {
        let n = self.len.min(MAX_EVENTS);
        for (slot, t) in out.iter_mut().zip(times.iter_mut()).take(n) {
            let (e, at) = self.buf[self.head];
            *slot = WireEvent {
                kind: e.kind,
                code: e.code,
                value: e.value,
            };
            *t = at;
            self.head = (self.head + 1) % RING;
            self.len -= 1;
        }
        n
    }
}

/// Exit codes (stable for the scenarios).
mod exit {
    pub const MAP_BAR: u64 = 10;
    pub const NOT_EHCI: u64 = 11;
    pub const DMA: u64 = 12;
    pub const RESET_TIMEOUT: u64 = 13;
    pub const START_TIMEOUT: u64 = 14;
    /// `strict`: a failure halted the driver (ADR 0028).
    pub const TRANSPORT_FAULT: u64 = 15;
    /// The controller reported a host system error and halted (EHCI 4.15.2.4).
    pub const HOST_SYSTEM_ERROR: u64 = 16;
}

/// What `usb_fault` injects (tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Injection {
    /// A transaction error reported on the CBW without touching the
    /// controller; the Reset Recovery that follows is real.
    Cbw,
    /// A CSW read with no command outstanding: the device STALLs its bulk
    /// IN pipe, a real halt that the recovery clears.
    Csw,
}

/// Test fault injection (`usb_fault=N|idle[:cbw|:csw]`, ADR 0028).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultInject {
    None,
    /// Block requests left before the injected failure.
    Count(u32, Injection),
    /// The first block request after two idle seconds (once).
    AfterIdle(Injection),
}

impl FaultInject {
    const IDLE_MS: u64 = 2000;

    fn parse(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::None;
        };
        let (when, kind) = value.split_once(':').unwrap_or((value, "cbw"));
        let kind = match kind {
            "cbw" => Injection::Cbw,
            "csw" => Injection::Csw,
            _ => {
                say(b"usb_fault: the EHCI driver injects cbw or csw only; ignored");
                return Self::None;
            }
        };
        match when {
            "idle" => Self::AfterIdle(kind),
            n => n.parse().map_or(Self::None, |n| Self::Count(n, kind)),
        }
    }

    fn due(&mut self, now: u64, last_request_ms: u64) -> Option<Injection> {
        match *self {
            Self::None => None,
            Self::Count(1, kind) => {
                *self = Self::None;
                Some(kind)
            }
            Self::Count(n, kind) => {
                *self = Self::Count(n - 1, kind);
                None
            }
            Self::AfterIdle(kind)
                if last_request_ms > 0 && now - last_request_ms >= Self::IDLE_MS =>
            {
                *self = Self::None;
                Some(kind)
            }
            Self::AfterIdle(_) => None,
        }
    }
}

struct Ehci {
    op: Mmio,
    params: Params,
    version: u16,
    clock: Clock,
    strict: bool,
    fault_inject: FaultInject,
    last_request_ms: u64,
    /// Upper 32 bits of every structure (CTRLDSSEGMENT) on a 64-bit
    /// controller.
    ext64: bool,
    frame_list: u64,
    async_head: (u64, u64),
    pipes: [Pipe; MAX_PIPES],
    /// Queue heads after the async head, in list order.
    async_order: [usize; MAX_PIPES],
    async_len: usize,
    /// Interrupt queue heads, in chain order; the chain runs every
    /// `periodic_period` frames.
    periodic_order: [usize; MAX_PIPES],
    periodic_len: usize,
    setup_buf: (u64, u64),
    ctrl_buf: (u64, u64),
    hid_reports: [(u64, u64); MAX_HIDS],
    storage_bufs: [(u64, u64); 3],
    devices: [Option<Device>; MAX_DEVICES],
    /// USB addresses in use (bit n = address n).
    addresses: u128,
    hids: [Option<Hid>; MAX_HIDS],
    storage: Option<Storage>,
    /// Ports to enumerate from a time on (connect debounce).
    pending: [Option<(Parent, u64)>; MAX_PENDING],
    next_hub_poll: u64,
    /// Every port seen at start has been enumerated: the block service
    /// is served from then on (init asks its capacity once).
    boot_done: bool,
    /// A block request's buffer may still be owned by the controller
    /// (a lost queue head): it is never released.
    buffer_unsafe: bool,
    /// REQUEST SENSE in progress: a failure inside it asks for no more sense.
    in_sense: bool,
    /// Deadline of the unit of work in progress (`with_deadline`).
    deadline: Option<u64>,
    /// The storage the block service has handed out (`Served`).
    served: Option<Served>,
    /// After a restart, the system disk this driver served before (digest
    /// of its GPT disk GUID, `driver_arg::EXPECTED_DISK`): no other disk
    /// is served.
    expected_disk: Option<u64>,
    /// One sector for the GPT header.
    sector_buf: (u64, u64),
    /// Input Info requests held until the boot enumeration ends.
    held_info: [Handle; abi::IPC_QUEUE_CAPACITY],
    held_len: usize,
    /// When the driver started (boot enumeration bounds).
    started: u64,
    /// Input events waiting for the client.
    events: Pending,
    keyboards: u8,
    mice: u8,
}

impl Ehci {
    /// `started`: when the process started (the Info and boot enumeration
    /// bounds count from there).
    fn open(mask: u64, clock: Clock, strict: bool, started: u64) -> Result<Self, u64> {
        let Some(bar) = polled_bar_handle(mask, 0) else {
            say(b"no BAR 0 among the capabilities");
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
        let cap0 = base.r32(cap::CAPLENGTH);
        let caplength = (cap0 & 0xFF) as u64;
        let version = (cap0 >> 16) as u16;
        if caplength < 0x10 || version < HCI_VERSION_1_0 || version == 0xFFFF {
            sys::print(b"usb-ehci: CAPLENGTH ");
            num(caplength);
            sys::print(b" HCIVERSION ");
            hex(version as u64);
            sys::print(b"\n");
            return Err(exit::NOT_EHCI);
        }
        let params = Params::decode(base.r32(cap::HCSPARAMS), base.r32(cap::HCCPARAMS));
        let op = Mmio(base.0 + caplength);
        let region = DmaRegion::alloc(ARENA_BYTES).map_err(|_| exit::DMA)?;
        // Every link pointer is 32 bits with CTRLDSSEGMENT above it (EHCI
        // 2.3.5): the arena must sit in one 4 GiB segment, the first one
        // on a 32-bit controller.
        let segment = region.phys >> 32;
        if segment != (region.phys + region.len - 1) >> 32
            || (!params.addressing_64 && segment != 0)
        {
            say(b"DMA memory outside the controller's addressing (4 GiB segment)");
            return Err(exit::DMA);
        }
        let mut arena = Arena { region, next: 0 };

        // EHCI 5.1: the firmware may still own the controller (SMM).
        if !bios_handoff(&cfg, params.eecp, &clock) && strict {
            say(b"strict: halting: the firmware keeps the controller");
            sys::process_exit(exit::TRANSPORT_FAULT);
        }

        // EHCI 4.1: stop, reset.
        op.w32(op::USBCMD, op.r32(op::USBCMD) & !op::CMD_RUN);
        if !wait_reg(
            &op,
            op::USBSTS,
            op::STS_HALTED,
            true,
            &clock,
            HALT_TIMEOUT_MS,
        ) {
            say(b"the controller does not halt");
            return Err(exit::RESET_TIMEOUT);
        }
        op.w32(op::USBCMD, op::CMD_RESET);
        if !wait_reg(
            &op,
            op::USBCMD,
            op::CMD_RESET,
            false,
            &clock,
            HC_RESET_TIMEOUT_MS,
        ) {
            return Err(exit::RESET_TIMEOUT);
        }

        // Schedules: a frame list with every entry empty and the async
        // schedule's head, a queue head that is never executed (H set, its
        // overlay halted; EHCI 4.8, 4.10.1).
        let (frame_list, fl_virt) = arena
            .alloc(FRAME_LIST_LEN as u64 * 4, 4096)
            .ok_or(exit::DMA)?;
        for i in 0..FRAME_LIST_LEN as u64 {
            w32(fl_virt + 4 * i, link::TERMINATE);
        }
        let async_head = arena.alloc(QueueHead::STRIDE, 32).ok_or(exit::DMA)?;
        let head_link = link::qh(async_head.0).ok_or(exit::DMA)?;
        let head = QueueHead {
            horizontal: head_link,
            address: 0,
            endpoint: 0,
            speed: Speed::High,
            max_packet: 64,
            head: true,
            toggle_from_qtd: false,
            control_split: false,
            s_mask: 0,
            c_mask: 0,
            tt: None,
        };
        let words = head.words();
        let n = if params.addressing_64 {
            QueueHead::WORDS_64
        } else {
            QueueHead::WORDS_32
        };
        for (i, w) in words.iter().take(n).enumerate() {
            w32(async_head.1 + 4 * i as u64, *w);
        }
        w32(async_head.1 + QueueHead::TOKEN, token::HALTED);

        let empty = QueueHead {
            horizontal: link::TERMINATE,
            head: false,
            ..head
        };
        let mut pipes = [Pipe {
            qh: (0, 0),
            qtds: (0, 0),
            desc: empty,
            schedule: Schedule::Idle,
            period: 0,
            in_use: false,
            lost: false,
        }; MAX_PIPES];
        for p in pipes.iter_mut() {
            p.qh = arena.alloc(QueueHead::STRIDE, 32).ok_or(exit::DMA)?;
            p.qtds = arena
                .alloc((QTDS_PER_PIPE as u64 + 1) * Qtd::STRIDE, 32)
                .ok_or(exit::DMA)?;
        }
        let setup_buf = arena.alloc(8, 32).ok_or(exit::DMA)?;
        let ctrl_buf = arena.alloc(CONTROL_BUFFER as u64, 64).ok_or(exit::DMA)?;
        let mut hid_reports = [(0, 0); MAX_HIDS];
        for r in hid_reports.iter_mut() {
            *r = arena.alloc(64, 64).ok_or(exit::DMA)?;
        }
        let sector_buf = arena.alloc(512, 64).ok_or(exit::DMA)?;
        let mut storage_bufs = [(0, 0); 3];
        for b in storage_bufs.iter_mut() {
            *b = arena.alloc(64, 64).ok_or(exit::DMA)?;
        }

        // No interrupts: the driver polls (Intel EHCIs have no MSI).
        op.w32(op::USBINTR, 0);
        if params.addressing_64 {
            op.w32(op::CTRLDSSEGMENT, segment as u32);
        }
        op.w32(op::PERIODICLISTBASE, frame_list as u32);
        op.w32(op::ASYNCLISTADDR, async_head.0 as u32);
        // Frame list of 1024 entries (bits 3:2 = 0), threshold 8 micro-frames.
        op.w32(op::USBCMD, op::CMD_ITC_8 | op::CMD_RUN);
        if !wait_reg(
            &op,
            op::USBSTS,
            op::STS_HALTED,
            false,
            &clock,
            HALT_TIMEOUT_MS,
        ) {
            return Err(exit::START_TIMEOUT);
        }
        op.w32(
            op::USBCMD,
            op::CMD_ITC_8 | op::CMD_RUN | op::CMD_PERIODIC | op::CMD_ASYNC,
        );
        let both = op::STS_PERIODIC | op::STS_ASYNC;
        let deadline = clock.now() + SCHEDULE_TIMEOUT_MS;
        while op.r32(op::USBSTS) & both != both {
            if clock.now() >= deadline {
                say(b"the schedules do not start");
                return Err(exit::START_TIMEOUT);
            }
            sys::thread_yield();
        }
        // Last step of 4.1: route every port to this controller (only
        // companion controllers are affected; the Intel xHCI routing in
        // its own XUSB2PR register is not).
        op.w32(op::CONFIGFLAG, op::CONFIGURED);
        if params.port_power_control {
            for port in 1..=params.ports {
                let v = op.r32(op::portsc(port));
                op.w32(op::portsc(port), portsc::write_value(v, portsc::POWER, 0));
            }
        }

        let now = clock.now();
        let mut ctl = Self {
            op,
            params,
            version,
            clock,
            strict,
            fault_inject: FaultInject::None,
            last_request_ms: 0,
            ext64: params.addressing_64,
            frame_list: fl_virt,
            async_head,
            pipes,
            async_order: [0; MAX_PIPES],
            async_len: 0,
            periodic_order: [0; MAX_PIPES],
            periodic_len: 0,
            setup_buf,
            ctrl_buf,
            hid_reports,
            storage_bufs,
            devices: [None; MAX_DEVICES],
            addresses: 1,
            hids: [None; MAX_HIDS],
            storage: None,
            pending: [None; MAX_PENDING],
            next_hub_poll: now + HUB_POLL_MS,
            boot_done: false,
            buffer_unsafe: false,
            in_sense: false,
            deadline: None,
            served: None,
            expected_disk: None,
            sector_buf,
            held_info: [Handle(0); abi::IPC_QUEUE_CAPACITY],
            held_len: 0,
            started,
            events: Pending {
                buf: [(Event::default(), 0); RING],
                head: 0,
                len: 0,
            },
            keyboards: 0,
            mice: 0,
        };
        // Every root port is looked at once its power and connection
        // settled (USB 2.0 7.1.7.3).
        for port in 1..=params.ports {
            ctl.push_pending(Parent::Root(port), now + DEBOUNCE_MS);
        }
        Ok(ctl)
    }

    fn now(&self) -> u64 {
        self.clock.now()
    }

    /// Runs one unit of work with a deadline `ms` from now (or the
    /// enclosing one if sooner); every transfer inside is cut to it.
    fn with_deadline<T>(&mut self, ms: u64, f: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.deadline;
        let mine = self.now() + ms;
        self.deadline = Some(outer.map_or(mine, |d| d.min(mine)));
        let result = f(self);
        self.deadline = outer;
        result
    }

    /// Whether the input service's Info may be answered: after the boot
    /// enumeration, or once `INFO_WAIT_MS` passed.
    fn info_ready(&self) -> bool {
        self.boot_done || self.now() >= self.started + INFO_WAIT_MS
    }

    /// Holds an Info request until `info_ready` (answered at once if the
    /// holding room is full).
    fn hold_info(&mut self, reply_to: Handle) {
        if self.info_ready() || self.held_len == self.held_info.len() {
            answer_info(self, reply_to);
        } else {
            self.held_info[self.held_len] = reply_to;
            self.held_len += 1;
        }
    }

    /// Answers the held Info requests once they are due. Called between
    /// steps and from inside every wait, so the answer never waits for a
    /// step to end.
    fn release_info(&mut self) {
        if self.held_len == 0 || !self.info_ready() {
            return;
        }
        for i in 0..self.held_len {
            answer_info(self, self.held_info[i]);
        }
        self.held_len = 0;
    }

    fn pause(&mut self, ms: u64) {
        let end = self.now() + ms;
        while self.now() < end {
            self.release_info();
            sys::thread_sleep(1);
        }
    }

    fn portsc(&self, port: u8) -> u32 {
        self.op.r32(op::portsc(port))
    }

    fn write_portsc(&self, port: u8, v: u32) {
        self.op.w32(op::portsc(port), v);
    }

    // ---- queue heads and schedules ----

    fn qtd_addr(&self, p: usize, i: usize) -> (u64, u64) {
        let (phys, virt) = self.pipes[p].qtds;
        let off = i as u64 * Qtd::STRIDE;
        (phys + off, virt + off)
    }

    fn write_qh(&self, p: usize) {
        let words = self.pipes[p].desc.words();
        let n = if self.ext64 {
            QueueHead::WORDS_64
        } else {
            QueueHead::WORDS_32
        };
        for (i, w) in words.iter().take(n).enumerate() {
            w32(self.pipes[p].qh.1 + 4 * i as u64, *w);
        }
    }

    /// Takes a free queue head for `desc` with an empty overlay and the
    /// queue's stop qTD (inactive, pointing nowhere).
    fn pipe_alloc(&mut self, desc: QueueHead) -> Option<usize> {
        let p = self.pipes.iter().position(|p| !p.in_use && !p.lost)?;
        self.pipes[p].desc = desc;
        self.pipes[p].in_use = true;
        self.pipes[p].schedule = Schedule::Idle;
        self.write_qh(p);
        let (_, stop) = self.qtd_addr(p, QTDS_PER_PIPE);
        w32(stop, link::TERMINATE);
        w32(stop + 4, link::TERMINATE);
        w32(stop + 8, 0);
        Some(p)
    }

    /// Link pointer of whatever follows position `i` of the async list.
    fn async_link_after(&self, i: usize) -> u32 {
        let target = if i < self.async_len {
            self.pipes[self.async_order[i]].qh.0
        } else {
            self.async_head.0
        };
        target as u32 | 2
    }

    /// Inserts a queue head right after the async head (EHCI 4.8.1: its
    /// horizontal link is written before the head points at it).
    fn async_link(&mut self, p: usize) {
        let next = self.async_link_after(0);
        self.pipes[p].desc.horizontal = next;
        w32(self.pipes[p].qh.1, next);
        fence(Ordering::SeqCst);
        w32(self.async_head.1, self.pipes[p].qh.0 as u32 | 2);
        self.async_order.copy_within(0..self.async_len, 1);
        self.async_order[0] = p;
        self.async_len += 1;
        self.pipes[p].schedule = Schedule::Async;
    }

    /// Removes a queue head from the async schedule (EHCI 4.8.2): the
    /// previous element skips it, then the Interrupt on Async Advance
    /// doorbell confirms the controller holds no reference to it. False
    /// if the confirmation never came (the queue head is lost).
    fn async_unlink(&mut self, p: usize) -> bool {
        let Some(i) = self.async_order[..self.async_len]
            .iter()
            .position(|&q| q == p)
        else {
            return true;
        };
        let prev = if i == 0 {
            self.async_head.1
        } else {
            self.pipes[self.async_order[i - 1]].qh.1
        };
        let next = self.async_link_after(i + 1);
        w32(prev, next);
        if i > 0 {
            self.pipes[self.async_order[i - 1]].desc.horizontal = next;
        }
        self.async_order.copy_within(i + 1..self.async_len, i);
        self.async_len -= 1;
        self.pipes[p].schedule = Schedule::Idle;
        self.op.w32(op::USBSTS, op::STS_ASYNC_ADVANCE);
        let cmd = self.op.r32(op::USBCMD);
        self.op.w32(op::USBCMD, cmd | op::CMD_IAA_DOORBELL);
        if wait_reg(
            &self.op,
            op::USBSTS,
            op::STS_ASYNC_ADVANCE,
            true,
            &self.clock,
            IAA_TIMEOUT_MS,
        ) {
            self.op.w32(op::USBSTS, op::STS_ASYNC_ADVANCE);
            return true;
        }
        say(b"the controller did not confirm the async advance (EHCI 4.8.2): queue head lost");
        record("ehci iaa timeout", p as u64);
        self.pipes[p].lost = true;
        self.strict_halt(b"async advance timeout");
        false
    }

    /// Rewrites the frame list for the current interrupt chain.
    fn write_frame_list(&self) {
        let (chain, period) = match self.periodic_order[..self.periodic_len].first() {
            Some(&p) => (
                self.pipes[p].qh.0 as u32 | 2,
                self.periodic_order[..self.periodic_len]
                    .iter()
                    .map(|&q| self.pipes[q].period)
                    .min()
                    .unwrap_or(1),
            ),
            None => (link::TERMINATE, 0),
        };
        for i in 0..FRAME_LIST_LEN {
            w32(
                self.frame_list + 4 * i as u64,
                periodic_entry(i, period, chain),
            );
        }
    }

    /// Puts an interrupt queue head at the front of the chain. The chain
    /// runs at the shortest period of its members: polling an endpoint
    /// more often than it asks is allowed (USB 2.0 5.7.4).
    fn periodic_link(&mut self, p: usize, period: u16) {
        let next = match self.periodic_order[..self.periodic_len].first() {
            Some(&q) => self.pipes[q].qh.0 as u32 | 2,
            None => link::TERMINATE,
        };
        self.pipes[p].desc.horizontal = next;
        self.pipes[p].period = period;
        w32(self.pipes[p].qh.1, next);
        fence(Ordering::SeqCst);
        self.periodic_order.copy_within(0..self.periodic_len, 1);
        self.periodic_order[0] = p;
        self.periodic_len += 1;
        self.pipes[p].schedule = Schedule::Periodic;
        self.write_frame_list();
    }

    /// Takes an interrupt queue head out of the chain and waits until the
    /// controller cannot be using it (two frames, EHCI 4.6).
    fn periodic_unlink(&mut self, p: usize) -> bool {
        let Some(i) = self.periodic_order[..self.periodic_len]
            .iter()
            .position(|&q| q == p)
        else {
            return true;
        };
        let after = self.pipes[p].desc.horizontal;
        if i > 0 {
            let prev = self.periodic_order[i - 1];
            self.pipes[prev].desc.horizontal = after;
            w32(self.pipes[prev].qh.1, after);
        }
        self.periodic_order.copy_within(i + 1..self.periodic_len, i);
        self.periodic_len -= 1;
        self.pipes[p].schedule = Schedule::Idle;
        self.write_frame_list();
        let start = self.op.r32(op::FRINDEX);
        let deadline = self.now() + PERIODIC_UNLINK_TIMEOUT_MS;
        loop {
            let moved = self.op.r32(op::FRINDEX).wrapping_sub(start) & 0x3FFF;
            if moved >= PERIODIC_UNLINK_MICROFRAMES {
                return true;
            }
            if self.now() >= deadline {
                say(b"the frame index does not advance: interrupt queue head lost");
                record("ehci frindex stuck", p as u64);
                self.pipes[p].lost = true;
                self.strict_halt(b"frame index stuck");
                return false;
            }
            sys::thread_yield();
        }
    }

    fn pipe_free(&mut self, p: usize) {
        let released = match self.pipes[p].schedule {
            Schedule::Async => self.async_unlink(p),
            Schedule::Periodic => self.periodic_unlink(p),
            Schedule::Idle => true,
        };
        if released {
            self.pipes[p].in_use = false;
        }
    }

    /// Changes a linked async queue head's endpoint characteristics: out of
    /// the schedule, rewritten, back in (EHCI 4.8.2).
    fn update_qh(&mut self, p: usize, address: u8, max_packet: u16) -> bool {
        if !self.async_unlink(p) {
            return false;
        }
        self.pipes[p].desc.address = address;
        self.pipes[p].desc.max_packet = max_packet;
        self.write_qh(p);
        self.async_link(p);
        true
    }

    /// Leaves a halted or unlinked queue head idle: no next qTD, not active,
    /// not halted (EHCI 4.10.2). The pointer goes first so the controller
    /// cannot resume the rest of a failed chain.
    fn overlay_idle(&self, p: usize, keep_toggle: bool) {
        let qh = self.pipes[p].qh.1;
        w32(qh + QueueHead::NEXT_QTD, link::TERMINATE);
        w32(qh + QueueHead::ALT_NEXT_QTD, link::TERMINATE);
        fence(Ordering::SeqCst);
        let t = r32(qh + QueueHead::TOKEN);
        w32(
            qh + QueueHead::TOKEN,
            overlay_token(t, keep_toggle, self.pipes[p].desc.speed),
        );
    }

    // ---- transfers ----

    /// Writes `steps` as a qTD chain and hands it to the idle queue head.
    /// The only qTDs an idle overlay can lead to are the stop qTD (after a
    /// short packet) or none, so the overlay pointers are cleared first;
    /// then the qTDs are written (each token last), the overlay token is
    /// made idle with its toggle kept, and only then does the next pointer
    /// give the controller the chain (EHCI 4.10.2).
    fn submit(&mut self, p: usize, steps: &[Step]) -> Result<(), Fail> {
        let n = steps.len();
        if n == 0 || n > QTDS_PER_PIPE {
            return Err(Fail::Buffer);
        }
        if self.pipes[p].lost {
            return Err(Fail::Lost);
        }
        let qh = self.pipes[p].qh.1;
        w32(qh + QueueHead::NEXT_QTD, link::TERMINATE);
        w32(qh + QueueHead::ALT_NEXT_QTD, link::TERMINATE);
        fence(Ordering::SeqCst);
        let (stop, _) = self.qtd_addr(p, QTDS_PER_PIPE);
        let (last, _) = self.qtd_addr(p, n - 1);
        let words_len = if self.ext64 {
            Qtd::WORDS_64
        } else {
            Qtd::WORDS_32
        };
        for (i, s) in steps.iter().enumerate() {
            if !self.ext64 && s.buffer + s.bytes as u64 > 1 << 32 {
                return Err(Fail::Buffer);
            }
            let next = if i + 1 < n {
                self.qtd_addr(p, i + 1).0 as u32
            } else {
                link::TERMINATE
            };
            let alt_next = match s.on_short {
                OnShort::Stop => stop as u32,
                OnShort::Status => last as u32,
                OnShort::Continue => link::TERMINATE,
            };
            let words = Qtd {
                next,
                alt_next,
                pid: s.pid,
                bytes: s.bytes,
                toggle: s.toggle,
                buffer: s.buffer,
            }
            .words()
            .ok_or(Fail::Buffer)?;
            let (_, virt) = self.qtd_addr(p, i);
            for (k, w) in words.iter().take(words_len).enumerate() {
                if k != 2 {
                    w32(virt + 4 * k as u64, *w);
                }
            }
            fence(Ordering::SeqCst);
            w32(virt + 8, words[2]);
        }
        fence(Ordering::SeqCst);
        let t = r32(qh + QueueHead::TOKEN);
        w32(
            qh + QueueHead::TOKEN,
            overlay_token(t, true, self.pipes[p].desc.speed),
        );
        fence(Ordering::SeqCst);
        w32(qh + QueueHead::NEXT_QTD, self.qtd_addr(p, 0).0 as u32);
        Ok(())
    }

    /// State of a submitted chain: `None` while it runs; the bytes moved
    /// when it ended; the failure when a qTD halted the queue. The first
    /// qTD that is not complete decides (EHCI 4.10.2: a short packet goes
    /// to the alternate pointer).
    fn scan(&self, p: usize, steps: &[Step]) -> Option<Result<u32, Fail>> {
        let mut moved = 0;
        let mut i = 0;
        while i < steps.len() {
            let t = r32(self.qtd_addr(p, i).1 + 8);
            match decode_token(t) {
                Outcome::Active => return None,
                Outcome::Error(e) => return Some(Err(Fail::Error(e, t))),
                Outcome::Done { remaining } => {
                    moved += steps[i].bytes.saturating_sub(remaining);
                    if remaining > 0 {
                        match steps[i].on_short {
                            OnShort::Stop => return Some(Ok(moved)),
                            OnShort::Status => {
                                i = steps.len() - 1;
                                continue;
                            }
                            OnShort::Continue => {}
                        }
                    }
                    i += 1;
                }
            }
        }
        Some(Ok(moved))
    }

    /// Runs a chain on an async queue head until `deadline` (cut to the
    /// deadline of the unit of work; refused if that has passed). A queue that
    /// halted stays halted for the caller to report (and dump) before it
    /// calls `overlay_idle`; one still running at the deadline is retired:
    /// out of the schedule, idle, back in.
    fn run(&mut self, p: usize, steps: &[Step], deadline: u64) -> Result<u32, Fail> {
        let deadline = self.deadline.map_or(deadline, |cap| deadline.min(cap));
        let start = self.now();
        if start >= deadline {
            return Err(Fail::Refused);
        }
        self.submit(p, steps)?;
        loop {
            if let Some(result) = self.scan(p, steps) {
                return result;
            }
            let now = self.now();
            if now >= deadline {
                // `strict`: the dump shows the queue as the timeout left it,
                // before it is retired.
                if self.strict {
                    let kind = if self.pipes[p].desc.endpoint == 0 {
                        "ctrl"
                    } else {
                        "transfer"
                    };
                    self.report_failure(kind, p, Fail::Timeout);
                    self.strict_halt(b"transfer timeout");
                }
                if !self.async_unlink(p) {
                    return Err(Fail::Lost);
                }
                self.overlay_idle(p, true);
                self.async_link(p);
                return Err(Fail::Timeout);
            }
            self.release_info();
            if now - start < SPIN_MS {
                sys::thread_yield();
            } else {
                sys::thread_sleep(1);
            }
        }
    }

    /// A control transfer on the EP0 queue head `p` (USB 2.0 8.5.3): SETUP,
    /// optional data in the control buffer, status. Every failure is
    /// reported and recorded; with `strict` it halts the driver, except a
    /// STALL of a request the specification makes optional
    /// (`optional`). Returns the data bytes moved.
    fn control(
        &mut self,
        p: usize,
        setup: SetupPacket,
        data: Option<u16>,
        optional: bool,
    ) -> Result<u32, Fail> {
        let bytes = setup.to_bytes();
        for (i, chunk) in bytes.chunks(4).enumerate() {
            w32(
                self.setup_buf.1 + 4 * i as u64,
                u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            );
        }
        let dir_in = setup.is_in();
        let mut steps = [Step {
            pid: Pid::Setup,
            buffer: self.setup_buf.0,
            bytes: 8,
            toggle: false,
            on_short: OnShort::Continue,
        }; 3];
        let mut n = 1;
        if let Some(len) = data {
            steps[1] = Step {
                pid: if dir_in { Pid::In } else { Pid::Out },
                buffer: self.ctrl_buf.0,
                bytes: len.min(CONTROL_BUFFER) as u32,
                toggle: true,
                on_short: if dir_in {
                    OnShort::Status
                } else {
                    OnShort::Continue
                },
            };
            n = 2;
        }
        steps[n] = Step {
            pid: if dir_in && data.is_some() {
                Pid::Out
            } else {
                Pid::In
            },
            buffer: 0,
            bytes: 0,
            toggle: true,
            on_short: OnShort::Continue,
        };
        let deadline = self.now() + CONTROL_TIMEOUT_MS;
        match self.run(p, &steps[..n + 1], deadline) {
            Ok(moved) => Ok(moved.saturating_sub(8)),
            Err(fail) => {
                if !matches!(fail, Fail::Lost | Fail::Refused) && self.unplugged(p) {
                    self.overlay_idle(p, true);
                    return Err(fail);
                }
                self.report_failure("ctrl", p, fail);
                sys::print(b"usb-ehci:   request ");
                for b in bytes {
                    hex(b as u64);
                    sys::print(b" ");
                }
                sys::print(b"\n");
                if !(optional && fail.is_stall()) && fail != Fail::Refused {
                    self.strict_halt(b"control transfer");
                }
                if fail != Fail::Lost {
                    self.overlay_idle(p, true);
                    self.clear_tt(p, fail, hub::TT_CONTROL, dir_in);
                }
                Err(fail)
            }
        }
    }

    /// A bulk transfer on queue head `p` until `deadline`, in qTDs that end
    /// on packet boundaries. The caller reports failures and leaves a
    /// halted queue idle.
    fn bulk(
        &mut self,
        p: usize,
        dir_in: bool,
        phys: u64,
        len: u32,
        deadline: u64,
    ) -> Result<u32, Fail> {
        let mut pieces = [(0u64, 0u32); QTDS_PER_PIPE];
        let n = split_transfer(phys, len, self.pipes[p].desc.max_packet, &mut pieces)
            .ok_or(Fail::Buffer)?;
        let mut steps = [Step {
            pid: Pid::Out,
            buffer: 0,
            bytes: 0,
            toggle: false,
            on_short: OnShort::Continue,
        }; QTDS_PER_PIPE];
        for (s, (at, bytes)) in steps.iter_mut().zip(pieces.iter()).take(n) {
            *s = Step {
                pid: if dir_in { Pid::In } else { Pid::Out },
                buffer: *at,
                bytes: *bytes,
                toggle: false,
                on_short: if dir_in {
                    OnShort::Stop
                } else {
                    OnShort::Continue
                },
            };
        }
        self.run(p, &steps[..n], deadline)
    }

    /// Prints and records a failed transfer (ADR 0028).
    fn report_failure(&self, kind: &str, p: usize, fail: Fail) {
        let d = self.pipes[p].desc;
        sys::print(b"usb-ehci: ");
        sys::print(kind.as_bytes());
        sys::print(b" transfer to address ");
        num(d.address as u64);
        sys::print(b" endpoint ");
        num(d.endpoint as u64);
        sys::print(b" (");
        sys::print(d.speed.name().as_bytes());
        sys::print(b" speed) failed: ");
        fail.print();
        sys::print(b"\n");
        record_parts(kind, fail.short(), fail.code());
    }

    /// An IN control request that must return at least `need` bytes: a
    /// shorter reply would leave stale bytes of the control buffer behind.
    fn control_read(
        &mut self,
        p: usize,
        setup: SetupPacket,
        need: u16,
        what: &'static str,
    ) -> Result<u32, &'static str> {
        let moved = self
            .control(p, setup, Some(setup.length), false)
            .map_err(|_| what)?;
        if moved < need as u32 {
            sys::print(b"usb-ehci: ");
            sys::print(what.as_bytes());
            sys::print(b": ");
            num(moved as u64);
            sys::print(b" bytes, at least ");
            num(need as u64);
            sys::print(b" expected\n");
            record("ehci short reply", moved as u64);
            self.strict_halt(what.as_bytes());
            return Err("short reply to a control request");
        }
        Ok(moved)
    }

    /// The device that owns queue head `p`.
    fn pipe_device(&self, p: usize) -> Option<usize> {
        let a = self.pipes[p].desc.address;
        self.devices
            .iter()
            .position(|d| d.is_some_and(|d| d.ep0 == p || (a != 0 && d.address == a)))
    }

    /// A transfer failed on a device whose port reports it unplugged: that
    /// is the disconnect, which the port change reports, not a fault.
    fn unplugged(&mut self, p: usize) -> bool {
        let Some(d) = self.pipe_device(p).and_then(|i| self.devices[i]) else {
            return false;
        };
        if self.strict && matches!(d.parent, Parent::Hub(..)) {
            say(b"strict: controller state before asking the hub about the port");
            self.dump();
        }
        if self.port_connected(d.parent) {
            return false;
        }
        self.print_parent(d.parent);
        sys::print(b": transfer ended, the device is unplugged\n");
        true
    }

    /// `ClearTTBuffer` (USB 2.0 11.17.5, 11.24.2.3) after a split control
    /// or bulk transfer was aborted by a timeout or transaction errors, so
    /// the translator does not keep the endpoint's buffer busy.
    fn clear_tt(&mut self, p: usize, fail: Fail, ep_type: u8, dir_in: bool) {
        let d = self.pipes[p].desc;
        let Some(tt) = d.tt else {
            return;
        };
        if !matches!(
            fail,
            Fail::Timeout | Fail::Error(TransferError::Transaction, _)
        ) {
            return;
        }
        let Some(hub_ep0) = self
            .devices
            .iter()
            .flatten()
            .find(|h| h.address == tt.hub)
            .map(|h| h.ep0)
        else {
            return;
        };
        say(b"ClearTTBuffer on the translator (USB 2.0 11.24.2.3)");
        let _ = self.control(
            hub_ep0,
            hub::clear_tt_buffer(d.address, d.endpoint, ep_type, dir_in),
            None,
            false,
        );
    }

    fn ctrl_data(&self, len: usize) -> [u8; CONTROL_BUFFER as usize] {
        let mut out = [0u8; CONTROL_BUFFER as usize];
        let len = len.min(out.len());
        // SAFETY: the driver's control buffer in its DMA arena.
        let src = unsafe { core::slice::from_raw_parts(self.ctrl_buf.1 as *const u8, len) };
        out[..len].copy_from_slice(src);
        out
    }

    // ---- ports and enumeration ----

    fn push_pending(&mut self, parent: Parent, at: u64) {
        if self.is_pending(parent) {
            return;
        }
        match self.pending.iter_mut().find(|e| e.is_none()) {
            Some(slot) => *slot = Some((parent, at)),
            None => {
                self.print_parent(parent);
                sys::print(b": too many ports waiting for enumeration; ignored\n");
                record("ehci pending full", 0);
            }
        }
    }

    fn is_pending(&self, parent: Parent) -> bool {
        self.pending.iter().flatten().any(|(q, _)| *q == parent)
    }

    fn device_on(&self, parent: Parent) -> Option<usize> {
        self.devices
            .iter()
            .position(|d| d.is_some_and(|d| d.parent == parent))
    }

    fn print_path(&self, index: usize) {
        if let Some(d) = self.devices[index] {
            for (i, port) in d.path[..d.depth as usize].iter().enumerate() {
                if i > 0 {
                    sys::print(b".");
                }
                num(*port as u64);
            }
        }
    }

    fn print_parent(&self, parent: Parent) {
        sys::print(b"usb-ehci: port ");
        match parent {
            Parent::Root(port) => num(port as u64),
            Parent::Hub(h, port) => {
                self.print_path(h);
                sys::print(b".");
                num(port as u64);
            }
        }
    }

    /// Hands a root port to a companion controller (EHCI 4.2.2), or says
    /// that none exists (Intel 6/7 series have a rate matching hub instead,
    /// so a full- or low-speed device never reaches a root port there).
    fn release_port(&mut self, port: u8, what: &[u8]) {
        self.print_parent(Parent::Root(port));
        sys::print(b": ");
        sys::print(what);
        if self.params.companions > 0 {
            let v = self.portsc(port);
            self.write_portsc(port, portsc::write_value(v, portsc::OWNER, 0));
            sys::print(b" device handed to a companion controller (EHCI 4.2.2)\n");
        } else {
            sys::print(b" device on a root port without companion controllers: not supported (EHCI 4.2.2)\n");
            record("ehci no companion", port as u64);
        }
    }

    /// Resets a root port (EHCI 2.3.9, 4.2.2). `None` when the device is
    /// not high speed and was handed over (or cannot be).
    fn reset_root_port(&mut self, port: u8) -> Result<Option<Speed>, &'static str> {
        let v = self.portsc(port);
        self.write_portsc(port, portsc::write_value(v, v & portsc::CHANGE_BITS, 0));
        if portsc::low_speed_line(v) {
            self.release_port(port, b"low-speed");
            return Ok(None);
        }
        let v = self.portsc(port);
        self.write_portsc(port, portsc::write_value(v, portsc::RESET, portsc::ENABLED));
        self.pause(ROOT_RESET_MS);
        let v = self.portsc(port);
        self.write_portsc(port, portsc::write_value(v, 0, portsc::RESET));
        if !wait_reg(
            &self.op,
            op::portsc(port),
            portsc::RESET,
            false,
            &self.clock,
            PORT_RESET_END_MS,
        ) {
            return Err("the port reset does not end (EHCI 2.3.9)");
        }
        let v = self.portsc(port);
        // A change bit raised around the reset belongs to the device just
        // reset, whose presence is checked right here.
        self.write_portsc(port, portsc::write_value(v, v & portsc::CHANGE_BITS, 0));
        if v & portsc::CONNECTED == 0 {
            return Err("the device left during the reset");
        }
        if v & portsc::ENABLED == 0 {
            self.release_port(port, b"full-speed");
            return Ok(None);
        }
        self.pause(RESET_RECOVERY_MS);
        Ok(Some(Speed::High))
    }

    fn hub_request(&mut self, hub: usize, setup: SetupPacket) -> Result<(), &'static str> {
        let ep0 = self.devices[hub].ok_or("the hub is gone")?.ep0;
        self.control(ep0, setup, None, false)
            .map(|_| ())
            .map_err(|_| "hub request failed")
    }

    fn hub_port_status(&mut self, hub: usize, port: u8) -> Result<hub::PortStatus, &'static str> {
        let ep0 = self.devices[hub].ok_or("the hub is gone")?.ep0;
        self.control_read(ep0, hub::get_port_status(port), 4, "GetPortStatus failed")?;
        hub::PortStatus::parse(&self.ctrl_data(4)).ok_or("invalid port status")
    }

    /// Acknowledges every change bit of a hub port (USB 2.0 11.24.2.7.2).
    fn hub_clear_changes(
        &mut self,
        hub: usize,
        port: u8,
        st: &hub::PortStatus,
    ) -> Result<(), &'static str> {
        let mut features = [0u16; 5];
        let n = st.changes_to_clear(&mut features);
        for f in &features[..n] {
            self.hub_request(hub, hub::clear_port_feature(port, *f))?;
        }
        Ok(())
    }

    /// Resets a hub port and returns the speed of its device (USB 2.0
    /// 11.24.2.13 PORT_RESET, 11.24.2.7 C_PORT_RESET).
    fn reset_hub_port(&mut self, hub: usize, port: u8) -> Result<Speed, &'static str> {
        self.hub_request(hub, hub::set_port_feature(port, hub::feature::PORT_RESET))?;
        let deadline = self.now() + HUB_RESET_TIMEOUT_MS;
        let st = loop {
            self.pause(10);
            let st = self.hub_port_status(hub, port)?;
            if st.reset_done() && !st.resetting() {
                break st;
            }
            if self.now() >= deadline {
                return Err("the hub port reset does not end (USB 2.0 11.5.1.5)");
            }
        };
        self.hub_request(
            hub,
            hub::clear_port_feature(port, hub::feature::C_PORT_RESET),
        )?;
        if !st.connected() {
            return Err("the device left during the reset");
        }
        if !st.enabled() {
            return Err("the hub port is not enabled after the reset");
        }
        self.pause(RESET_RECOVERY_MS);
        Ok(st.speed())
    }

    /// Disables the port of a device that failed its enumeration, so it
    /// cannot answer at address 0 again (USB 2.0 11.5.1.3).
    fn disable_port(&mut self, parent: Parent) {
        match parent {
            Parent::Root(port) => {
                let v = self.portsc(port);
                self.write_portsc(port, portsc::write_value(v, 0, portsc::ENABLED));
            }
            Parent::Hub(h, port) => {
                let _ =
                    self.hub_request(h, hub::clear_port_feature(port, hub::feature::PORT_ENABLE));
            }
        }
    }

    fn attach(&mut self, parent: Parent) {
        if let Err(why) = self.try_attach(parent) {
            self.print_parent(parent);
            sys::print(b": ");
            sys::print(why.as_bytes());
            sys::print(b"\n");
            record("ehci enumeration", 0);
        }
    }

    /// Resets the port and enumerates the device on it (USB 2.0 9.1.2).
    fn try_attach(&mut self, parent: Parent) -> Result<(), &'static str> {
        let (speed, tt, path, depth) = match parent {
            Parent::Root(port) => {
                let v = self.portsc(port);
                if v & portsc::CONNECTED == 0 || v & portsc::OWNER != 0 {
                    return Ok(());
                }
                let Some(speed) = self.reset_root_port(port)? else {
                    return Ok(());
                };
                let mut path = [0u8; 7];
                path[0] = port;
                (speed, None, path, 1)
            }
            Parent::Hub(h, port) => {
                let hub_dev = self.devices[h].ok_or("the hub is gone")?;
                let st = self.hub_port_status(h, port)?;
                self.hub_clear_changes(h, port, &st)?;
                if !st.connected() {
                    return Ok(());
                }
                let speed = self.reset_hub_port(h, port)?;
                let tt = match tt_for(hub_dev.speed, hub_dev.address, hub_dev.tt, port, speed) {
                    Ok(tt) => tt,
                    Err(why) => {
                        self.disable_port(parent);
                        return Err(why);
                    }
                };
                let mut path = hub_dev.path;
                path[hub_dev.depth as usize] = port;
                (speed, tt, path, hub_dev.depth + 1)
            }
        };
        let Some(index) = self.devices.iter().position(|d| d.is_none()) else {
            self.disable_port(parent);
            return Err("too many devices");
        };
        let Some(ep0) = self.pipe_alloc(QueueHead {
            horizontal: link::TERMINATE,
            address: 0,
            endpoint: 0,
            speed,
            max_packet: speed.default_ep0_max_packet(),
            head: false,
            toggle_from_qtd: true,
            control_split: speed != Speed::High,
            s_mask: 0,
            c_mask: 0,
            tt,
        }) else {
            self.disable_port(parent);
            return Err("no free queue heads");
        };
        self.async_link(ep0);
        self.devices[index] = Some(Device {
            address: 0,
            speed,
            tt,
            parent,
            path,
            depth,
            ep0,
            hub: None,
        });
        if let Err(why) = self.enumerate(index) {
            self.detach(index);
            self.disable_port(parent);
            return Err(why);
        }
        Ok(())
    }

    fn enumerate(&mut self, index: usize) -> Result<(), &'static str> {
        let dev = self.devices[index].ok_or("device")?;
        let ep0 = dev.ep0;
        // bMaxPacketSize0 from the first 8 bytes, which any EP0 can carry.
        self.control_read(
            ep0,
            SetupPacket::get_descriptor(request::DESC_DEVICE, 0, 8),
            8,
            "GET_DESCRIPTOR(device, 8 bytes) failed",
        )?;
        let head = self.ctrl_data(8);
        let max_packet0 = head[7] as u16;
        // USB 2.0 9.6.1: 8, 16, 32 or 64; 64 at high speed.
        if head[1] != request::DESC_DEVICE
            || !matches!(max_packet0, 8 | 16 | 32 | 64)
            || (dev.speed == Speed::High && max_packet0 != 64)
        {
            return Err("invalid device descriptor header");
        }
        let address = (1..128u8)
            .find(|a| self.addresses & 1 << a == 0)
            .ok_or("no free USB address")?;
        self.control(ep0, set_address(address), None, false)
            .map_err(|_| "SET_ADDRESS failed")?;
        self.addresses |= 1 << address;
        if let Some(d) = self.devices[index].as_mut() {
            d.address = address;
        }
        self.pause(SET_ADDRESS_MS);
        if !self.update_qh(ep0, address, max_packet0) {
            return Err("EP0 queue head lost");
        }
        self.control_read(
            ep0,
            SetupPacket::get_descriptor(request::DESC_DEVICE, 0, DeviceDescriptor::LEN),
            DeviceDescriptor::LEN,
            "GET_DESCRIPTOR(device) failed",
        )?;
        let raw_desc = self.ctrl_data(DeviceDescriptor::LEN as usize);
        let desc = DeviceDescriptor::parse(&raw_desc).ok_or("invalid device descriptor")?;
        // iSerialNumber (USB 2.0 9.6.1), for the storage identity.
        let serial_index = raw_desc[16];
        self.control_read(
            ep0,
            SetupPacket::get_descriptor(request::DESC_CONFIGURATION, 0, 9),
            9,
            "GET_DESCRIPTOR(config) failed",
        )?;
        let short = self.ctrl_data(9);
        let declared = u16::from_le_bytes([short[2], short[3]]);
        let total = declared.clamp(9, CONTROL_BUFFER);
        if declared > CONTROL_BUFFER {
            self.print_parent(dev.parent);
            sys::print(b": configuration descriptor of ");
            num(declared as u64);
            sys::print(b" bytes, only the first 512 are read\n");
        }
        let moved = self.control_read(
            ep0,
            SetupPacket::get_descriptor(request::DESC_CONFIGURATION, 0, total),
            9,
            "GET_DESCRIPTOR(full config) failed",
        )?;
        let config = self.ctrl_data(moved as usize);
        let config = &config[..moved as usize];

        self.print_parent(dev.parent);
        sys::print(b": device ");
        hex4(desc.vendor as u64);
        sys::print(b":");
        hex4(desc.product as u64);
        sys::print(b" class ");
        num(desc.class as u64);
        sys::print(b" bcdUSB ");
        hex4(desc.usb_version as u64);
        sys::print(b" speed ");
        sys::print(dev.speed.name().as_bytes());
        sys::print(b" address ");
        num(address as u64);
        if let Some(tt) = dev.tt {
            sys::print(b" (translator: hub ");
            num(tt.hub as u64);
            sys::print(b" port ");
            num(tt.port as u64);
            sys::print(b")");
        }
        sys::print(b"\n");

        if config.len() < 9 {
            return Err("configuration descriptor too short");
        }
        if desc.class == hub::CLASS {
            if dev.depth > hub::MAX_DEPTH {
                return Err("a hub beyond the fifth tier (USB 2.0 4.1.1)");
            }
            return self.configure_hub(index, config[5]);
        }
        let mut hids = [None; 4];
        let (value, n) = parse_configuration(config, &mut hids).ok_or("invalid configuration")?;
        if n > 0 {
            return self.configure_hids(index, value, &hids[..n]);
        }
        if let Some((value, iface)) = parse_storage_interface(config) {
            if self.storage.is_some() {
                self.print_parent(dev.parent);
                sys::print(
                    b": a storage device is already served; this one is left unconfigured\n",
                );
                return Ok(());
            }
            return self.configure_storage(index, value, iface, serial_index);
        }
        self.print_parent(dev.parent);
        sys::print(b": no boot HID, hub or bulk-only storage interface; left unconfigured\n");
        Ok(())
    }

    /// A USB 2.0 hub (USB 2.0 11.23, 11.24): configured, ports powered,
    /// each port looked at once power and connection settled. A multi-TT
    /// hub stays in its default single-TT setting (11.23.1).
    fn configure_hub(&mut self, index: usize, config_value: u8) -> Result<(), &'static str> {
        let dev = self.devices[index].ok_or("device")?;
        self.control(
            dev.ep0,
            SetupPacket::set_configuration(config_value),
            None,
            false,
        )
        .map_err(|_| "SET_CONFIGURATION (hub) failed")?;
        self.control_read(
            dev.ep0,
            hub::get_descriptor(hub::DESCRIPTOR_LEN),
            7,
            "GetHubDescriptor failed",
        )?;
        let desc = hub::Descriptor::parse(&self.ctrl_data(hub::DESCRIPTOR_LEN as usize))
            .ok_or("invalid hub descriptor")?;
        let ports = desc.ports.min(15);
        if desc.power_switching() {
            for port in 1..=ports {
                self.hub_request(index, hub::set_port_feature(port, hub::feature::PORT_POWER))?;
            }
        }
        if let Some(d) = self.devices[index].as_mut() {
            d.hub = Some(HubState {
                ports,
                failed: false,
            });
        }
        self.print_parent(dev.parent);
        sys::print(b": hub with ");
        num(ports as u64);
        sys::print(b" ports\n");
        let at = self.now() + desc.power_on_ms as u64 + DEBOUNCE_MS;
        for port in 1..=ports {
            self.push_pending(Parent::Hub(index, port), at);
        }
        Ok(())
    }

    /// Boot HID interfaces (HID 1.11 7.2.6 SET_PROTOCOL, 7.2.4 SET_IDLE),
    /// each with an interrupt queue head in the periodic schedule.
    fn configure_hids(
        &mut self,
        index: usize,
        value: u8,
        hids: &[Option<HidInterface>],
    ) -> Result<(), &'static str> {
        let dev = self.devices[index].ok_or("device")?;
        self.control(dev.ep0, SetupPacket::set_configuration(value), None, false)
            .map_err(|_| "SET_CONFIGURATION failed")?;
        for hid in hids.iter().flatten() {
            let decoder = match hid.protocol {
                1 => Decoder::Keyboard(HidBootKeyboard::new()),
                2 => Decoder::Mouse(HidBootMouse::new()),
                _ => continue,
            };
            if hid.max_packet_size > 64 {
                self.print_parent(dev.parent);
                sys::print(b": HID interface with ");
                num(hid.max_packet_size as u64);
                sys::print(b"-byte packets not supported (boot reports fit 64)\n");
                continue;
            }
            self.control(
                dev.ep0,
                SetupPacket::hid_set_protocol(hid.interface, 0),
                None,
                false,
            )
            .map_err(|_| "SET_PROTOCOL failed")?;
            // SET_IDLE is required of boot keyboards and optional for mice
            // (HID 1.11 Appendix G): a mouse's STALL is reported and
            // tolerated; any failure leaves the default idle rate.
            let _ = self.control(
                dev.ep0,
                SetupPacket::hid_set_idle(hid.interface),
                None,
                hid.protocol == 2,
            );
            let h = self
                .hids
                .iter()
                .position(|h| h.is_none())
                .ok_or("too many HID interfaces")?;
            let sched = interrupt_schedule(dev.speed, hid.b_interval);
            let pipe = self
                .pipe_alloc(QueueHead {
                    horizontal: link::TERMINATE,
                    address: dev.address,
                    endpoint: hid.endpoint,
                    speed: dev.speed,
                    max_packet: hid.max_packet_size,
                    head: false,
                    toggle_from_qtd: false,
                    control_split: false,
                    s_mask: sched.s_mask,
                    c_mask: sched.c_mask,
                    tt: dev.tt,
                })
                .ok_or("no free queue heads")?;
            self.periodic_link(pipe, sched.period_frames);
            self.hids[h] = Some(Hid {
                device: index,
                pipe,
                endpoint: hid.endpoint,
                decoder,
                report: self.hid_reports[h],
                len: hid.max_packet_size.max(1),
                seen: false,
                stall_cleared: false,
                clear_owed: false,
                stopped: false,
            });
            match decoder {
                Decoder::Keyboard(_) => self.keyboards += 1,
                Decoder::Mouse(_) => self.mice += 1,
            }
            self.print_parent(dev.parent);
            sys::print(match decoder {
                Decoder::Keyboard(_) => b": HID keyboard (boot protocol, ".as_slice(),
                Decoder::Mouse(_) => b": HID mouse (boot protocol, ",
            });
            num(hid.max_packet_size as u64);
            sys::print(b" bytes per report, endpoint ");
            num(hid.endpoint as u64);
            sys::print(b", every ");
            num(sched.period_frames as u64);
            sys::print(b" ms)\n");
            self.arm(h);
        }
        Ok(())
    }

    /// Bulk-only mass storage (ADR 0023): queue heads for both bulk
    /// endpoints, then INQUIRY, TEST UNIT READY and READ CAPACITY(10).
    fn configure_storage(
        &mut self,
        index: usize,
        value: u8,
        iface: StorageInterface,
        serial_index: u8,
    ) -> Result<(), &'static str> {
        let dev = self.devices[index].ok_or("device")?;
        // The serial number string (USB 2.0 9.6.7, US English LANGID
        // 0x0409) is part of the identity; a device without one, or that
        // refuses it (a STALL, tolerated), is identified without it.
        let mut serial = [0u8; SERIAL_LEN];
        if serial_index != 0 {
            let request = SetupPacket {
                request_type: 0x80,
                request: request::GET_DESCRIPTOR,
                value: (request::DESC_STRING as u16) << 8 | serial_index as u16,
                index: 0x0409,
                length: SERIAL_LEN as u16,
            };
            match self.control(dev.ep0, request, Some(SERIAL_LEN as u16), true) {
                Ok(n) => {
                    let data = self.ctrl_data(n as usize);
                    serial[..n as usize].copy_from_slice(&data[..n as usize]);
                }
                // The device refuses the string: it has none to give.
                Err(fail) if fail.is_stall() => {}
                // Anything else (a timeout, no time left) says nothing about
                // the identity: the enumeration fails and the next connect
                // tries again, rather than taking it for another device.
                Err(_) => return Err("serial number read failed"),
            }
        }
        self.control(dev.ep0, SetupPacket::set_configuration(value), None, false)
            .map_err(|_| "SET_CONFIGURATION failed")?;
        let bulk = |endpoint: u8, max_packet: u16| QueueHead {
            horizontal: link::TERMINATE,
            address: dev.address,
            endpoint,
            speed: dev.speed,
            max_packet,
            head: false,
            toggle_from_qtd: false,
            control_split: false,
            s_mask: 0,
            c_mask: 0,
            tt: dev.tt,
        };
        let pipe_in = self
            .pipe_alloc(bulk(iface.bulk_in, iface.max_packet_in))
            .ok_or("no free queue heads")?;
        self.async_link(pipe_in);
        let Some(pipe_out) = self.pipe_alloc(bulk(iface.bulk_out, iface.max_packet_out)) else {
            self.pipe_free(pipe_in);
            return Err("no free queue heads");
        };
        self.async_link(pipe_out);
        let [cbw, csw, small] = self.storage_bufs;
        self.storage = Some(Storage {
            device: index,
            interface: iface.interface,
            bulk_in: Bulk {
                pipe: pipe_in,
                address: 0x80 | iface.bulk_in,
            },
            bulk_out: Bulk {
                pipe: pipe_out,
                address: iface.bulk_out,
            },
            tag: 0,
            cbw,
            csw,
            small,
            capacity: 0,
            sync_cache: true,
            broken: false,
            needs_reset: false,
            errors_reported: 0,
        });
        // The device was detached by the caller on failure, which frees
        // the bulk queue heads with the storage.
        self.storage_init(dev.parent, serial)
    }

    fn storage_init(
        &mut self,
        parent: Parent,
        serial: [u8; SERIAL_LEN],
    ) -> Result<(), &'static str> {
        let (small_phys, small_virt) = self.storage.as_ref().ok_or("storage")?.small;
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        let small = || unsafe { core::slice::from_raw_parts(small_virt as *const u8, 64) };
        self.scsi(
            scsi::inquiry(),
            Some((small_phys, scsi::INQUIRY_LEN as u32, true)),
            NO_MEDIUM_WINDOW_MS,
            None,
        )
        .map_err(|e| self.scsi_failed(e, "INQUIRY failed"))?;
        let inq = scsi::Inquiry::parse(small()).ok_or("invalid INQUIRY")?;
        if inq.peripheral_type != 0 {
            return Err("not a direct access device (disk)");
        }
        let ready_until = self.now() + READY_WAIT_MS;
        loop {
            match self.scsi(scsi::test_unit_ready(), None, NO_MEDIUM_WINDOW_MS, None) {
                Ok(_) => break,
                Err(ScsiFail::Failed) => {
                    let sense = self.sense();
                    // SPC-4: NOT READY with ASC 3Ah is "medium not
                    // present", not a transient state (an empty card
                    // reader). The device stays enumerated without a
                    // block service; nothing is retried.
                    if sense.is_some_and(|s| s.key == scsi::Sense::NOT_READY && s.asc == 0x3A) {
                        self.print_parent(parent);
                        sys::print(b": no medium; the storage interface is left unconfigured\n");
                        if let Some(st) = self.storage.take() {
                            self.pipe_free(st.bulk_in.pipe);
                            self.pipe_free(st.bulk_out.pipe);
                        }
                        return Ok(());
                    }
                    if !matches!(
                        sense.map(|s| s.key),
                        Some(scsi::Sense::UNIT_ATTENTION | scsi::Sense::NOT_READY)
                    ) {
                        return Err("TEST UNIT READY rejected");
                    }
                }
                Err(e) => return Err(self.scsi_failed(e, "TEST UNIT READY: transport failure")),
            }
            if self.now() >= ready_until {
                return Err("the device never becomes ready in time");
            }
            self.pause(NOT_READY_PAUSE_MS);
        }
        self.scsi(
            scsi::read_capacity10(),
            Some((small_phys, scsi::READ_CAPACITY_LEN as u32, true)),
            NO_MEDIUM_WINDOW_MS,
            None,
        )
        .map_err(|e| self.scsi_failed(e, "READ CAPACITY failed"))?;
        let (last_lba, block_len) =
            scsi::parse_read_capacity10(small()).ok_or("invalid READ CAPACITY")?;
        if block_len != 512 {
            return Err("blocks other than 512 bytes: not supported");
        }
        let capacity = last_lba as u64 + 1;
        // The GPT header (LBA 1) carries the disk GUID: the identity that
        // survives a driver restart (init passes it back).
        let (sector_phys, sector_virt) = self.sector_buf;
        let shortfall = self
            .scsi(
                scsi::read10(1, 1),
                Some((sector_phys, 512, true)),
                MEDIUM_WINDOW_MS,
                None,
            )
            .map_err(|e| self.scsi_failed(e, "READ(10) of the GPT header failed"))?;
        // SAFETY: the driver's 512-byte sector buffer.
        let sector = unsafe { core::slice::from_raw_parts(sector_virt as *const u8, 512) };
        if shortfall != 0 {
            // A partial read is a failed read, not a disk without a GPT:
            // the enumeration fails and is retried on the next connect.
            return Err("short read of the GPT header");
        }
        // No valid GPT header: a disk without one (the identity has no GUID).
        let disk_guid = aricode_gpt::Header::parse(sector).ok().map(|h| h.disk_guid);
        let identity = Served {
            capacity,
            vendor: inq.vendor,
            product: inq.product,
            disk_guid,
            serial,
        };
        let expected_ok = match self.expected_disk {
            None => true,
            Some(d) => disk_guid.is_some_and(|g| driver_arg::digest(&g) == d),
        };
        match self.served {
            None if expected_ok => self.served = Some(identity),
            Some(served) if served == identity => {
                self.print_parent(parent);
                sys::print(b": the served storage device is back\n");
            }
            other => {
                // Another stick must never silently become the disk the
                // clients were using (nor, after a restart, the one init
                // chose): it is left unconfigured and the block service
                // keeps answering Io until the served one returns.
                self.print_parent(parent);
                sys::print(b": a different storage device (");
                num(capacity);
                sys::print(b" sectors");
                match other {
                    Some(served) => {
                        sys::print(b", was ");
                        num(served.capacity);
                    }
                    None => sys::print(b", not the system disk served before the restart"),
                }
                sys::print(b"): not served\n");
                record("ehci other storage", capacity);
                if let Some(st) = self.storage.take() {
                    self.pipe_free(st.bulk_in.pipe);
                    self.pipe_free(st.bulk_out.pipe);
                }
                return Ok(());
            }
        }
        if let Some(st) = self.storage.as_mut() {
            st.capacity = capacity;
        }
        self.print_parent(parent);
        sys::print(b": USB storage ");
        sys::print(inq.vendor.trim_ascii());
        sys::print(b" ");
        sys::print(inq.product.trim_ascii());
        sys::print(b", ");
        num(capacity);
        sys::print(b" sectors\n");
        Ok(())
    }

    /// A SCSI command of the enumeration failed: with `strict` a transport
    /// or CSW failure halts the driver; the message is passed through.
    fn scsi_failed(&self, e: ScsiFail, why: &'static str) -> &'static str {
        // Running out of the step's time is not a device failure.
        let refused = matches!(e, ScsiFail::Transport(Fail::Refused, _));
        if matches!(e, ScsiFail::Transport(..) | ScsiFail::Csw) && !refused {
            self.strict_halt(why.as_bytes());
        }
        why
    }

    /// Forgets a device and everything below it, releasing its queue heads
    /// and address.
    fn detach(&mut self, index: usize) {
        let Some(dev) = self.devices[index] else {
            return;
        };
        for j in 0..MAX_DEVICES {
            if self.devices[j].is_some_and(|c| matches!(c.parent, Parent::Hub(h, _) if h == index))
            {
                self.detach(j);
            }
        }
        for e in self.pending.iter_mut() {
            if e.is_some_and(|(p, _)| matches!(p, Parent::Hub(h, _) if h == index)) {
                *e = None;
            }
        }
        for h in 0..MAX_HIDS {
            let Some(hid) = self.hids[h] else { continue };
            if hid.device != index {
                continue;
            }
            self.pipe_free(hid.pipe);
            // An empty report releases every key and button still held, so
            // nothing stays pressed after an unplug.
            let mut out = [Event::default(); MAX_REPORT_EVENTS];
            let mut decoder = hid.decoder;
            let n = match &mut decoder {
                Decoder::Keyboard(k) => {
                    self.keyboards -= 1;
                    k.feed(&[0; 8], &mut out)
                }
                Decoder::Mouse(m) => {
                    self.mice -= 1;
                    m.feed(&[0; 4], &mut out)
                }
            };
            let now = self.now();
            for e in &out[..n] {
                self.events.push(*e, now);
            }
            self.hids[h] = None;
        }
        if let Some(st) = self.storage.as_ref() {
            if st.device == index {
                let (bulk_in, bulk_out, served) =
                    (st.bulk_in.pipe, st.bulk_out.pipe, st.capacity > 0);
                self.pipe_free(bulk_in);
                self.pipe_free(bulk_out);
                self.storage = None;
                if served {
                    say(b"USB storage removed: block requests answer Io until it is back");
                    record("ehci storage removed", 0);
                }
            }
        }
        self.pipe_free(dev.ep0);
        if dev.address != 0 {
            self.addresses &= !(1 << dev.address);
        }
        self.devices[index] = None;
    }

    /// A connection change on a port: what was there is gone; a device
    /// present now is enumerated after the debounce.
    fn connection_changed(&mut self, parent: Parent, connected: bool) {
        if let Some(d) = self.device_on(parent) {
            self.print_parent(parent);
            sys::print(b": device disconnected\n");
            self.detach(d);
        }
        if connected {
            let at = self.now() + DEBOUNCE_MS;
            self.push_pending(parent, at);
        }
    }

    /// Port work of one main loop iteration: controller errors, root port
    /// changes, the hub poll and at most one enumeration that is due.
    fn service_ports(&mut self) {
        let sts = self.op.r32(op::USBSTS);
        // A host system error halts the controller (4.15.2.4); a halt for
        // any other reason is just as fatal: nothing runs any more.
        if sts & (op::STS_HOST_SYSTEM_ERROR | op::STS_HALTED) != 0 {
            say(if sts & op::STS_HOST_SYSTEM_ERROR != 0 {
                b"host system error: the controller halted (EHCI 4.15.2.4)"
            } else {
                b"the controller halted unexpectedly (USBSTS.HCHalted)"
            });
            record("ehci controller halted", sts as u64);
            self.dump();
            sys::process_exit(exit::HOST_SYSTEM_ERROR);
        }
        if sts & op::STS_PORT_CHANGE != 0 {
            self.op.w32(op::USBSTS, op::STS_PORT_CHANGE);
        }
        for port in 1..=self.params.ports {
            let v = self.portsc(port);
            if v & portsc::CHANGE_BITS == 0 || self.is_pending(Parent::Root(port)) {
                continue;
            }
            self.write_portsc(port, portsc::write_value(v, v & portsc::CHANGE_BITS, 0));
            if v & portsc::OVER_CURRENT_CHANGE != 0 {
                self.print_parent(Parent::Root(port));
                sys::print(b": over-current change\n");
                record("ehci over-current", port as u64);
            }
            if v & portsc::CONNECT_CHANGE != 0 && v & portsc::OWNER == 0 {
                self.connection_changed(Parent::Root(port), v & portsc::CONNECTED != 0);
            }
        }
        let now = self.now();
        if now >= self.next_hub_poll {
            self.release_info();
            self.with_deadline(HUB_POLL_STEP_MS, Self::poll_hubs);
            self.next_hub_poll = self.now() + HUB_POLL_MS;
        }
        let due = self
            .pending
            .iter()
            .position(|e| e.is_some_and(|(_, at)| at <= now));
        if let Some(k) = due {
            if let Some((parent, _)) = self.pending[k].take() {
                self.release_info();
                self.with_deadline(ATTACH_STEP_MS, |c| c.attach(parent));
            }
        }
        let boot_over = self.now() >= self.started + BOOT_ENUMERATION_MS;
        if !self.boot_done && (self.pending.iter().all(|e| e.is_none()) || boot_over) {
            if boot_over {
                say(b"boot enumeration took too long; the ports still pending come later");
                record("ehci boot enumeration", 0);
            }
            self.boot_done = true;
            sys::print(b"usb-ehci: device ready: ");
            sys::print(device_name(self).as_bytes());
            sys::print(b"\n");
            if self.storage.as_ref().is_none_or(|s| s.capacity == 0) {
                say(b"no USB storage");
            }
        }
    }

    /// GetPortStatus on every hub port (USB 2.0 11.24.2.7); changes are
    /// acknowledged and acted on. A hub that fails a request is reported
    /// once and no longer polled.
    fn poll_hubs(&mut self) {
        for h in 0..MAX_DEVICES {
            let Some(state) = self.devices[h].and_then(|d| d.hub) else {
                continue;
            };
            if state.failed {
                continue;
            }
            for port in 1..=state.ports {
                if self.devices[h].is_none() {
                    break;
                }
                if self.is_pending(Parent::Hub(h, port)) {
                    continue;
                }
                let result = self
                    .hub_port_status(h, port)
                    .and_then(|st| self.hub_clear_changes(h, port, &st).map(|_| st));
                let st = match result {
                    Ok(st) => st,
                    // The poll's time ran out: the next poll goes on.
                    Err(_) if self.deadline.is_some_and(|d| self.now() >= d) => return,
                    Err(why) => {
                        self.print_parent(Parent::Hub(h, port));
                        sys::print(b": ");
                        sys::print(why.as_bytes());
                        sys::print(b"; the hub's ports are no longer watched\n");
                        if let Some(HubState { failed, .. }) =
                            self.devices[h].as_mut().and_then(|d| d.hub.as_mut())
                        {
                            *failed = true;
                        }
                        break;
                    }
                };
                if st.over_current_changed() {
                    self.print_parent(Parent::Hub(h, port));
                    sys::print(b": over-current change\n");
                    record("ehci hub over-current", port as u64);
                }
                if st.connection_changed() {
                    self.connection_changed(Parent::Hub(h, port), st.connected());
                }
            }
        }
    }

    /// Whether a device is still attached to its port.
    fn port_connected(&mut self, parent: Parent) -> bool {
        match parent {
            Parent::Root(port) => self.portsc(port) & portsc::CONNECTED != 0,
            // A hub that does not answer says nothing about the port: the
            // failure stays a failure.
            Parent::Hub(h, port) => self
                .hub_port_status(h, port)
                .map_or(true, |s| s.connected()),
        }
    }

    // ---- HID ----

    /// Gives an interrupt queue head its report qTD.
    fn arm(&mut self, h: usize) {
        let Some(hid) = self.hids[h] else { return };
        let step = Step {
            pid: Pid::In,
            buffer: hid.report.0,
            bytes: hid.len as u32,
            toggle: false,
            on_short: OnShort::Stop,
        };
        if self.submit(hid.pipe, &[step]).is_err() {
            say(b"HID report buffer not addressable");
            if let Some(x) = self.hids[h].as_mut() {
                x.stopped = true;
            }
        }
    }

    /// Collects finished reports into `pending` and rearms their qTDs.
    fn poll_hids(&mut self) {
        let now = self.now();
        let mut out = [Event::default(); MAX_REPORT_EVENTS];
        for h in 0..MAX_HIDS {
            let Some(mut hid) = self.hids[h] else {
                continue;
            };
            if hid.stopped {
                continue;
            }
            if hid.clear_owed {
                self.clear_hid_halt(h);
                continue;
            }
            let t = r32(self.qtd_addr(hid.pipe, 0).1 + 8);
            match decode_token(t) {
                Outcome::Active => continue,
                Outcome::Done { remaining } => {
                    let got = (hid.len as u32).saturating_sub(remaining) as usize;
                    if !hid.seen {
                        hid.seen = true;
                        sys::print(b"usb-ehci: first report from address ");
                        num(self.pipes[hid.pipe].desc.address as u64);
                        sys::print(b" (");
                        num(got as u64);
                        sys::print(b" bytes)\n");
                    }
                    hid.stall_cleared = false;
                    // SAFETY: the driver's 64-byte report buffer.
                    let report = unsafe {
                        core::slice::from_raw_parts(hid.report.1 as *const u8, got.min(64))
                    };
                    let n = match &mut hid.decoder {
                        Decoder::Keyboard(k) => k.feed(report, &mut out),
                        Decoder::Mouse(m) => m.feed(report, &mut out),
                    };
                    for e in &out[..n] {
                        self.events.push(*e, now);
                    }
                    self.hids[h] = Some(hid);
                    self.arm(h);
                }
                Outcome::Error(e) => {
                    let fail = Fail::Error(e, t);
                    // A device being unplugged fails its transfers before
                    // the port change is seen: that is the disconnect,
                    // which the port change reports, not a fault.
                    if self.unplugged(hid.pipe) {
                        hid.stopped = true;
                        self.hids[h] = Some(hid);
                        continue;
                    }
                    self.report_failure("intr", hid.pipe, fail);
                    self.strict_halt(b"interrupt transfer");
                    self.overlay_idle(hid.pipe, !fail.is_stall());
                    // USB 2.0 9.4.5: a halted endpoint is cleared with
                    // CLEAR_FEATURE(ENDPOINT_HALT), once per stall.
                    if fail.is_stall() && !hid.stall_cleared {
                        hid.clear_owed = true;
                        self.hids[h] = Some(hid);
                        self.clear_hid_halt(h);
                    } else {
                        self.stop_hid(h);
                    }
                }
            }
        }
    }

    /// Sends the owed CLEAR_FEATURE(ENDPOINT_HALT) of HID `h` and rearms
    /// it. Refused for lack of time, it stays owed for the next poll; any
    /// other failure stops the endpoint until the device is plugged in
    /// again.
    fn clear_hid_halt(&mut self, h: usize) {
        let Some(hid) = self.hids[h] else { return };
        let Some(ep0) = self.devices[hid.device].map(|d| d.ep0) else {
            return;
        };
        let request = SetupPacket::clear_endpoint_halt(0x80 | hid.endpoint);
        match self.control(ep0, request, None, false) {
            Ok(_) => {
                if let Some(x) = self.hids[h].as_mut() {
                    x.clear_owed = false;
                    x.stall_cleared = true;
                }
                self.arm(h);
            }
            Err(Fail::Refused) => {}
            Err(_) => self.stop_hid(h),
        }
    }

    fn stop_hid(&mut self, h: usize) {
        if let Some(x) = self.hids[h].as_mut() {
            x.stopped = true;
            x.clear_owed = false;
        }
        say(b"HID endpoint stopped until the device is plugged in again");
    }

    // ---- storage (Bulk-Only Transport) ----

    /// One SCSI command over the bulk-only transport: CBW, data (as
    /// `(physical, bytes, towards the host)`) and CSW (BOT 5.3). A failed
    /// transfer goes through `transfer_failure`; a CSW that does not close
    /// the command asks for a Reset Recovery (BOT 6.3, 6.7). A passed
    /// command returns the bytes it did not move.
    fn scsi(
        &mut self,
        cdb: Cdb,
        data: Option<(u64, u32, bool)>,
        window_ms: u64,
        inject: Option<Injection>,
    ) -> Result<u32, ScsiFail> {
        // BOT 5.3.4: no CBW while a Reset Recovery is owed.
        if self
            .storage
            .as_ref()
            .is_some_and(|st| st.needs_reset && !st.broken)
        {
            self.reset_recovery();
        }
        let (tag, cbw, csw, bulk_in, bulk_out) = {
            let st = self.storage.as_mut().ok_or(ScsiFail::Broken)?;
            if st.broken || st.needs_reset {
                return Err(ScsiFail::Broken);
            }
            st.tag = st.tag.wrapping_add(1);
            (st.tag, st.cbw, st.csw, st.bulk_in.pipe, st.bulk_out.pipe)
        };
        let window_end = self.now() + CBW_TIMEOUT_MS + window_ms;
        let wrapper = Cbw {
            tag,
            data_len: data.map_or(0, |d| d.1),
            data_in: data.is_some_and(|d| d.2),
            lun: 0,
            cdb,
        };
        // SAFETY: the CBW buffer in the driver's DMA memory.
        unsafe { (cbw.1 as *mut [u8; Cbw::LEN]).write_volatile(wrapper.encode()) };
        match inject {
            Some(Injection::Cbw) => {
                say(b"injected fault: transaction error reported on the CBW");
                let fail = Fail::Error(TransferError::Transaction, 0);
                return Err(self.transfer_failure(Stage::Cbw, false, fail, tag, window_end));
            }
            Some(Injection::Csw) => {
                // No CBW: the device has no command and answers the CSW read
                // with a STALL (BOT 6.7.2 path on a real halt).
                say(b"injected fault: reading a CSW with no command outstanding");
                return Err(
                    match self.bulk(bulk_in, true, csw.0, Csw::LEN as u32, window_end) {
                        Ok(_) => {
                            say(b"the device delivered a CSW for no command");
                            if !self.strict {
                                self.reset_recovery();
                            }
                            ScsiFail::Csw
                        }
                        Err(fail) => self.transfer_failure(Stage::Csw, true, fail, tag, window_end),
                    },
                );
            }
            None => {}
        }
        let deadline = self.now() + CBW_TIMEOUT_MS;
        if let Err(fail) = self.bulk(bulk_out, false, cbw.0, Cbw::LEN as u32, deadline) {
            return Err(self.transfer_failure(Stage::Cbw, false, fail, tag, window_end));
        }
        let mut moved = 0;
        if let Some((phys, len, dir_in)) = data {
            let pipe = if dir_in { bulk_in } else { bulk_out };
            match self.bulk(pipe, dir_in, phys, len, window_end) {
                Ok(n) => moved = n,
                Err(fail) => {
                    return Err(self.transfer_failure(Stage::Data, dir_in, fail, tag, window_end))
                }
            }
        }
        if let Err(fail) = self.bulk(bulk_in, true, csw.0, Csw::LEN as u32, window_end) {
            return Err(self.transfer_failure(Stage::Csw, true, fail, tag, window_end));
        }
        // SAFETY: the CSW buffer in the driver's DMA memory.
        let raw = unsafe { (csw.1 as *const [u8; Csw::LEN]).read_volatile() };
        match check_csw(&raw, tag) {
            // The bytes the command did not move: the CSW's residue (BOT
            // 6.7) or what the data stage fell short of, whichever is more.
            Ok(Csw {
                status: CswStatus::Passed,
                residue,
                ..
            }) => Ok(residue.max(data.map_or(0, |d| d.1).saturating_sub(moved))),
            Ok(_) => Err(ScsiFail::Failed),
            Err(e) => {
                report_csw_error(e, tag);
                record("ehci csw invalid", 0);
                if !self.strict {
                    self.reset_recovery();
                }
                Err(ScsiFail::Csw)
            }
        }
    }

    /// A bulk transfer at `stage` failed: it is reported; a STALL is
    /// cleared (USB 2.0 9.4.5, BOT 6.7); then the Bulk-Only rule for the
    /// stage applies (BOT 5.3, 6.7.2): after a CBW failure a Reset
    /// Recovery, after a DATA or CSW failure one CSW read, which closes the
    /// command if valid, else a Reset Recovery. Returns the failure.
    fn transfer_failure(
        &mut self,
        stage: Stage,
        dir_in: bool,
        fail: Fail,
        tag: u32,
        window_end: u64,
    ) -> ScsiFail {
        let Some((pipe, csw, bulk_in)) = self.storage.as_ref().map(|st| {
            (
                if dir_in {
                    st.bulk_in.pipe
                } else {
                    st.bulk_out.pipe
                },
                st.csw,
                st.bulk_in.pipe,
            )
        }) else {
            return ScsiFail::Transport(fail, None);
        };
        if fail == Fail::Refused {
            // Nothing reached the controller. Once the CBW went out the
            // command is outstanding: only a Reset Recovery closes it.
            say(b"no time left in this request: the stage was not started");
            record("ehci no time", stage as u64);
            if stage != Stage::Cbw {
                if let Some(st) = self.storage.as_mut() {
                    st.needs_reset = true;
                }
            }
            return ScsiFail::Transport(fail, None);
        }
        if fail != Fail::Lost && self.unplugged(pipe) {
            self.overlay_idle(pipe, true);
            return ScsiFail::Gone;
        }
        sys::print(b"usb-ehci: ");
        sys::print(stage.name().as_bytes());
        sys::print(b" stage of tag ");
        num(tag as u64);
        sys::print(b" failed\n");
        self.report_failure(if dir_in { "bulk in" } else { "bulk out" }, pipe, fail);
        if fail == Fail::Lost {
            self.storage_broken();
            return ScsiFail::Transport(fail, None);
        }
        // `strict`: no recovery; the caller records the request and halts
        // with the queue head as the failure left it.
        if self.strict {
            return ScsiFail::Transport(fail, None);
        }
        self.overlay_idle(pipe, true);
        self.clear_tt(pipe, fail, hub::TT_BULK, dir_in);
        if fail.is_stall() {
            if let Err(e) = self.clear_bulk_halt(dir_in) {
                self.recovery_failed(e);
                return ScsiFail::Transport(fail, None);
            }
        }
        let mut sense = None;
        match after_transfer_failure(stage) {
            Next::ResetRecovery => self.reset_recovery(),
            Next::ReadCsw => {
                let deadline = window_end.max(self.now() + MIN_STAGE_MS);
                match self.bulk(bulk_in, true, csw.0, Csw::LEN as u32, deadline) {
                    Ok(_) => {
                        // SAFETY: the CSW buffer in the driver's DMA memory.
                        let raw = unsafe { (csw.1 as *const [u8; Csw::LEN]).read_volatile() };
                        match check_csw(&raw, tag) {
                            Ok(c) => {
                                sys::print(b"usb-ehci: CSW received after the failure: status ");
                                num(c.status as u64);
                                sys::print(b" residue ");
                                num(c.residue as u64);
                                sys::print(b"; transport ready\n");
                                if c.status == CswStatus::Failed {
                                    sense = self.sense();
                                }
                            }
                            Err(e) => {
                                report_csw_error(e, tag);
                                self.reset_recovery();
                            }
                        }
                    }
                    Err(f2) => {
                        self.report_failure("bulk in", bulk_in, f2);
                        match f2 {
                            Fail::Lost => self.storage_broken(),
                            Fail::Refused => self.recovery_failed(f2),
                            _ => {
                                self.overlay_idle(bulk_in, true);
                                self.clear_tt(bulk_in, f2, hub::TT_BULK, true);
                                // BOT 6.7.2: the second CSW read failed too;
                                // a STALL is cleared, then Reset Recovery.
                                let cleared = if f2.is_stall() {
                                    self.clear_bulk_halt(true)
                                } else {
                                    Ok(())
                                };
                                match cleared {
                                    Ok(()) => self.reset_recovery(),
                                    Err(e) => self.recovery_failed(e),
                                }
                            }
                        }
                    }
                }
            }
        }
        ScsiFail::Transport(fail, sense)
    }

    /// `CLEAR_FEATURE(ENDPOINT_HALT)` on a bulk endpoint; its queue head
    /// restarts at DATA0 (USB 2.0 9.4.5).
    fn clear_bulk_halt(&mut self, dir_in: bool) -> Result<(), Fail> {
        let Some((ep0, bulk)) = self.storage.as_ref().and_then(|st| {
            let b = if dir_in { st.bulk_in } else { st.bulk_out };
            Some((self.devices[st.device]?.ep0, b))
        }) else {
            return Err(Fail::Lost);
        };
        self.control(
            ep0,
            SetupPacket::clear_endpoint_halt(bulk.address),
            None,
            false,
        )?;
        self.overlay_idle(bulk.pipe, false);
        Ok(())
    }

    /// A recovery step did not complete: for lack of time the Reset
    /// Recovery is owed to the next command; otherwise the storage is
    /// unusable until it is plugged in again.
    fn recovery_failed(&mut self, fail: Fail) {
        if fail == Fail::Refused {
            if let Some(st) = self.storage.as_mut() {
                st.needs_reset = true;
            }
            say(b"Reset Recovery postponed (no time left): it runs before the next command");
        } else {
            self.storage_broken();
        }
    }

    /// Reset Recovery (BOT 5.3.4): Bulk-Only Mass Storage Reset, then both
    /// bulk halts cleared. If it fails the storage is unusable until it is
    /// plugged in again.
    fn reset_recovery(&mut self) {
        let Some((ep0, interface)) = self
            .storage
            .as_ref()
            .and_then(|st| Some((self.devices[st.device]?.ep0, st.interface)))
        else {
            return;
        };
        say(b"Reset Recovery (BOT 5.3.4): mass storage reset, both bulk halts cleared");
        record("ehci reset recovery", 0);
        let result = self
            .control(ep0, SetupPacket::mass_storage_reset(interface), None, false)
            .map(|_| ())
            .and_then(|()| self.clear_bulk_halt(true))
            .and_then(|()| self.clear_bulk_halt(false));
        match result {
            Ok(()) => {
                if let Some(st) = self.storage.as_mut() {
                    st.needs_reset = false;
                }
                say(b"Reset Recovery done; transport ready");
            }
            Err(e) => self.recovery_failed(e),
        }
    }

    fn storage_broken(&mut self) {
        if let Some(st) = self.storage.as_mut() {
            st.broken = true;
        }
        say(b"USB storage unusable until it is plugged in again");
        record("ehci storage lost", 0);
    }

    /// REQUEST SENSE (SPC-4): the device's reason for a failed command.
    fn sense(&mut self) -> Option<scsi::Sense> {
        if self.in_sense {
            return None;
        }
        let (phys, virt) = self.storage.as_ref()?.small;
        self.in_sense = true;
        let result = self.scsi(
            scsi::request_sense(),
            Some((phys, scsi::SENSE_LEN as u32, true)),
            NO_MEDIUM_WINDOW_MS,
            None,
        );
        self.in_sense = false;
        if let Err(e) = result {
            self.scsi_failed(e, "REQUEST SENSE failed");
            return None;
        }
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        scsi::Sense::parse(unsafe { core::slice::from_raw_parts(virt as *const u8, 64) })
    }

    /// Reads or writes `count` sectors in the client's DMA buffer. A unit
    /// attention is retried once while the client's wait allows it (SPC-4
    /// unit attention condition: it reports a past event, the command
    /// itself was not run). Transport failures are recorded with
    /// the LBA and not retried: the Bulk-Only recovery has run and the
    /// request answers `Io`.
    fn block_io(&mut self, write: bool, lba: u64, count: u16, phys: u64) -> bool {
        self.with_deadline(REQUEST_BUDGET_MS, |c| {
            c.block_io_within(write, lba, count, phys)
        })
    }

    fn block_io_within(&mut self, write: bool, lba: u64, count: u16, phys: u64) -> bool {
        let Ok(lba32) = u32::try_from(lba) else {
            return false;
        };
        let cdb = if write {
            scsi::write10(lba32, count)
        } else {
            scsi::read10(lba32, count)
        };
        let name: &[u8] = if write { b"WRITE(10)" } else { b"READ(10)" };
        let now = self.now();
        let mut inject = self.fault_inject.due(now, self.last_request_ms);
        self.last_request_ms = now;
        let mut unit_attention_retried = false;
        loop {
            let injected = inject.is_some();
            let data = Some((phys, count as u32 * 512, !write));
            match self.scsi(cdb, data, MEDIUM_WINDOW_MS, inject) {
                Ok(0) => return true,
                Ok(short) => {
                    // Part of the sectors were not moved: the client's
                    // buffer would hold stale bytes (BOT 6.7.2, 6.7.3).
                    sys::print(b"usb-ehci: ");
                    sys::print(name);
                    sys::print(b" lba ");
                    num(lba);
                    sys::print(b": ");
                    num(short as u64);
                    sys::print(b" bytes not transferred\n");
                    record("ehci short data", short as u64);
                    self.strict_halt(name);
                    return false;
                }
                Err(ScsiFail::Failed) => {
                    let sense = self.sense();
                    if let Some(sense) = sense {
                        record_sense(&sense);
                    }
                    // The retry needs a whole command window inside the
                    // request's deadline.
                    let time_left = self
                        .deadline
                        .is_some_and(|d| self.now() + CBW_TIMEOUT_MS + MEDIUM_WINDOW_MS <= d);
                    if sense.map(|s| s.key) == Some(scsi::Sense::UNIT_ATTENTION)
                        && !unit_attention_retried
                        && time_left
                    {
                        unit_attention_retried = true;
                        inject = None;
                        continue;
                    }
                    self.report_error(name, lba, sense);
                    return false;
                }
                Err(ScsiFail::Broken | ScsiFail::Gone) => {
                    self.report_error(name, lba, None);
                    return false;
                }
                Err(ScsiFail::Csw) => {
                    self.strict_halt(b"invalid CSW");
                    self.report_error(name, lba, None);
                    return false;
                }
                Err(ScsiFail::Transport(fail, sense)) => {
                    let prefix = match (write, injected) {
                        (false, false) => fault::READ,
                        (true, false) => fault::WRITE,
                        (false, true) => fault::INJECTED_READ,
                        (true, true) => fault::INJECTED_WRITE,
                    };
                    let mut buf = [0u8; fault::NAME_LEN];
                    if let Some(text) = fault::lba_name(prefix, lba, &mut buf) {
                        record(text, fail.code());
                    }
                    if let Some(sense) = sense {
                        sys::print(b"usb-ehci: ");
                        sys::print(name);
                        sys::print(b" lba ");
                        num(lba);
                        sys::print(b": the device reports the command failed, sense ");
                        print_sense(&sense);
                        sys::print(b"\n");
                        record_sense(&sense);
                    }
                    if fail == Fail::Lost {
                        self.buffer_unsafe = true;
                    }
                    // Running out of the request's time is not a device
                    // failure; everything else halts under `strict`.
                    if fail != Fail::Refused {
                        self.strict_halt(name);
                    }
                    self.report_error(name, lba, None);
                    return false;
                }
            }
        }
    }

    /// Makes writes durable (`SYNCHRONIZE CACHE`). A device that does not
    /// implement the command has no cache to write back (ADR 0023).
    fn flush(&mut self) -> bool {
        self.with_deadline(REQUEST_BUDGET_MS, Self::flush_within)
    }

    fn flush_within(&mut self) -> bool {
        if !self.storage.as_ref().is_some_and(|s| s.sync_cache) {
            return self.storage.is_some();
        }
        match self.scsi(scsi::synchronize_cache10(), None, MEDIUM_WINDOW_MS, None) {
            Ok(_) => true,
            Err(ScsiFail::Failed) => {
                let sense = self.sense();
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
            Err(ScsiFail::Broken | ScsiFail::Gone) => {
                self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                false
            }
            Err(fail) => {
                if let ScsiFail::Transport(f, _) = fail {
                    record("ehci flush fail", f.code());
                    if f == Fail::Refused {
                        self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                        return false;
                    }
                }
                self.strict_halt(b"SYNCHRONIZE CACHE");
                self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                false
            }
        }
    }

    /// The first few failed block requests, with the device's sense.
    fn report_error(&mut self, what: &[u8], lba: u64, sense: Option<scsi::Sense>) {
        let Some(st) = self.storage.as_mut() else {
            return;
        };
        if st.errors_reported >= 4 {
            return;
        }
        st.errors_reported += 1;
        let broken = st.broken;
        sys::print(b"usb-ehci: ");
        sys::print(what);
        sys::print(b" lba ");
        num(lba);
        match sense {
            Some(s) => {
                sys::print(b" rejected: sense ");
                print_sense(&s);
                sys::print(b"\n");
            }
            None if broken => sys::print(b" refused: the storage is unusable\n"),
            None => sys::print(b" transport failure\n"),
        }
    }

    // ---- strict mode ----

    /// `strict` (ADR 0028): dump the controller and stop; init does not
    /// restart the driver.
    fn strict_halt(&self, what: &[u8]) {
        if !self.strict {
            return;
        }
        sys::print(b"usb-ehci: strict: halting on a transfer failure (");
        sys::print(what);
        sys::print(b")\n");
        self.dump();
        sys::process_exit(exit::TRANSPORT_FAULT);
    }

    fn dump(&self) {
        sys::print(b"  USBCMD ");
        hex(self.op.r32(op::USBCMD) as u64);
        sys::print(b" USBSTS ");
        hex(self.op.r32(op::USBSTS) as u64);
        sys::print(b" FRINDEX ");
        hex(self.op.r32(op::FRINDEX) as u64);
        sys::print(b" HCIVERSION ");
        hex4(self.version as u64);
        sys::print(b"\n");
        for port in 1..=self.params.ports {
            let v = self.portsc(port);
            let mut text = [0u8; 96];
            let n = portsc::describe(v, &mut text);
            sys::print(b"  port ");
            num(port as u64);
            sys::print(b" PORTSC ");
            hex(v as u64);
            sys::print(b" ");
            sys::print(&text[..n]);
            sys::print(b"\n");
        }
        for (p, pipe) in self.pipes.iter().enumerate() {
            if !pipe.in_use {
                continue;
            }
            let qh = pipe.qh.1;
            sys::print(b"  qh ");
            num(p as u64);
            sys::print(b" address ");
            num(pipe.desc.address as u64);
            sys::print(b" endpoint ");
            num(pipe.desc.endpoint as u64);
            sys::print(b" chars ");
            hex(r32(qh + 4) as u64);
            sys::print(b" caps ");
            hex(r32(qh + 8) as u64);
            sys::print(b" current ");
            hex(r32(qh + 12) as u64);
            sys::print(b" next ");
            hex(r32(qh + QueueHead::NEXT_QTD) as u64);
            sys::print(b" token ");
            hex(r32(qh + QueueHead::TOKEN) as u64);
            sys::print(b" qtds");
            for i in 0..QTDS_PER_PIPE {
                sys::print(b" ");
                hex(r32(self.qtd_addr(p, i).1 + 8) as u64);
            }
            sys::print(b"\n");
        }
    }
}

/// `CSW` failures are printed with what was received (BOT 6.3).
fn report_csw_error(e: CswError, tag: u32) {
    match e {
        CswError::Invalid => say(b"invalid CSW (signature, length or status)"),
        CswError::TagMismatch(csw) => {
            sys::print(b"usb-ehci: CSW tag mismatch: expected ");
            num(tag as u64);
            sys::print(b", received ");
            num(csw.tag as u64);
            sys::print(b"\n");
        }
        CswError::PhaseError(csw) => {
            sys::print(b"usb-ehci: CSW phase error, tag ");
            num(csw.tag as u64);
            sys::print(b" residue ");
            num(csw.residue as u64);
            sys::print(b"\n");
        }
    }
}

/// Firmware handoff (EHCI 5.1, USBLEGSUP in configuration space): the OS
/// Owned semaphore is set, the firmware gets `HANDOFF_TIMEOUT_MS` to clear
/// its own, and every legacy SMI source is turned off. False (reported and
/// recorded) if the firmware kept the controller.
fn bios_handoff(cfg: &Mmio, eecp: u8, clock: &Clock) -> bool {
    let Some(off) = legacy::find(eecp, |o| cfg.r32(o as u64)) else {
        return true;
    };
    let off = off as u64;
    let mut released = true;
    if cfg.r32(off) & legacy::OS_OWNED == 0 {
        // HC OS Owned Semaphore: bit 24, the byte at offset 3.
        cfg.w8(off + 3, 1);
    }
    if cfg.r32(off) & legacy::BIOS_OWNED != 0 {
        let deadline = clock.now() + HANDOFF_TIMEOUT_MS;
        while cfg.r32(off) & legacy::BIOS_OWNED != 0 && clock.now() < deadline {
            sys::thread_sleep(5);
        }
        released = cfg.r32(off) & legacy::BIOS_OWNED == 0;
        say(if released {
            b"the firmware released the EHCI"
        } else {
            b"the firmware does not release the EHCI (EHCI 5.1); continuing"
        });
        if !released {
            record("ehci bios handoff", cfg.r32(off) as u64);
        }
    }
    let ctl = cfg.r32(off + 4);
    cfg.w32(off + 4, legacy::smis_disabled(ctl));
    released
}

/// Bounded wait for `reg & mask` to be set (or clear).
fn wait_reg(mmio: &Mmio, reg: u64, mask: u32, set: bool, clock: &Clock, timeout_ms: u64) -> bool {
    let deadline = clock.now() + timeout_ms;
    loop {
        if (mmio.r32(reg) & mask != 0) == set {
            return true;
        }
        if clock.now() >= deadline {
            return false;
        }
        sys::thread_yield();
    }
}

/// One block protocol request on the USB storage.
fn serve_block(ctl: &mut Ehci, msg: &[u8], handles: &[Handle]) {
    let reply_to = handles[0];
    let capacity = ctl.storage.as_ref().map_or(0, |s| s.capacity);
    let invalid = BlockReply {
        status: BlockStatus::Invalid,
        value: 0,
    };
    let io = BlockReply {
        status: BlockStatus::Io,
        value: 0,
    };
    let reply = match BlockRequest::decode(msg) {
        None => invalid,
        // The served storage is absent (unplugged, or replaced by another
        // device that is not served): its clients get Io, never another
        // disk's data (audit ehci-1, H1).
        Some(_) if capacity == 0 && (ctl.served.is_some() || ctl.expected_disk.is_some()) => io,
        Some(_) if capacity == 0 => invalid,
        Some(BlockRequest {
            op: BlockOp::Info, ..
        }) => BlockReply {
            status: BlockStatus::Ok,
            value: capacity,
        },
        Some(BlockRequest {
            op: BlockOp::Flush, ..
        }) => {
            if ctl.flush() {
                BlockReply {
                    status: BlockStatus::Ok,
                    value: 0,
                }
            } else {
                io
            }
        }
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
                match map_client_buffer(handles[1]) {
                    Some((phys, len)) if len >= count as u64 * 512 => {
                        if ctl.block_io(op == BlockOp::Write, lba, count, phys) {
                            BlockReply {
                                status: BlockStatus::Ok,
                                value: count as u64,
                            }
                        } else {
                            io
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
    // A buffer the controller may still own (a queue head it never
    // released) is kept for good, so its memory is never reused.
    if handles.len() >= 2 {
        if ctl.buffer_unsafe {
            say(b"client buffer kept: the controller may still own it");
            ctl.buffer_unsafe = false;
        } else {
            let _ = sys::handle_close(handles[1]);
        }
    }
    if sys::ipc_send(reply_to, &reply.encode(), &[], 0).is_err() {
        say(b"block reply undelivered: the client stopped waiting");
        record("ehci reply undelivered", reply.status as u64);
    }
    let _ = sys::handle_close(reply_to);
}

/// Receive errors that mean the endpoint itself is gone (closed or revoked
/// by init, ADR 0030) rather than a bad message.
fn endpoint_gone(e: sys::Error) -> bool {
    e.endpoint_gone() || e == sys::Error::BadHandle
}

fn answer_info(ctl: &Ehci, reply_to: Handle) {
    let reply = Reply::status(Status::Ok)
        .with_name(device_name(ctl))
        .unwrap_or(Reply::status(Status::Ok));
    answer(reply_to, reply);
}

fn answer(reply_to: Handle, reply: Reply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

fn device_name(ctl: &Ehci) -> &'static str {
    match (ctl.keyboards > 0, ctl.mice > 0) {
        (true, true) => "EHCI HID Keyboard+Mouse",
        (true, false) => "EHCI HID Keyboard",
        (false, true) => "EHCI HID Mouse",
        (false, false) => "EHCI HID (no devices)",
    }
}

fn main(mask: u64) -> u64 {
    let clock = Clock(extra_handle(mask, 0));
    let started = clock.now();
    let mut args = [0u8; 256];
    let (strict, fault_inject) = match sys::cmdline(clock.0, &mut args) {
        Some(text) => {
            let c = aricode_cmdline::CmdLine::new(text);
            (
                c.has(aricode_cmdline::keys::STRICT),
                FaultInject::parse(c.get(aricode_cmdline::keys::USB_FAULT)),
            )
        }
        None => (false, FaultInject::None),
    };
    let mut ctl = match Ehci::open(mask, clock, strict, started) {
        Ok(c) => c,
        Err(exit::NOT_EHCI) => {
            say(b"the controller is not EHCI 1.0; skipping");
            return 0;
        }
        Err(step) => {
            sys::print(b"usb-ehci: EHCI initialization failed: ");
            sys::print(match step {
                exit::MAP_BAR => b"capabilities or BAR mapping\n".as_slice(),
                exit::DMA => b"DMA memory\n",
                exit::RESET_TIMEOUT => b"the controller does not halt or reset\n",
                exit::START_TIMEOUT => b"the controller does not start\n",
                _ => b"unknown step\n",
            });
            return step;
        }
    };
    ctl.fault_inject = fault_inject;
    ctl.expected_disk = driver_arg::expected_digest(mask);
    if ctl.expected_disk.is_some() {
        say(b"restarted: only the system disk served before is served again");
    }
    if strict {
        say(b"strict mode: the first transfer failure halts the driver");
    }
    sys::print(b"usb-ehci: EHCI ready (HCIVERSION ");
    hex4(ctl.version as u64);
    sys::print(b", ");
    num(ctl.params.ports as u64);
    sys::print(b" ports, ");
    num(ctl.params.companions as u64);
    sys::print(b" companion controllers, ");
    sys::print(if ctl.ext64 {
        b"64-bit".as_slice()
    } else {
        b"32-bit"
    });
    sys::print(b" addressing, polled)\n");

    // Second extra (init passes it): the block service endpoint (ADR
    // 0023). Without it that handle number may be one of the driver's own,
    // so its type is checked.
    let block_ep = extra_handle(mask, 1);
    let has_block = sys::object_kind(block_ep) == Ok(abi::kind::ENDPOINT);
    let mut waiting: Option<(Handle, u64)> = None;
    let mut msg = [0u8; 32];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    let mut events = [WireEvent::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    let with_block = [SERVICE, block_ep];
    loop {
        // The block service waits for the boot enumeration: init asks the
        // capacity once, right after launching the driver.
        let endpoints: &[Handle] = if ctl.boot_done && has_block {
            &with_block
        } else {
            &with_block[..1]
        };
        let mut timeout = POLL_MS;
        // At most eight messages per iteration, served in one batch: a
        // block request and the one queued behind it run back to back, and
        // housekeeping (`HOUSEKEEPING_MS`) runs once per batch (budget
        // chain at `REQUEST_BUDGET_MS`).
        for _ in 0..8 {
            let (which, n, nh) =
                match sys::ipc_receive_any(endpoints, &mut msg, &mut handles, timeout) {
                    Ok(r) => r,
                    Err(sys::Error::Timeout) => break,
                    // The service endpoints are gone (init retired them):
                    // nobody can be served any more, so the driver ends.
                    Err(e) if endpoint_gone(e) => {
                        aricode_rt::print_error(b"usb-ehci: service endpoint closed; exiting", e);
                        return 0;
                    }
                    Err(e) => {
                        aricode_rt::print_error(b"usb-ehci: receive", e);
                        break;
                    }
                };
            timeout = 0;
            if nh == 0 {
                continue;
            }
            if which == 1 {
                serve_block(&mut ctl, &msg[..n], &handles[..nh]);
                continue;
            }
            let reply_to = handles[0];
            match Request::decode(&msg[..n]) {
                // Held until the boot enumeration is over (or `INFO_WAIT_MS`
                // passed), so the device name is the final one.
                Some(Request { op: Op::Info, .. }) => ctl.hold_info(reply_to),
                Some(Request {
                    op: Op::Read,
                    timeout_ms,
                }) => {
                    if ctl.events.len > 0 {
                        let n = ctl.events.take(&mut events, &mut times);
                        answer(reply_to, Reply::events(&events[..n], &times[..n]));
                    } else if waiting.is_some() {
                        answer(reply_to, Reply::status(Status::Busy));
                    } else {
                        waiting = Some((reply_to, ctl.now() + timeout_ms.max(1) as u64));
                    }
                }
                None => answer(reply_to, Reply::status(Status::Invalid)),
            }
        }
        // One deadline for the whole housekeeping of this iteration; the
        // hub poll and an enumeration inside it nest by their minimum.
        ctl.with_deadline(HOUSEKEEPING_MS, |c| {
            c.poll_hids();
            c.service_ports();
        });
        ctl.release_info();
        if let Some((reply_to, deadline)) = waiting {
            if ctl.events.len > 0 {
                let n = ctl.events.take(&mut events, &mut times);
                answer(reply_to, Reply::events(&events[..n], &times[..n]));
                waiting = None;
            } else if ctl.now() >= deadline {
                answer(reply_to, Reply::status(Status::Timeout));
                waiting = None;
            }
        }
    }
}

aricode_rt::entry!(main);
