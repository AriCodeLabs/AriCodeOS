//! usbd (Phase 25, second part; ADR 0023): ring 3 xHCI driver for
//! USB HID keyboards and mice using the boot protocol, and for one
//! mass storage device (bulk-only, SCSI). Receives the driver contract
//! from init (`SERVICE`, cfg, hardware, IRQ, BAR0) and, as extras,
//! `SYSTEM` (`READ`) for the clock and the block service endpoint.
//! Enumerates the ports with a device attached, configures each boot HID
//! interface, polls its interrupt endpoints and delivers the events
//! over IPC (`InputServiceProtocol v1`) as a single input device;
//! storage is served with the block protocol (`aricode-proto::block`).
#![no_std]
#![no_main]

use aricode_input::{Event, HidBootKeyboard, HidBootMouse, MAX_REPORT_EVENTS};
use aricode_proto::block::{
    Op as BlockOp, Reply as BlockReply, Request as BlockRequest, Status as BlockStatus, MAX_SECTORS,
};
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS};
use aricode_rt::sys::block::map_client_buffer;
use aricode_rt::sys::virtio_pci::{bar_handle, extra_handle, DmaRegion, Mmio, CFG, IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Handle};
use aricode_usb_storage::{scsi, Cbw, Cdb, Csw, CswStatus};
use aricode_xhci::{
    bulk_trbs, cap, completion, dci, erst_entry, op, parse_storage_interface, rt, speed, trb_type,
    write_input_context, DeviceDescriptor, EndpointContext, EventRing, HidInterface, Params,
    ProducerRing, SetupPacket, SlotContext, StorageInterface, Trb,
};

const CMD_RING_TRBS: usize = 16;
const EVENT_RING_TRBS: usize = 32;
const XFER_RING_TRBS: usize = 16;
const MAX_DEVICES: usize = 4;
/// Pending report TRBs per interrupt endpoint.
const REPORTS_IN_FLIGHT: usize = 4;
const RING: usize = 128;

fn say(msg: &[u8]) {
    sys::print(b"usb-input: ");
    sys::print(msg);
    sys::print(b"\n");
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(v, &mut b));
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

/// Producer ring with its memory.
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
}

struct Endpoint {
    dci: u8,
    ring: Ring,
    /// The first report has arrived (diagnostic trace).
    seen: bool,
    /// Reports in flight: (TRB, physical buffer, virtual buffer).
    reports: [(u64, u64, u64); REPORTS_IN_FLIGHT],
    report_len: u16,
}

struct UsbDevice {
    slot: u8,
    ep0: Ring,
    endpoints: [Option<Endpoint>; MAX_DEVICES],
    /// Per endpoint: keyboard or mouse.
    decoders: [Decoder; MAX_DEVICES],
    /// Device input context (physical, virtual, bytes) and its slot
    /// context, to configure endpoints after addressing it.
    input_ctx: (u64, u64, u64),
    slot_ctx: SlotContext,
}

/// Bulk endpoint of a storage device.
struct Bulk {
    dci: u8,
    /// USB endpoint address (bit 7 = IN), for `CLEAR_FEATURE`.
    address: u8,
    ring: Ring,
    max_packet: u16,
}

/// Bulk-only mass storage device (at most one).
struct Storage {
    device: usize,
    slot: u8,
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
    /// 512-byte sectors.
    capacity: u64,
    /// The device supports `SYNCHRONIZE CACHE`; if it rejects it as
    /// unsupported, the flush has nothing to write back.
    sync_cache: bool,
    /// I/O errors reported (capped so as not to flood the console).
    errors_reported: u8,
}

/// Why a SCSI command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScsiFail {
    /// Failed bulk transfer or invalid CSW, with the xHCI completion code
    /// of the failed transfer (0: timeout, invalid CSW or phase error).
    Transport(u8),
    /// The device rejected the command (`REQUEST SENSE` says why).
    Failed,
}

#[derive(Clone, Copy)]
enum Decoder {
    None,
    Keyboard(HidBootKeyboard),
    Mouse(HidBootMouse),
}

struct Controller {
    /// `strict` boot flag: halt on the first transport failure.
    strict: bool,
    /// `usb_fault=N|idle` (tests): an injected transport failure on the
    /// N-th block request, or on the first one after two idle seconds.
    fault_inject: FaultInject,
    /// Last block request (ms), for `usb_fault=idle`.
    last_request_ms: u64,
    base: Mmio,
    op: Mmio,
    runtime: Mmio,
    doorbells: Mmio,
    params: Params,
    arena: Arena,
    cmd: Ring,
    events: EventRing,
    events_virt: u64,
    dcbaa_virt: u64,
    scratch_virt: u64,
    devices: [Option<UsbDevice>; MAX_DEVICES],
    storage: Option<Storage>,
    clock: Clock,
    keyboards: u8,
    mice: u8,
    /// The ports were routed from the EHCI controllers to the xHCI (Intel):
    /// devices need more time to show up.
    ports_switched: bool,
}

/// Event pending delivery to the client.
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
    fn open(mask: u64, clock: Clock) -> Result<Self, u64> {
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

        // Structures: DCBAA, scratchpads, command ring, event ring and ERST.
        let slots = params.max_slots as u64;
        let (dcbaa_phys, dcbaa_virt) = arena.alloc((slots + 1) * 8, 64).ok_or(exit::DMA)?;
        let mut scratch_virt = 0;
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
            scratch_virt = arr_virt;
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

        Ok(Self {
            strict: false,
            fault_inject: FaultInject::None,
            last_request_ms: 0,
            base,
            op,
            runtime,
            doorbells,
            params,
            arena,
            cmd,
            events,
            events_virt: ev_virt,
            dcbaa_virt,
            scratch_virt,
            devices: [None, None, None, None],
            storage: None,
            clock,
            keyboards: 0,
            mice: 0,
            ports_switched,
        })
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

    /// Next event in the ring, if any; updates ERDP.
    fn next_event(&mut self) -> Option<Trb> {
        let trb = read_trb(self.events_virt + (self.events.dequeue * Trb::LEN) as u64);
        let ev = self.events.accept(trb)?;
        self.runtime.w64(rt::ERDP, self.events.dequeue_pointer());
        Some(ev)
    }

    /// Sends a command and waits for its completion (code, slot).
    fn command(&mut self, trb: Trb, timeout_ms: u64) -> Option<(u8, u8)> {
        let phys = self.cmd.push(trb);
        self.ring_doorbell(0, 0);
        let deadline = self.clock.now() + timeout_ms;
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::COMMAND_COMPLETION && ev.parameter == phys {
                    return Some((ev.completion_code(), ev.slot_id()));
                }
                self.stash_event(ev);
            }
            if self.clock.now() >= deadline {
                return None;
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    /// Events that arrive while waiting for something else: transfer events
    /// are processed on the fly (reports), the rest are ignored.
    fn stash_event(&mut self, ev: Trb) {
        if ev.trb_type() == trb_type::TRANSFER_EVENT {
            self.handle_transfer_event(ev, None);
        }
    }

    /// Control transfer on EP0 of `slot`; waits for the Status Stage.
    fn control(
        &mut self,
        slot_index: usize,
        setup: SetupPacket,
        data: Option<(u64, u16)>,
    ) -> Option<u32> {
        let (slot, status_phys) = {
            let dev = self.devices[slot_index].as_mut()?;
            dev.ep0.push(Trb::setup_stage(&setup));
            if let Some((phys, len)) = data {
                dev.ep0.push(Trb::data_stage(phys, len, setup.is_in()));
            }
            let status_phys = dev
                .ep0
                .push(Trb::status_stage(data.is_none() || !setup.is_in()));
            (dev.slot, status_phys)
        };
        self.ring_doorbell(slot, 1);
        let deadline = self.clock.now() + 1000;
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::TRANSFER_EVENT && ev.parameter == status_phys {
                    return match ev.completion_code() {
                        completion::SUCCESS | completion::SHORT_PACKET => Some(ev.residual()),
                        _ => None,
                    };
                }
                self.stash_event(ev);
            }
            if self.clock.now() >= deadline {
                return None;
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    /// Enumerates a port with a device attached: reset, slot, address,
    /// descriptors, configuration and HID interfaces.
    fn enumerate_port(&mut self, port: u8) -> Result<(), &'static str> {
        let sc = self.portsc(port);
        if sc & op::PORTSC_CCS == 0 {
            return Err("no device");
        }
        if sc & op::PORTSC_PED == 0 {
            // USB 2: the port must be reset; USB 3 enables itself.
            let keep = sc & op::PORTSC_POWER;
            self.write_portsc(port, keep | op::PORTSC_RESET);
            let deadline = self.clock.now() + 500;
            while self.portsc(port) & op::PORTSC_PED == 0 {
                if self.clock.now() >= deadline {
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
        // Drop pending Port Status Change events.
        while let Some(ev) = self.next_event() {
            self.stash_event(ev);
        }

        let (code, slot) = self
            .command(Trb::enable_slot(), 500)
            .ok_or("Enable Slot: no response")?;
        if code != completion::SUCCESS || slot == 0 {
            return Err("Enable Slot failed");
        }
        let ctx_bytes = self.params.context_bytes();
        let (dev_ctx_phys, _) = self
            .arena
            .alloc((32 * ctx_bytes) as u64, 64)
            .ok_or("no DMA")?;
        // SAFETY: DCBAA entry `slot`.
        unsafe { ((self.dcbaa_virt + 8 * slot as u64) as *mut u64).write_volatile(dev_ctx_phys) };
        let ep0 = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("no DMA")?;
        let (in_phys, in_virt) = self
            .arena
            .alloc((33 * ctx_bytes) as u64, 64)
            .ok_or("no DMA")?;
        let slot_ctx = SlotContext {
            route_string: 0,
            speed: spd,
            context_entries: 1,
            root_hub_port: port,
        };
        let ep0_ctx = EndpointContext::control(speed::default_ep0_max_packet(spd), ep0.state.phys);
        let in_len = (33 * ctx_bytes) as u64;
        write_input_context(
            // SAFETY: input context inside the DMA buffer.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            0b11,
            &slot_ctx,
            &[(1, ep0_ctx)],
        );
        let (code, _) = self
            .command(Trb::address_device(in_phys, slot), 1000)
            .ok_or("Address Device: no response")?;
        if code != completion::SUCCESS {
            return Err("Address Device failed");
        }
        let index = self
            .devices
            .iter()
            .position(|d| d.is_none())
            .ok_or("too many devices")?;
        self.devices[index] = Some(UsbDevice {
            slot,
            ep0,
            endpoints: [None, None, None, None],
            decoders: [Decoder::None; MAX_DEVICES],
            input_ctx: (in_phys, in_virt, in_len),
            slot_ctx,
        });

        // Device descriptor in two steps (xHCI 4.3, step 7): first only
        // 8 bytes, which fit in any EP0, to learn bMaxPacketSize0; if it
        // differs from the assumed value the context is fixed with
        // Evaluate Context and only then is the full descriptor requested.
        let (buf_phys, buf_virt) = self.arena.alloc(512, 64).ok_or("no DMA")?;
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_DEVICE, 0, 8),
            Some((buf_phys, 8)),
        )
        .ok_or("GET_DESCRIPTOR(device, 8 bytes) failed")?;
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
                0b10,
                &slot_ctx,
                &[(1, ep0_ctx)],
            );
            let (code, _) = self
                .command(Trb::evaluate_context(in_phys, slot), 500)
                .ok_or("Evaluate Context: no response")?;
            if code != completion::SUCCESS {
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
        )
        .ok_or("GET_DESCRIPTOR(device) failed")?;
        let desc = DeviceDescriptor::parse(raw).ok_or("invalid device descriptor")?;
        // Full configuration.
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, 9),
            Some((buf_phys, 9)),
        )
        .ok_or("GET_DESCRIPTOR(config) failed")?;
        let total = u16::from_le_bytes([raw[2], raw[3]]).clamp(9, 512);
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, total),
            Some((buf_phys, total)),
        )
        .ok_or("GET_DESCRIPTOR(full config) failed")?;
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
            self.control(index, SetupPacket::set_configuration(value), None)
                .ok_or("SET_CONFIGURATION failed")?;
            return self.configure_storage(index, port, iface);
        }
        self.control(index, SetupPacket::set_configuration(config_value), None)
            .ok_or("SET_CONFIGURATION failed")?;

        // Each HID interface: boot protocol, interrupt endpoint
        // configured and several reports in flight.
        let mut max_dci = 1u8;
        let mut eps: [(u8, EndpointContext); 4] = [(0, EndpointContext::default()); 4];
        let mut prepared = 0;
        for hid in hids.iter().flatten() {
            self.control(index, SetupPacket::hid_set_protocol(hid.interface, 0), None)
                .ok_or("SET_PROTOCOL failed")?;
            let _ = self.control(index, SetupPacket::hid_set_idle(hid.interface), None);
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
                ring,
                seen: false,
                reports,
                report_len,
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
            add_flags,
            &slot_ctx,
            &eps[..prepared],
        );
        let (code, _) = self
            .command(Trb::configure_endpoint(in_phys, slot), 1000)
            .ok_or("Configure Endpoint: no response")?;
        if code != completion::SUCCESS {
            return Err("Configure Endpoint failed");
        }
        // Reports in flight.
        let dev = self.devices[index].as_mut().ok_or("device")?;
        for ep in dev.endpoints.iter_mut().flatten() {
            for r in ep.reports.iter_mut() {
                r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
            }
            self.doorbells.w32(4 * slot as u64, ep.dci as u32);
        }
        let _ = desc;
        Ok(())
    }

    /// Configures the bulk endpoints of a storage interface, identifies
    /// the device (INQUIRY), waits for it to become ready and reads its
    /// capacity. Only 512-byte blocks are supported.
    fn configure_storage(
        &mut self,
        index: usize,
        port: u8,
        iface: StorageInterface,
    ) -> Result<(), &'static str> {
        let ring_in = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("no DMA")?;
        let ring_out = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("no DMA")?;
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
            (dev.slot, dev.input_ctx, dev.slot_ctx)
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
            1 | 1 << dci_in | 1 << dci_out,
            &slot_ctx,
            &eps,
        );
        let (code, _) = self
            .command(Trb::configure_endpoint(in_phys, slot), 1000)
            .ok_or("Configure Endpoint: no response")?;
        if code != completion::SUCCESS {
            return Err("Configure Endpoint (storage) failed");
        }
        let cbw = self.arena.alloc(64, 64).ok_or("no DMA")?;
        let csw = self.arena.alloc(64, 64).ok_or("no DMA")?;
        let small = self.arena.alloc(64, 64).ok_or("no DMA")?;
        self.storage = Some(Storage {
            device: index,
            slot,
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
        });
        if let Err(why) = self.storage_init(port) {
            self.storage = None;
            return Err(why);
        }
        Ok(())
    }

    /// INQUIRY, TEST UNIT READY (with retries while the device
    /// starts up) and READ CAPACITY(10).
    fn storage_init(&mut self, port: u8) -> Result<(), &'static str> {
        let (small_phys, small_virt) = self.storage.as_ref().ok_or("storage")?.small;
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        let small = || unsafe { core::slice::from_raw_parts(small_virt as *const u8, 64) };
        self.scsi(
            scsi::inquiry(),
            Some((small_phys, scsi::INQUIRY_LEN as u32, true)),
        )
        .map_err(|_| "INQUIRY failed")?;
        let inq = scsi::Inquiry::parse(small()).ok_or("invalid INQUIRY")?;
        if inq.peripheral_type != 0 {
            return Err("not a direct access device (disk)");
        }
        let mut ready = false;
        for _ in 0..20 {
            match self.scsi(scsi::test_unit_ready(), None) {
                Ok(()) => {
                    ready = true;
                    break;
                }
                Err(ScsiFail::Failed) => {
                    let sense = self.sense();
                    if !matches!(
                        sense.map(|s| s.key),
                        Some(scsi::Sense::UNIT_ATTENTION | scsi::Sense::NOT_READY)
                    ) {
                        return Err("TEST UNIT READY rejected");
                    }
                }
                Err(ScsiFail::Transport(_)) => return Err("TEST UNIT READY: no response"),
            }
            let until = self.clock.now() + 100;
            while self.clock.now() < until {
                let _ = sys::irq_wait(IRQ, 10);
            }
        }
        if !ready {
            return Err("the device never becomes ready");
        }
        self.scsi(
            scsi::read_capacity10(),
            Some((small_phys, scsi::READ_CAPACITY_LEN as u32, true)),
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

    /// Synchronous bulk transfer on the storage IN or OUT endpoint.
    /// `Ok(bytes not transferred)` or the xHCI completion code
    /// (`completion::STALL` included; 0 = no event).
    fn bulk(&mut self, dir_in: bool, phys: u64, len: u32, timeout_ms: u64) -> Result<u32, u8> {
        let (slot, dci, last) = {
            let st = self.storage.as_mut().ok_or(0u8)?;
            let ep = if dir_in {
                &mut st.bulk_in
            } else {
                &mut st.bulk_out
            };
            let mut trbs = [Trb::noop_command(); 4];
            let n = bulk_trbs(phys, len, ep.max_packet, &mut trbs).ok_or(0u8)?;
            let mut last = 0;
            for t in &trbs[..n] {
                last = ep.ring.push(*t);
            }
            (st.slot, ep.dci, last)
        };
        self.ring_doorbell(slot, dci as u32);
        let deadline = self.clock.now() + timeout_ms;
        loop {
            while let Some(ev) = self.next_event() {
                if ev.trb_type() == trb_type::TRANSFER_EVENT
                    && ev.slot_id() == slot
                    && ev.endpoint_id() == dci
                {
                    match ev.completion_code() {
                        completion::SUCCESS | completion::SHORT_PACKET if ev.parameter == last => {
                            return Ok(ev.residual());
                        }
                        completion::SUCCESS | completion::SHORT_PACKET => continue,
                        code => return Err(code),
                    }
                }
                self.stash_event(ev);
            }
            if self.clock.now() >= deadline {
                return Err(0);
            }
            let _ = sys::irq_wait(IRQ, 5);
        }
    }

    /// Recovers a halted (STALL) bulk endpoint: Reset Endpoint, the ring
    /// continues from where the producer is, and `CLEAR_FEATURE(HALT)`.
    fn recover_bulk(&mut self, dir_in: bool) {
        let (slot, dci, address, pointer, cycle, device) = {
            let Some(st) = self.storage.as_ref() else {
                return;
            };
            let ep = if dir_in { &st.bulk_in } else { &st.bulk_out };
            (
                st.slot,
                ep.dci,
                ep.address,
                ep.ring.state.phys + (ep.ring.state.enqueue * Trb::LEN) as u64,
                ep.ring.state.cycle,
                st.device,
            )
        };
        let _ = self.command(Trb::reset_endpoint(slot, dci), 500);
        let _ = self.command(Trb::set_tr_dequeue(slot, dci, pointer, cycle), 500);
        let _ = self.control(device, SetupPacket::clear_endpoint_halt(address), None);
    }

    /// One SCSI command over the bulk-only transport: CBW, data (if any, as
    /// `(physical, bytes, towards the host)`) and CSW. A STALL in the data or
    /// in the CSW is recovered as BOT 6.7 requires; the CSW gives the status.
    fn scsi(&mut self, cdb: Cdb, data: Option<(u64, u32, bool)>) -> Result<(), ScsiFail> {
        let (tag, cbw_phys, cbw_virt, csw_phys, csw_virt) = {
            let st = self.storage.as_mut().ok_or(ScsiFail::Transport(0))?;
            st.tag = st.tag.wrapping_add(1);
            (st.tag, st.cbw.0, st.cbw.1, st.csw.0, st.csw.1)
        };
        let cbw = Cbw {
            tag,
            data_len: data.map_or(0, |d| d.1),
            data_in: data.is_some_and(|d| d.2),
            lun: 0,
            cdb,
        };
        // SAFETY: CBW in the driver's DMA buffer.
        unsafe { (cbw_virt as *mut [u8; Cbw::LEN]).write_volatile(cbw.encode()) };
        if let Err(code) = self.bulk(false, cbw_phys, Cbw::LEN as u32, 1000) {
            self.recover_bulk(false);
            return Err(ScsiFail::Transport(code));
        }
        if let Some((phys, len, dir_in)) = data {
            match self.bulk(dir_in, phys, len, 5000) {
                Ok(_) => {}
                Err(completion::STALL) => self.recover_bulk(dir_in),
                Err(code) => return Err(ScsiFail::Transport(code)),
            }
        }
        let mut csw = self.bulk(true, csw_phys, Csw::LEN as u32, 1000);
        if csw == Err(completion::STALL) {
            self.recover_bulk(true);
            csw = self.bulk(true, csw_phys, Csw::LEN as u32, 1000);
        }
        csw.map_err(ScsiFail::Transport)?;
        // SAFETY: CSW in the driver's DMA buffer.
        let raw = unsafe { (csw_virt as *const [u8; Csw::LEN]).read_volatile() };
        let csw = Csw::parse(&raw).ok_or(ScsiFail::Transport(0))?;
        if csw.tag != tag {
            return Err(ScsiFail::Transport(0));
        }
        match csw.status {
            CswStatus::Passed => Ok(()),
            CswStatus::Failed => Err(ScsiFail::Failed),
            CswStatus::PhaseError => Err(ScsiFail::Transport(0)),
        }
    }

    /// `REQUEST SENSE` after a rejected command.
    fn sense(&mut self) -> Option<scsi::Sense> {
        let (phys, virt) = self.storage.as_ref()?.small;
        self.scsi(
            scsi::request_sense(),
            Some((phys, scsi::SENSE_LEN as u32, true)),
        )
        .ok()?;
        // SAFETY: own 64-byte buffer in the driver's DMA memory.
        scsi::Sense::parse(unsafe { core::slice::from_raw_parts(virt as *const u8, 64) })
    }

    /// A transport failure as a kernel fault event: `usb r|w lba N` with the
    /// completion code (`diag` shows it; `usb_fault` injections say so).
    fn record_fault(&self, write: bool, lba: u64, code: u8, injected: bool) {
        let mut name = [0u8; 24];
        let prefix: &[u8] = match (write, injected) {
            (false, false) => b"usb read lba ",
            (true, false) => b"usb write lba ",
            (false, true) => b"usb inj read lba ",
            (true, true) => b"usb inj write lba ",
        };
        name[..prefix.len()].copy_from_slice(prefix);
        let mut digits = [0u8; 20];
        let d = aricode_rt::fmt_u64(lba, &mut digits);
        let n = (prefix.len() + d.len()).min(name.len());
        name[prefix.len()..n].copy_from_slice(&d[..n - prefix.len()]);
        if let Ok(text) = core::str::from_utf8(&name[..n]) {
            let _ = sys::event_record(code as u64, text);
        }
    }

    /// `strict` (ADR 0028): dump what a transport failure looked like and
    /// stop; init does not restart the driver, the state stays inspectable.
    fn halt(&mut self, write: bool, lba: u64, count: u16, code: u8, attempt: usize) -> ! {
        sys::print(b"usb-input: strict: halting on a transport failure\n");
        sys::print(b"  command   ");
        sys::print(if write { b"WRITE(10)" } else { b"READ(10)" });
        sys::print(b" lba ");
        num(lba);
        sys::print(b" count ");
        num(count as u64);
        sys::print(b"\n  completion code ");
        num(code as u64);
        sys::print(b" attempt ");
        num(attempt as u64);
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
        }
        sys::print(b"\n");
        sys::process_exit(exit::TRANSPORT_FAULT)
    }

    /// Reset Recovery (BOT 5.3.4) after a failed transfer: Bulk-Only Mass
    /// Storage Reset, then both bulk pipes unhalted. Some sticks answer the
    /// first commands after a write with transaction errors; this puts the
    /// transport back in a known state before the command is retried.
    fn reset_recovery(&mut self) {
        let Some((device, interface)) = self.storage.as_ref().map(|s| (s.device, s.interface))
        else {
            return;
        };
        let _ = self.control(device, SetupPacket::mass_storage_reset(interface), None);
        self.recover_bulk(true);
        self.recover_bulk(false);
    }

    /// Reads or writes `count` sectors in the client's DMA buffer. A
    /// rejection due to unit attention is retried once. A transport failure
    /// (a stick that stops answering for a moment, typically right after a
    /// write) is retried after growing pauses; a Reset Recovery is the last
    /// resort, after every pause, since some sticks never come back from
    /// it.
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
        const PAUSES_MS: [u64; 6] = [50, 200, 500, 1000, 2000, 3000];
        let mut transport_failures = 0;
        let now = self.clock.now();
        let injected_now = self.fault_inject.due(now, self.last_request_ms);
        self.last_request_ms = now;
        for attempt in 0..8 {
            let injected = injected_now && attempt == 0;
            let result = if injected {
                Err(ScsiFail::Transport(completion::USB_TRANSACTION_ERROR))
            } else {
                self.scsi(cdb, Some((phys, count as u32 * 512, !write)))
            };
            match result {
                Ok(()) => {
                    if transport_failures > 0 {
                        say(b"transfer recovered");
                    }
                    return true;
                }
                Err(ScsiFail::Failed) => {
                    let sense = self.sense();
                    if sense.map(|s| s.key) != Some(scsi::Sense::UNIT_ATTENTION) {
                        self.report_error(name, lba, sense);
                        return false;
                    }
                }
                Err(ScsiFail::Transport(code)) => {
                    // Never hidden: every failed transfer is an event
                    // (ADR 0028), retried or not.
                    self.record_fault(write, lba, code, injected);
                    if self.strict {
                        self.halt(write, lba, count, code, transport_failures);
                    }
                    if transport_failures >= PAUSES_MS.len() {
                        self.report_error(name, lba, None);
                        return false;
                    }
                    let pause = PAUSES_MS[transport_failures];
                    transport_failures += 1;
                    sys::print(b"usb-input: transfer failed (completion code ");
                    num(code as u64);
                    sys::print(b"), retry in ");
                    num(pause);
                    sys::print(b" ms\n");
                    sys::time::sleep_ms(pause);
                    if transport_failures == PAUSES_MS.len() {
                        say(b"reset recovery (last resort)");
                        self.reset_recovery();
                    }
                }
            }
        }
        self.report_error(name, lba, None);
        false
    }

    /// Makes writes durable (`SYNCHRONIZE CACHE`). A device that does
    /// not implement the command has no cache to write back: it is noted
    /// once and the flush becomes immediate.
    fn flush(&mut self) -> bool {
        if !self.storage.as_ref().is_some_and(|s| s.sync_cache) {
            return self.storage.is_some();
        }
        match self.scsi(scsi::synchronize_cache10(), None) {
            Ok(()) => true,
            Err(ScsiFail::Failed) => {
                let sense = self.sense();
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
            Err(ScsiFail::Transport(_)) => {
                self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                false
            }
        }
    }

    /// Reports a failed command with its sense data (the first few times).
    fn report_error(&mut self, what: &[u8], lba: u64, sense: Option<scsi::Sense>) {
        let Some(st) = self.storage.as_mut() else {
            return;
        };
        if st.errors_reported >= 4 {
            return;
        }
        st.errors_reported += 1;
        sys::print(b"usb-input: ");
        sys::print(what);
        sys::print(b" lba ");
        num(lba);
        match sense {
            Some(s) => {
                sys::print(b" rejected: sense ");
                num(s.key as u64);
                sys::print(b"/");
                num(s.asc as u64);
                sys::print(b"/");
                num(s.ascq as u64);
                sys::print(b"\n");
            }
            None => sys::print(b" transport failure\n"),
        }
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

    /// A Transfer Event from an interrupt endpoint: decodes the report
    /// and requeues the TRB.
    fn handle_transfer_event(&mut self, ev: Trb, pending: Option<&mut Pending>) {
        let now = self.clock.now();
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
                let got = (ep.report_len as u32).saturating_sub(ev.residual()) as usize;
                if !ep.seen {
                    ep.seen = true;
                    sys::print(b"usb-input: first report from slot ");
                    num(dev.slot as u64);
                    sys::print(b" (");
                    num(got as u64);
                    sys::print(b" bytes, code ");
                    num(ev.completion_code() as u64);
                    sys::print(b")\n");
                }
                // SAFETY: the driver's report buffer (64 bytes).
                let report = unsafe { core::slice::from_raw_parts(r.2 as *const u8, got.min(64)) };
                let n = match decoder {
                    Decoder::Keyboard(k) => k.feed(report, &mut out),
                    Decoder::Mouse(m) => m.feed(report, &mut out),
                    Decoder::None => 0,
                };
                if let Some(p) = pending {
                    for e in &out[..n] {
                        p.push(*e, now);
                    }
                }
                r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
                self.doorbells.w32(4 * dev.slot as u64, ep.dci as u32);
                return;
            }
        }
    }

    /// Collects the ring events; reports go to `pending`.
    fn poll(&mut self, pending: &mut Pending) {
        // Acknowledge the interrupt of interrupter 0.
        let iman = self.runtime.r32(rt::IMAN);
        if iman & rt::IMAN_PENDING != 0 {
            self.runtime.w32(rt::IMAN, iman);
        }
        while let Some(ev) = self.next_event() {
            match ev.trb_type() {
                trb_type::TRANSFER_EVENT => self.handle_transfer_event(ev, Some(pending)),
                trb_type::PORT_STATUS_CHANGE => {
                    let port = ev.port_id();
                    if port >= 1 && port <= self.params.max_ports {
                        let sc = self.portsc(port);
                        self.write_portsc(
                            port,
                            (sc & op::PORTSC_POWER) | (sc & op::PORTSC_CHANGE_BITS),
                        );
                    }
                }
                _ => {}
            }
        }
        let _ = (self.base.0, self.scratch_virt);
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
                match map_client_buffer(handles[1]) {
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
    if handles.len() >= 2 {
        let _ = sys::handle_close(handles[1]);
    }
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
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

fn device_name(ctl: &Controller) -> &'static str {
    match (ctl.keyboards > 0, ctl.mice > 0) {
        (true, true) => "USB HID Keyboard+Mouse",
        (true, false) => "USB HID Keyboard",
        (false, true) => "USB HID Mouse",
        (false, false) => "USB HID (no devices)",
    }
}

/// Test fault injection (`usb_fault`, ADR 0028).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultInject {
    None,
    /// Block requests left before the injected failure.
    Count(u32),
    /// The first block request after two idle seconds (once).
    AfterIdle,
}

impl FaultInject {
    const IDLE_MS: u64 = 2000;

    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("idle") => Self::AfterIdle,
            Some(n) => n.parse().map_or(Self::None, Self::Count),
            None => Self::None,
        }
    }

    /// Whether this request fails; advances the state.
    fn due(&mut self, now: u64, last_request_ms: u64) -> bool {
        match *self {
            Self::None => false,
            Self::Count(1) => {
                *self = Self::None;
                true
            }
            Self::Count(n) => {
                *self = Self::Count(n - 1);
                false
            }
            Self::AfterIdle if last_request_ms > 0 && now - last_request_ms >= Self::IDLE_MS => {
                *self = Self::None;
                true
            }
            Self::AfterIdle => false,
        }
    }
}

fn main(mask: u64) -> u64 {
    let clock = Clock(extra_handle(mask, 0));
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
    sys::print(b"usb-input: xHCI ready (");
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
    let settle = ctl.clock.now() + if ctl.ports_switched { 500 } else { 100 };
    while ctl.clock.now() < settle {
        let _ = sys::irq_wait(IRQ, 10);
    }
    for port in 1..=ctl.params.max_ports {
        if ctl.portsc(port) & op::PORTSC_CCS == 0 {
            continue;
        }
        match ctl.enumerate_port(port) {
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
    let mut pending = Pending {
        buf: [(Event::default(), 0); RING],
        head: 0,
        len: 0,
    };
    let mut waiting: Option<(Handle, u64)> = None;
    let mut msg = [0u8; 32];
    let mut handles = [Handle(0); abi::IPC_MAX_HANDLES];
    let mut events = [WireEvent::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    say(b"serving requests");
    loop {
        ctl.poll(&mut pending);
        let now = ctl.clock.now();
        for _ in 0..8 {
            let Ok((which, n, nh)) = sys::ipc_receive_any(endpoints, &mut msg, &mut handles, 0)
            else {
                break;
            };
            if nh == 0 {
                continue;
            }
            if which == 1 {
                serve_block(&mut ctl, &msg[..n], &handles[..nh]);
                continue;
            }
            let reply_to = handles[0];
            match Request::decode(&msg[..n]) {
                Some(Request { op: Op::Info, .. }) => {
                    let reply = Reply::status(Status::Ok)
                        .with_name(device_name(&ctl))
                        .unwrap_or(Reply::status(Status::Ok));
                    answer(reply_to, reply);
                }
                Some(Request {
                    op: Op::Read,
                    timeout_ms,
                }) => {
                    if pending.len > 0 {
                        let n = pending.take(&mut events, &mut times);
                        answer(reply_to, Reply::events(&events[..n], &times[..n]));
                    } else if waiting.is_some() {
                        answer(reply_to, Reply::status(Status::Busy));
                    } else {
                        waiting = Some((reply_to, now + timeout_ms.max(1) as u64));
                    }
                }
                None => answer(reply_to, Reply::status(Status::Invalid)),
            }
        }
        if let Some((reply_to, deadline)) = waiting {
            if pending.len > 0 {
                let n = pending.take(&mut events, &mut times);
                answer(reply_to, Reply::events(&events[..n], &times[..n]));
                waiting = None;
            } else if now >= deadline {
                answer(reply_to, Reply::status(Status::Timeout));
                waiting = None;
            }
        }
        let _ = sys::irq_wait(IRQ, 10);
    }
}

aricode_rt::entry!(main);
