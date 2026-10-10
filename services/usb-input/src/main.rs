//! usbd (Fase 25, segunda parte; ADR 0023): driver xHCI en ring 3 para
//! teclados y ratones USB HID con protocolo de arranque y para un
//! dispositivo de almacenamiento masivo (bulk-only, SCSI). Recibe el
//! contrato de driver de init (`SERVICE`, cfg, hardware, IRQ, BAR0) y, como
//! extras, `SYSTEM` (`READ`) para el reloj y el endpoint del servicio de
//! bloques. Enumera los puertos con dispositivo, configura cada interfaz HID
//! de arranque, sondea sus endpoints de interrupción y entrega los eventos
//! por IPC (`InputServiceProtocol v1`) como un único dispositivo de entrada;
//! el almacenamiento se sirve con el protocolo de bloques (`aricode-proto::block`).
#![no_std]
#![no_main]

use aricode_input::{Event, HidBootKeyboard, HidBootMouse, MAX_REPORT_EVENTS};
use aricode_proto::block::{
    Op as BlockOp, Reply as BlockReply, Request as BlockRequest, Status as BlockStatus, MAX_SECTORS,
};
use aricode_proto::input::{Event as WireEvent, Op, Reply, Request, Status, MAX_EVENTS};
use aricode_rt::sys::block::ClientBuffers;
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
    /// Input context del dispositivo (físico, virtual, bytes) y su contexto
    /// de slot, para configurar endpoints después de direccionarlo.
    input_ctx: (u64, u64, u64),
    slot_ctx: SlotContext,
}

/// Endpoint masivo de un dispositivo de almacenamiento.
struct Bulk {
    dci: u8,
    /// Dirección USB del endpoint (bit 7 = IN), para `CLEAR_FEATURE`.
    address: u8,
    ring: Ring,
    max_packet: u16,
}

/// Dispositivo de almacenamiento masivo bulk-only (uno como máximo).
struct Storage {
    device: usize,
    slot: u8,
    bulk_in: Bulk,
    bulk_out: Bulk,
    tag: u32,
    /// CBW, CSW y respuestas pequeñas (INQUIRY, sentido, capacidad) en el
    /// buffer DMA del driver: (físico, virtual).
    cbw: (u64, u64),
    csw: (u64, u64),
    small: (u64, u64),
    /// Sectores de 512 bytes.
    capacity: u64,
    /// El dispositivo admite `SYNCHRONIZE CACHE`; si lo rechaza como no
    /// soportado, el flush no tiene nada que vaciar.
    sync_cache: bool,
    /// Errores de E/S anunciados (se acotan para no inundar la consola).
    errors_reported: u8,
}

/// Por qué falló un comando SCSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScsiFail {
    /// Transferencia masiva fallida o CSW inválido.
    Transport,
    /// El dispositivo rechazó el comando (`REQUEST SENSE` dice por qué).
    Failed,
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
    storage: Option<Storage>,
    clock: Clock,
    keyboards: u8,
    mice: u8,
    /// Los puertos se enrutaron de los EHCI al xHCI (Intel): hay que dar
    /// más tiempo a que los dispositivos aparezcan.
    ports_switched: bool,
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
        let Some(bar) = bar_handle(mask, 0) else {
            say(b"sin BAR 0 o sin IRQ entre las capabilities");
            return Err(exit::MAP_BAR);
        };
        let base = Mmio(sys::mmio_map(bar).map_err(|_| {
            say(b"no se pudo mapear el BAR 0");
            exit::MAP_BAR
        })?);
        let cfg = Mmio(sys::mmio_map(CFG).map_err(|_| {
            say(b"no se pudo mapear el espacio de configuracion");
            exit::MAP_BAR
        })?);
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
        let region = DmaRegion::alloc(params.dma_arena_bytes()).map_err(|_| exit::DMA)?;
        let mut arena = Arena { region, next: 0 };

        // El firmware puede seguir siendo dueño del controlador (SMM):
        // pedirle que lo ceda antes de tocarlo.
        bios_handoff(&base, &clock);

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
        let ports_switched = intel_route_ports(&cfg);
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
            input_ctx: (in_phys, in_virt, in_len),
            slot_ctx,
        });

        // Descriptor de dispositivo en dos pasos (xHCI 4.3, paso 7): primero
        // solo 8 bytes, que caben en cualquier EP0, para conocer
        // bMaxPacketSize0; si difiere del supuesto se corrige el contexto con
        // Evaluate Context y solo entonces se pide el descriptor completo.
        let (buf_phys, buf_virt) = self.arena.alloc(512, 64).ok_or("sin DMA")?;
        self.control(
            index,
            SetupPacket::get_descriptor(aricode_xhci::request::DESC_DEVICE, 0, 8),
            Some((buf_phys, 8)),
        )
        .ok_or("GET_DESCRIPTOR(device, 8 bytes) fallo")?;
        // SAFETY: buffer DMA de 512 bytes del driver.
        let raw = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, 512) };
        let max_packet0 = raw[7] as u16;
        if raw[1] != aricode_xhci::request::DESC_DEVICE || max_packet0 == 0 {
            return Err("cabecera del descriptor de dispositivo invalida");
        }
        if max_packet0 != speed::default_ep0_max_packet(spd) && spd != speed::SUPER {
            let ep0_ctx = EndpointContext::control(max_packet0, ep0_ctx.ring_phys);
            write_input_context(
                // SAFETY: ídem.
                unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
                ctx_bytes,
                0b10,
                &slot_ctx,
                &[(1, ep0_ctx)],
            );
            let (code, _) = self
                .command(Trb::evaluate_context(in_phys, slot), 500)
                .ok_or("Evaluate Context sin respuesta")?;
            if code != completion::SUCCESS {
                return Err("Evaluate Context (tamano de paquete de EP0) fallo");
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
        .ok_or("GET_DESCRIPTOR(device) fallo")?;
        let desc = DeviceDescriptor::parse(raw).ok_or("descriptor de dispositivo invalido")?;
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
            // Sin HID: ¿almacenamiento masivo bulk-only (ADR 0023)?
            let Some((value, iface)) = parse_storage_interface(&raw[..total as usize]) else {
                return Err("sin interfaces HID de arranque ni de almacenamiento");
            };
            if self.storage.is_some() {
                return Err("ya hay un dispositivo de almacenamiento; se ignora");
            }
            self.control(index, SetupPacket::set_configuration(value), None)
                .ok_or("SET_CONFIGURATION fallo")?;
            return self.configure_storage(index, port, iface);
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

    /// Configura los endpoints masivos de una interfaz de almacenamiento,
    /// identifica el dispositivo (INQUIRY), espera a que esté listo y lee su
    /// capacidad. Solo se admiten bloques de 512 bytes.
    fn configure_storage(
        &mut self,
        index: usize,
        port: u8,
        iface: StorageInterface,
    ) -> Result<(), &'static str> {
        let ring_in = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("sin DMA")?;
        let ring_out = Ring::new(&mut self.arena, XFER_RING_TRBS).ok_or("sin DMA")?;
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
            let dev = self.devices[index].as_ref().ok_or("dispositivo")?;
            (dev.slot, dev.input_ctx, dev.slot_ctx)
        };
        let slot_ctx = SlotContext {
            context_entries: dci_in.max(dci_out),
            ..slot_ctx
        };
        let ctx_bytes = self.params.context_bytes();
        write_input_context(
            // SAFETY: input context del dispositivo dentro del buffer DMA.
            unsafe { core::slice::from_raw_parts_mut(in_virt as *mut u8, in_len as usize) },
            ctx_bytes,
            1 | 1 << dci_in | 1 << dci_out,
            &slot_ctx,
            &eps,
        );
        let (code, _) = self
            .command(Trb::configure_endpoint(in_phys, slot), 1000)
            .ok_or("Configure Endpoint sin respuesta")?;
        if code != completion::SUCCESS {
            return Err("Configure Endpoint (almacenamiento) fallo");
        }
        let cbw = self.arena.alloc(64, 64).ok_or("sin DMA")?;
        let csw = self.arena.alloc(64, 64).ok_or("sin DMA")?;
        let small = self.arena.alloc(64, 64).ok_or("sin DMA")?;
        self.storage = Some(Storage {
            device: index,
            slot,
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

    /// INQUIRY, TEST UNIT READY (con reintentos mientras el dispositivo
    /// arranca) y READ CAPACITY(10).
    fn storage_init(&mut self, port: u8) -> Result<(), &'static str> {
        let (small_phys, small_virt) = self.storage.as_ref().ok_or("almacenamiento")?.small;
        // SAFETY: buffer propio de 64 bytes en el DMA del driver.
        let small = || unsafe { core::slice::from_raw_parts(small_virt as *const u8, 64) };
        self.scsi(
            scsi::inquiry(),
            Some((small_phys, scsi::INQUIRY_LEN as u32, true)),
        )
        .map_err(|_| "INQUIRY fallo")?;
        let inq = scsi::Inquiry::parse(small()).ok_or("INQUIRY invalido")?;
        if inq.peripheral_type != 0 {
            return Err("no es un dispositivo de acceso directo (disco)");
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
                        return Err("TEST UNIT READY rechazado");
                    }
                }
                Err(ScsiFail::Transport) => return Err("TEST UNIT READY sin respuesta"),
            }
            let until = self.clock.now() + 100;
            while self.clock.now() < until {
                let _ = sys::irq_wait(IRQ, 10);
            }
        }
        if !ready {
            return Err("el dispositivo no llega a estar listo");
        }
        self.scsi(
            scsi::read_capacity10(),
            Some((small_phys, scsi::READ_CAPACITY_LEN as u32, true)),
        )
        .map_err(|_| "READ CAPACITY fallo")?;
        let (last_lba, block_len) =
            scsi::parse_read_capacity10(small()).ok_or("READ CAPACITY invalido")?;
        if block_len != 512 {
            return Err("bloques distintos de 512 bytes: no soportado");
        }
        let capacity = last_lba as u64 + 1;
        if let Some(st) = self.storage.as_mut() {
            st.capacity = capacity;
        }
        sys::print(b"usb-input: almacenamiento USB en puerto ");
        num(port as u64);
        sys::print(b": ");
        sys::print(inq.vendor.trim_ascii());
        sys::print(b" ");
        sys::print(inq.product.trim_ascii());
        sys::print(b", ");
        num(capacity);
        sys::print(b" sectores\n");
        Ok(())
    }

    /// Transferencia masiva síncrona por el endpoint IN u OUT del
    /// almacenamiento. `Ok(bytes no transferidos)` o el código de
    /// terminación del xHCI (`completion::STALL` incluido; 0 = sin evento).
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

    /// Recupera un endpoint masivo parado (STALL): Reset Endpoint, el
    /// anillo sigue desde donde va el productor y `CLEAR_FEATURE(HALT)`.
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

    /// Un comando SCSI por el transporte bulk-only: CBW, datos (si hay, en
    /// `(físico, bytes, hacia el anfitrión)`) y CSW. Un STALL en los datos o
    /// en el CSW se recupera como manda BOT 6.7; el estado lo da el CSW.
    fn scsi(&mut self, cdb: Cdb, data: Option<(u64, u32, bool)>) -> Result<(), ScsiFail> {
        let (tag, cbw_phys, cbw_virt, csw_phys, csw_virt) = {
            let st = self.storage.as_mut().ok_or(ScsiFail::Transport)?;
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
        // SAFETY: CBW en el buffer DMA del driver.
        unsafe { (cbw_virt as *mut [u8; Cbw::LEN]).write_volatile(cbw.encode()) };
        if self.bulk(false, cbw_phys, Cbw::LEN as u32, 1000).is_err() {
            self.recover_bulk(false);
            return Err(ScsiFail::Transport);
        }
        if let Some((phys, len, dir_in)) = data {
            match self.bulk(dir_in, phys, len, 5000) {
                Ok(_) => {}
                Err(completion::STALL) => self.recover_bulk(dir_in),
                Err(_) => return Err(ScsiFail::Transport),
            }
        }
        let mut csw = self.bulk(true, csw_phys, Csw::LEN as u32, 1000);
        if csw == Err(completion::STALL) {
            self.recover_bulk(true);
            csw = self.bulk(true, csw_phys, Csw::LEN as u32, 1000);
        }
        csw.map_err(|_| ScsiFail::Transport)?;
        // SAFETY: CSW en el buffer DMA del driver.
        let raw = unsafe { (csw_virt as *const [u8; Csw::LEN]).read_volatile() };
        let csw = Csw::parse(&raw).ok_or(ScsiFail::Transport)?;
        if csw.tag != tag {
            return Err(ScsiFail::Transport);
        }
        match csw.status {
            CswStatus::Passed => Ok(()),
            CswStatus::Failed => Err(ScsiFail::Failed),
            CswStatus::PhaseError => Err(ScsiFail::Transport),
        }
    }

    /// `REQUEST SENSE` tras un comando rechazado.
    fn sense(&mut self) -> Option<scsi::Sense> {
        let (phys, virt) = self.storage.as_ref()?.small;
        self.scsi(
            scsi::request_sense(),
            Some((phys, scsi::SENSE_LEN as u32, true)),
        )
        .ok()?;
        // SAFETY: buffer propio de 64 bytes en el DMA del driver.
        scsi::Sense::parse(unsafe { core::slice::from_raw_parts(virt as *const u8, 64) })
    }

    /// Lectura o escritura de `count` sectores en el buffer DMA del cliente;
    /// un rechazo por atención de unidad se reintenta una vez.
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
        for _ in 0..2 {
            match self.scsi(cdb, Some((phys, count as u32 * 512, !write))) {
                Ok(()) => return true,
                Err(ScsiFail::Failed) => {
                    let sense = self.sense();
                    if sense.map(|s| s.key) != Some(scsi::Sense::UNIT_ATTENTION) {
                        self.report_error(name, lba, sense);
                        return false;
                    }
                }
                Err(ScsiFail::Transport) => {
                    self.report_error(name, lba, None);
                    return false;
                }
            }
        }
        self.report_error(name, lba, None);
        false
    }

    /// Hace duraderas las escrituras (`SYNCHRONIZE CACHE`). Un dispositivo
    /// que no implementa el comando no tiene caché que vaciar: se anota una
    /// vez y el flush pasa a ser inmediato.
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
                    say(b"el dispositivo no admite SYNCHRONIZE CACHE; flush sin efecto");
                    true
                } else {
                    self.report_error(b"SYNCHRONIZE CACHE", 0, sense);
                    false
                }
            }
            Err(ScsiFail::Transport) => {
                self.report_error(b"SYNCHRONIZE CACHE", 0, None);
                false
            }
        }
    }

    /// Anuncia un comando fallido con su sentido (las primeras veces).
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
                sys::print(b" rechazado: sentido ");
                num(s.key as u64);
                sys::print(b"/");
                num(s.asc as u64);
                sys::print(b"/");
                num(s.ascq as u64);
                sys::print(b"\n");
            }
            None => sys::print(b" fallo en el transporte\n"),
        }
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

/// Cesión del controlador por el firmware (xHCI 4.22.1, capability USB
/// Legacy Support): se pide la propiedad al sistema, se espera a que el
/// firmware suelte la suya y se apagan sus SMI. Sin la capability (QEMU) no
/// hay nada que hacer.
fn bios_handoff(base: &Mmio, clock: &Clock) {
    const LEGACY_SUPPORT: u32 = 1;
    const BIOS_OWNED: u32 = 1 << 16;
    const OS_OWNED: u32 = 1 << 24;
    /// Bits reservados del control/estado heredado que se conservan.
    const KEEP: u32 = 0x7 << 1 | 0xFF << 5 | 0x7 << 17;
    /// Bits de estado (escribir 1 los limpia).
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
                    b"el firmware cedio el xHCI"
                } else {
                    b"el firmware no cede el xHCI; se continua"
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

/// Intel 7 series (Panther Point) y posteriores: los puertos USB 2 y 3
/// compartidos nacen en los EHCI y el xHCI solo los ve si el sistema los
/// enruta (registros XUSB2PR y USB3_PSSEN del espacio de configuración,
/// como `usb_enable_intel_xhci_ports` en Linux). Devuelve si cambió algo.
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
    sys::print(b"usb-input: puertos Intel enrutados al xHCI (USB2 ");
    num(usb2 as u64);
    sys::print(b", USB3 ");
    num(usb3 as u64);
    sys::print(b")\n");
    true
}

/// Una petición del protocolo de bloques sobre el almacenamiento USB.
fn serve_block(ctl: &mut Controller, buffers: &mut ClientBuffers, msg: &[u8], handles: &[Handle]) {
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
                match buffers.map(handles[1]) {
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
            sys::print(b"usb-input: fallo en la inicializacion del xHCI: ");
            sys::print(match step {
                exit::MAP_BAR => b"capabilities o mapeo del BAR\n",
                exit::DMA => b"memoria DMA (buffer insuficiente)\n",
                exit::RESET_TIMEOUT => b"el controlador no completa el reset\n",
                exit::START_TIMEOUT => b"el controlador no arranca\n",
                _ => b"paso desconocido\n",
            });
            return step;
        }
    };
    sys::print(b"usb-input: xHCI listo (");
    num(ctl.params.max_ports as u64);
    sys::print(b" puertos, ");
    num(ctl.params.max_slots as u64);
    sys::print(b" slots, contextos de ");
    num(ctl.params.context_bytes() as u64);
    sys::print(b" bytes, ");
    num(ctl.params.scratchpad_buffers as u64);
    sys::print(b" scratchpads)\n");
    // Dar tiempo a que los puertos vean el dispositivo tras el reset (más si
    // acaban de cambiar de controlador).
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
    if ctl.storage.is_none() {
        say(b"sin almacenamiento USB");
    }

    // Segundo extra (opcional; init lo pasa, el banco de pruebas del kernel
    // no): endpoint del servicio de bloques (ADR 0023). Sin el, ese numero
    // de handle puede ser uno creado por el propio driver (buffers DMA), asi
    // que se comprueba el tipo del objeto.
    let block_ep = extra_handle(mask, 1);
    let has_block = sys::object_kind(block_ep) == Ok(abi::kind::ENDPOINT);
    let endpoints: &[Handle] = if has_block {
        &[SERVICE, block_ep]
    } else {
        &[SERVICE]
    };
    let mut buffers = ClientBuffers::new();
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
    say(b"sirviendo peticiones");
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
                serve_block(&mut ctl, &mut buffers, &msg[..n], &handles[..nh]);
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
