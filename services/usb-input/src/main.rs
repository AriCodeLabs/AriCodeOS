//! usbd (Fase 25, segunda parte): driver xHCI en ring 3 para teclados y
//! ratones USB HID con protocolo de arranque. Recibe el contrato de driver
//! de init (`SERVICE`, cfg, hardware, IRQ, BAR0) y, como extra, `SYSTEM`
//! (`READ`) para el reloj. Enumera los puertos con dispositivo, configura
//! cada interfaz HID de arranque, sondea sus endpoints de interrupción y
//! entrega los eventos por IPC (`InputServiceProtocol v1`) como un único
//! dispositivo de entrada.
#![no_std]
#![no_main]

use aricode_input::{Event, HidBootKeyboard, HidBootMouse};
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS};
use aricode_rt::sys::virtio_pci::{bar_handle, extra_handle, DmaRegion, Mmio, IRQ, SERVICE};
use aricode_rt::sys::{self, abi, Handle};
use aricode_xhci::{
    cap, completion, dci, erst_entry, op, rt, speed, trb_type, write_input_context,
    DeviceDescriptor, EndpointContext, EventRing, HidInterface, Params, ProducerRing, SetupPacket,
    SlotContext, Trb,
};

const CMD_RING_TRBS: usize = 16;
const EVENT_RING_TRBS: usize = 32;
const XFER_RING_TRBS: usize = 16;
const MAX_DEVICES: usize = 4;
/// TRB de informe pendientes por endpoint de interrupción.
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

/// Reserva lineal dentro del buffer DMA del driver (alineación de 64 bytes).
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
    // SAFETY: `virt` es un TRB dentro del buffer DMA del driver.
    unsafe { (virt as *mut [u8; Trb::LEN]).write_volatile(trb.to_bytes()) };
}

fn read_trb(virt: u64) -> Trb {
    // SAFETY: ídem.
    Trb::from_bytes(&unsafe { (virt as *const [u8; Trb::LEN]).read_volatile() })
}

/// Anillo de productor con su memoria.
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

    /// Encola un TRB y devuelve su dirección física.
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
    /// Ya llegó el primer informe (traza de diagnóstico).
    seen: bool,
    /// Informes en vuelo: (TRB, buffer físico, buffer virtual).
    reports: [(u64, u64, u64); REPORTS_IN_FLIGHT],
    report_len: u16,
}

struct UsbDevice {
    slot: u8,
    ep0: Ring,
    endpoints: [Option<Endpoint>; MAX_DEVICES],
    /// Por endpoint: teclado o ratón.
    decoders: [Decoder; MAX_DEVICES],
}

#[derive(Clone, Copy)]
enum Decoder {
    None,
    Keyboard(HidBootKeyboard),
    Mouse(HidBootMouse),
}

struct Controller {
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
    clock: Clock,
    keyboards: u8,
    mice: u8,
}

/// Evento pendiente de entregar al cliente.
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

/// Códigos de salida de la inicialización (estables para los escenarios).
mod exit {
    pub const MAP_BAR: u64 = 10;
    pub const NOT_XHCI: u64 = 11;
    pub const DMA: u64 = 12;
    pub const RESET_TIMEOUT: u64 = 13;
    pub const START_TIMEOUT: u64 = 14;
}

impl Controller {
    fn open(mask: u64, clock: Clock) -> Result<Self, u64> {
        let bar = bar_handle(mask, 0).ok_or(exit::MAP_BAR)?;
        let base = Mmio(sys::mmio_map(bar).map_err(|_| exit::MAP_BAR)?);
        // Los registros de capacidad se leen por palabras de 32 bits: QEMU no
        // atiende accesos de 16 bits en HCIVERSION.
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
        let region = DmaRegion::alloc(64 * 1024).map_err(|_| exit::DMA)?;
        let mut arena = Arena { region, next: 0 };

        // Parar y reiniciar el controlador.
        op.w32(op::USBCMD, op.r32(op::USBCMD) & !op::CMD_RUN);
        wait_bits(&op, op::USBSTS, op::STS_HALTED, true, &clock, 200).ok_or(exit::RESET_TIMEOUT)?;
        op.w32(op::USBCMD, op::CMD_RESET);
        wait_bits(&op, op::USBCMD, op::CMD_RESET, false, &clock, 500).ok_or(exit::RESET_TIMEOUT)?;
        wait_bits(&op, op::USBSTS, op::STS_NOT_READY, false, &clock, 500)
            .ok_or(exit::RESET_TIMEOUT)?;

        // Estructuras: DCBAA, scratchpads, anillo de comandos, anillo de eventos y ERST.
        let slots = params.max_slots as u64;
        let (dcbaa_phys, dcbaa_virt) = arena.alloc((slots + 1) * 8, 64).ok_or(exit::DMA)?;
        let mut scratch_virt = 0;
        if params.scratchpad_buffers > 0 {
            let n = params.scratchpad_buffers as u64;
            let (arr_phys, arr_virt) = arena.alloc(n * 8, 64).ok_or(exit::DMA)?;
            for i in 0..n {
                let (page_phys, _) = arena.alloc(4096, 4096).ok_or(exit::DMA)?;
                // SAFETY: entrada del array de scratchpads dentro del buffer DMA.
                unsafe { ((arr_virt + 8 * i) as *mut u64).write_volatile(page_phys) };
            }
            // SAFETY: entrada 0 de la DCBAA.
            unsafe { (dcbaa_virt as *mut u64).write_volatile(arr_phys) };
            scratch_virt = arr_virt;
        }
        let cmd = Ring::new(&mut arena, CMD_RING_TRBS).ok_or(exit::DMA)?;
        let (ev_phys, ev_virt) = arena
            .alloc((EVENT_RING_TRBS * Trb::LEN) as u64, 64)
            .ok_or(exit::DMA)?;
        let (erst_phys, erst_virt) = arena.alloc(16, 64).ok_or(exit::DMA)?;
        // SAFETY: entrada de la ERST dentro del buffer DMA.
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
        op.w32(op::USBCMD, op::CMD_RUN | op::CMD_INTE);
        wait_bits(&op, op::USBSTS, op::STS_HALTED, false, &clock, 200)
            .ok_or(exit::START_TIMEOUT)?;

        Ok(Self {
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
            clock,
            keyboards: 0,
            mice: 0,
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

    /// Siguiente evento del anillo, si lo hay; actualiza ERDP.
    fn next_event(&mut self) -> Option<Trb> {
        let trb = read_trb(self.events_virt + (self.events.dequeue * Trb::LEN) as u64);
        let ev = self.events.accept(trb)?;
        self.runtime.w64(rt::ERDP, self.events.dequeue_pointer());
        Some(ev)
    }

    /// Envía un comando y espera su terminación (código, slot).
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

    /// Eventos que llegan mientras se espera otra cosa: los de transferencia
    /// se procesan al vuelo (informes), los demás se ignoran.
    fn stash_event(&mut self, ev: Trb) {
        if ev.trb_type() == trb_type::TRANSFER_EVENT {
            self.handle_transfer_event(ev, None);
        }
    }

    /// Transferencia de control en EP0 de `slot`; espera el Status Stage.
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

    /// Enumera un puerto con dispositivo conectado: reset, slot, dirección,
    /// descriptores, configuración e interfaces HID.
    fn enumerate_port(&mut self, port: u8) -> Result<(), &'static str> {
        let sc = self.portsc(port);
        if sc & op::PORTSC_CCS == 0 {
            return Err("sin dispositivo");
        }
        if sc & op::PORTSC_PED == 0 {
            // USB 2: hay que resetear el puerto; USB 3 se habilita solo.
            let keep = sc & op::PORTSC_POWER;
            self.write_portsc(port, keep | op::PORTSC_RESET);
            let deadline = self.clock.now() + 500;
            while self.portsc(port) & op::PORTSC_PED == 0 {
                if self.clock.now() >= deadline {
                    return Err("el puerto no se habilita tras el reset");
                }
                let _ = sys::irq_wait(IRQ, 5);
            }
        }
        // Limpiar los bits de cambio del puerto.
        let sc = self.portsc(port);
        self.write_portsc(
            port,
            (sc & op::PORTSC_POWER) | (sc & op::PORTSC_CHANGE_BITS),
        );
        let spd = op::port_speed(self.portsc(port));
        // Tirar los Port Status Change pendientes.
        while let Some(ev) = self.next_event() {
            self.stash_event(ev);
        }

        let (code, slot) = self
            .command(Trb::enable_slot(), 500)
            .ok_or("Enable Slot sin respuesta")?;
        if code != completion::SUCCESS || slot == 0 {
            return Err("Enable Slot fallo");
        }
        let ctx_bytes = self.params.context_bytes();
        let (dev_ctx_phys, _) = self
            .arena
            .alloc((32 * ctx_bytes) as u64, 64)
            .ok_or("sin DMA")?;
        // SAFETY: entrada `slot` de la DCBAA.
        unsafe { ((self.dcbaa_virt + 8 * slot as u64) as *mut u64).write_volatile(dev_ctx_phys) };
        let ep0 = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("sin DMA")?;
        let (in_phys, in_virt) = self
            .arena
            .alloc((33 * ctx_bytes) as u64, 64)
            .ok_or("sin DMA")?;
        let slot_ctx = SlotContext {
            route_string: 0,
            speed: spd,
            context_entries: 1,
            root_hub_port: port,
        };
        let ep0_ctx = EndpointContext::control(speed::default_ep0_max_packet(spd), ep0.state.phys);
        let in_len = (33 * ctx_bytes) as u64;
        write_input_context(
            // SAFETY: input context dentro del buffer DMA.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            0b11,
            &slot_ctx,
            &[(1, ep0_ctx)],
        );
        let (code, _) = self
            .command(Trb::address_device(in_phys, slot), 1000)
            .ok_or("Address Device sin respuesta")?;
        if code != completion::SUCCESS {
            return Err("Address Device fallo");
        }
        let index = self
            .devices
            .iter()
            .position(|d| d.is_none())
            .ok_or("demasiados dispositivos")?;
        self.devices[index] = Some(UsbDevice {
            slot,
            ep0,
            endpoints: [None, None, None, None],
            decoders: [Decoder::None; MAX_DEVICES],
        });

        // Descriptor de dispositivo (y ajuste del tamaño de paquete de EP0).
        let (buf_phys, buf_virt) = self.arena.alloc(512, 64).ok_or("sin DMA")?;
        self.control(
            index,
            SetupPacket::get_descriptor(
                aricode_xhci::request::DESC_DEVICE,
                0,
                DeviceDescriptor::LEN,
            ),
            Some((buf_phys, DeviceDescriptor::LEN)),
        )
        .ok_or("GET_DESCRIPTOR(device) fallo")?;
        // SAFETY: buffer DMA de 512 bytes del driver.
        let raw = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, 512) };
        let desc = DeviceDescriptor::parse(raw).ok_or("descriptor de dispositivo invalido")?;
        if desc.max_packet_size0 as u16 != speed::default_ep0_max_packet(spd) && spd != speed::SUPER
        {
            let ep0_ctx = EndpointContext::control(desc.max_packet_size0 as u16, ep0_ctx.ring_phys);
            write_input_context(
                // SAFETY: ídem.
                unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
                ctx_bytes,
                0b10,
                &slot_ctx,
                &[(1, ep0_ctx)],
            );
            self.command(Trb::evaluate_context(in_phys, slot), 500);
        }
        // Configuración completa.
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, 9),
            Some((buf_phys, 9)),
        )
        .ok_or("GET_DESCRIPTOR(config) fallo")?;
        let total = u16::from_le_bytes([raw[2], raw[3]]).clamp(9, 512);
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_CONFIGURATION, 0, total),
            Some((buf_phys, total)),
        )
        .ok_or("GET_DESCRIPTOR(config completa) fallo")?;
        let mut hids = [None; 4];
        let (config_value, n) =
            aricode_xhci::parse_configuration(&raw[..total as usize], &mut hids)
                .ok_or("configuracion invalida")?;
        if n == 0 {
            return Err("sin interfaces HID de arranque");
        }
        self.control(index, SetupPacket::set_configuration(config_value), None)
            .ok_or("SET_CONFIGURATION fallo")?;

        // Cada interfaz HID: protocolo de arranque, endpoint de interrupción
        // configurado y varios informes en vuelo.
        let mut max_dci = 1u8;
        let mut eps: [(u8, EndpointContext); 4] = [(0, EndpointContext::default()); 4];
        let mut prepared = 0;
        for hid in hids.iter().flatten() {
            self.control(index, SetupPacket::hid_set_protocol(hid.interface, 0), None)
                .ok_or("SET_PROTOCOL fallo")?;
            let _ = self.control(index, SetupPacket::hid_set_idle(hid.interface), None);
            let d = dci(hid.endpoint, true);
            let ring = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("sin DMA")?;
            let report_len = hid.max_packet_size.clamp(1, 64);
            let mut reports = [(0u64, 0u64, 0u64); REPORTS_IN_FLIGHT];
            for r in reports.iter_mut() {
                let (p, v) = self.arena.alloc(64, 64).ok_or("sin DMA")?;
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
            let dev = self.devices[index].as_mut().ok_or("dispositivo")?;
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
            // SAFETY: ídem.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            add_flags,
            &slot_ctx,
            &eps[..prepared],
        );
        let (code, _) = self
            .command(Trb::configure_endpoint(in_phys, slot), 1000)
            .ok_or("Configure Endpoint sin respuesta")?;
        if code != completion::SUCCESS {
            return Err("Configure Endpoint fallo");
        }
        // Informes en vuelo.
        let dev = self.devices[index].as_mut().ok_or("dispositivo")?;
        for ep in dev.endpoints.iter_mut().flatten() {
            for r in ep.reports.iter_mut() {
                r.0 = ep.ring.push(Trb::normal(r.1, ep.report_len as u32));
            }
            self.doorbells.w32(4 * slot as u64, ep.dci as u32);
        }
        let _ = desc;
        Ok(())
    }

    fn report_interface(&self, hid: &HidInterface, port: u8) {
        sys::print(b"usb-input: puerto ");
        num(port as u64);
        sys::print(match hid.protocol {
            1 => b": teclado HID (protocolo de arranque, ",
            2 => b": raton HID (protocolo de arranque, ",
            _ => b": interfaz HID desconocida (",
        });
        num(hid.max_packet_size as u64);
        sys::print(b" bytes por informe, endpoint ");
        num(hid.endpoint as u64);
        sys::print(b")\n");
    }

    /// Un Transfer Event de un endpoint de interrupción: decodifica el informe
    /// y vuelve a encolar el TRB.
    fn handle_transfer_event(&mut self, ev: Trb, pending: Option<&mut Pending>) {
        let now = self.clock.now();
        let mut out = [Event::default(); 16];
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
                    sys::print(b"usb-input: primer informe del slot ");
                    num(dev.slot as u64);
                    sys::print(b" (");
                    num(got as u64);
                    sys::print(b" bytes, codigo ");
                    num(ev.completion_code() as u64);
                    sys::print(b")\n");
                }
                // SAFETY: buffer de informe del driver (64 bytes).
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

    /// Recoge los eventos del anillo; los informes van a `pending`.
    fn poll(&mut self, pending: &mut Pending) {
        // Reconocer la interrupción del interrupter 0.
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

/// Espera acotada a que `reg & mask` valga `set`.
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
        (false, false) => "USB HID (sin dispositivos)",
    }
}

fn main(mask: u64) -> u64 {
    let clock = Clock(extra_handle(mask, 0));
    let mut ctl = match Controller::open(mask, clock) {
        Ok(c) => c,
        Err(exit::NOT_XHCI) => {
            say(b"el controlador no es xHCI; se omite");
            return 0;
        }
        Err(step) => {
            say(b"fallo en la inicializacion del xHCI");
            return step;
        }
    };
    sys::print(b"usb-input: xHCI listo (");
    num(ctl.params.max_ports as u64);
    sys::print(b" puertos, ");
    num(ctl.params.max_slots as u64);
    sys::print(b" slots, contextos de ");
    num(ctl.params.context_bytes() as u64);
    sys::print(b" bytes)\n");
    // Dar tiempo a que los puertos vean el dispositivo tras el reset.
    let settle = ctl.clock.now() + 100;
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
                sys::print(b"usb-input: puerto ");
                num(port as u64);
                sys::print(b": ");
                sys::print(e.as_bytes());
                sys::print(b"\n");
            }
        }
    }
    sys::print(b"usb-input: dispositivo listo: ");
    sys::print(device_name(&ctl).as_bytes());
    sys::print(b"\n");

    let mut pending = Pending {
        buf: [(Event::default(), 0); RING],
        head: 0,
        len: 0,
    };
    let mut waiting: Option<(Handle, u64)> = None;
    let mut msg = [0u8; Request::LEN];
    let mut handles = [Handle(0); 1];
    let mut events = [WireEvent::default(); MAX_EVENTS];
    let mut times = [0u64; MAX_EVENTS];
    say(b"sirviendo peticiones");
    loop {
        ctl.poll(&mut pending);
        let now = ctl.clock.now();
        for _ in 0..8 {
            let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, 0) else {
                break;
            };
            if nh == 0 {
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
