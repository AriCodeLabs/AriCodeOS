//! xHCI (Fase 25, segunda parte): todo lo que el driver de ring 3 necesita
//! saber del controlador y del protocolo USB sin tocar hardware:
//! desplazamientos y bits de registro, TRB (comandos, transferencias y
//! eventos), anillos con bit de ciclo y TRB de enlace, contextos de slot y
//! endpoint (de 32 o 64 bytes), tabla de segmentos del anillo de eventos,
//! paquetes SETUP y descriptores USB. Se prueba en host; el driver solo
//! copia estas estructuras a memoria DMA y escribe MMIO.

#![no_std]

/// Registros de capacidad (desde la base del BAR0).
pub mod cap {
    pub const CAPLENGTH: u64 = 0x00;
    pub const HCIVERSION: u64 = 0x02;
    pub const HCSPARAMS1: u64 = 0x04;
    pub const HCSPARAMS2: u64 = 0x08;
    pub const HCCPARAMS1: u64 = 0x10;
    pub const DBOFF: u64 = 0x14;
    pub const RTSOFF: u64 = 0x18;
}

/// Registros operacionales (desde `base + CAPLENGTH`).
pub mod op {
    pub const USBCMD: u64 = 0x00;
    pub const USBSTS: u64 = 0x04;
    pub const PAGESIZE: u64 = 0x08;
    pub const CRCR: u64 = 0x18;
    pub const DCBAAP: u64 = 0x30;
    pub const CONFIG: u64 = 0x38;
    /// Registros de puerto: `PORTSC` del puerto `n` (desde 1) en `0x400 + 0x10 * (n - 1)`.
    pub const PORT_BASE: u64 = 0x400;
    pub const PORT_STRIDE: u64 = 0x10;

    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;
    pub const CMD_INTE: u32 = 1 << 2;
    pub const STS_HALTED: u32 = 1 << 0;
    pub const STS_EINT: u32 = 1 << 3;
    pub const STS_PORT_CHANGE: u32 = 1 << 4;
    pub const STS_NOT_READY: u32 = 1 << 11;
    /// Ring Cycle State del anillo de comandos.
    pub const CRCR_RCS: u64 = 1 << 0;

    pub const PORTSC_CCS: u32 = 1 << 0;
    pub const PORTSC_PED: u32 = 1 << 1;
    pub const PORTSC_RESET: u32 = 1 << 4;
    pub const PORTSC_POWER: u32 = 1 << 9;
    pub const PORTSC_CSC: u32 = 1 << 17;
    pub const PORTSC_PEC: u32 = 1 << 18;
    pub const PORTSC_PRC: u32 = 1 << 21;
    /// Bits RW1C de cambio: se escriben a 1 para limpiarlos, nunca por accidente.
    pub const PORTSC_CHANGE_BITS: u32 = 0x00FE_0000;

    pub fn port_speed(portsc: u32) -> u8 {
        ((portsc >> 10) & 0xF) as u8
    }
}

/// Registros de runtime (desde `base + RTSOFF`): interrupter 0.
pub mod rt {
    pub const IMAN: u64 = 0x20;
    pub const IMOD: u64 = 0x24;
    pub const ERSTSZ: u64 = 0x28;
    pub const ERSTBA: u64 = 0x30;
    pub const ERDP: u64 = 0x38;
    pub const IMAN_PENDING: u32 = 1 << 0;
    pub const IMAN_ENABLE: u32 = 1 << 1;
    /// Event Handler Busy (RW1C) en ERDP.
    pub const ERDP_EHB: u64 = 1 << 3;
}

/// Velocidades del campo Port Speed de PORTSC y del contexto de slot.
pub mod speed {
    pub const FULL: u8 = 1;
    pub const LOW: u8 = 2;
    pub const HIGH: u8 = 3;
    pub const SUPER: u8 = 4;

    /// Tamaño máximo de paquete del endpoint 0 por defecto para cada velocidad.
    pub fn default_ep0_max_packet(speed: u8) -> u16 {
        match speed {
            LOW | FULL => 8,
            HIGH => 64,
            _ => 512,
        }
    }
}

/// HCSPARAMS1 / HCSPARAMS2 / HCCPARAMS1 decodificados.
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

    /// Bytes del buffer DMA que necesita el driver: 64 KiB para DCBAA,
    /// anillos, ERST y contextos de los dispositivos, más una página por cada
    /// scratchpad que pida el controlador (los Intel de 2012 piden decenas;
    /// QEMU ninguno) y una de holgura por la alineación de los scratchpads.
    pub fn dma_arena_bytes(&self) -> u64 {
        const BASE: u64 = 64 * 1024;
        const PAGE: u64 = 4096;
        BASE + (self.scratchpad_buffers as u64 + 1) * PAGE
    }

    /// Bytes de cada contexto (slot o endpoint).
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

/// Tipos de TRB (bits 15:10 de `control`).
pub mod trb_type {
    pub const NORMAL: u32 = 1;
    pub const SETUP_STAGE: u32 = 2;
    pub const DATA_STAGE: u32 = 3;
    pub const STATUS_STAGE: u32 = 4;
    pub const LINK: u32 = 6;
    pub const ENABLE_SLOT: u32 = 9;
    pub const ADDRESS_DEVICE: u32 = 11;
    pub const CONFIGURE_ENDPOINT: u32 = 12;
    pub const EVALUATE_CONTEXT: u32 = 13;
    pub const RESET_ENDPOINT: u32 = 14;
    pub const SET_TR_DEQUEUE: u32 = 16;
    pub const NOOP_COMMAND: u32 = 23;
    pub const TRANSFER_EVENT: u32 = 32;
    pub const COMMAND_COMPLETION: u32 = 33;
    pub const PORT_STATUS_CHANGE: u32 = 34;
}

/// Códigos de terminación de los eventos.
pub mod completion {
    pub const SUCCESS: u8 = 1;
    /// El endpoint respondió STALL (queda parado hasta Reset Endpoint).
    pub const STALL: u8 = 6;
    pub const SHORT_PACKET: u8 = 13;
}

pub const TRB_CYCLE: u32 = 1 << 0;
const TRB_TOGGLE_CYCLE: u32 = 1 << 1;
/// Interrupt on Short Packet.
const TRB_ISP: u32 = 1 << 2;
/// Chain: el TRB siguiente pertenece al mismo TD.
const TRB_CHAIN: u32 = 1 << 4;
/// Interrupt on Completion.
const TRB_IOC: u32 = 1 << 5;
/// Immediate Data (el parámetro es el dato, no un puntero).
const TRB_IDT: u32 = 1 << 6;
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
    /// Endpoint (DCI) de un Transfer Event.
    pub fn endpoint_id(&self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }
    /// Bytes no transferidos (Transfer Event).
    pub fn residual(&self) -> u32 {
        self.status & 0x00FF_FFFF
    }
    /// Puerto de un Port Status Change Event.
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
    /// Reset Endpoint: saca al endpoint `dci` del estado parado (tras STALL).
    pub fn reset_endpoint(slot: u8, dci: u8) -> Self {
        let mut t = Self::with_type(trb_type::RESET_ENDPOINT);
        t.control |= (dci as u32) << 16 | (slot as u32) << 24;
        t
    }
    /// Set TR Dequeue Pointer: el controlador sigue el anillo de `dci` desde
    /// `pointer` (dirección del TRB con el bit de ciclo del productor).
    pub fn set_tr_dequeue(slot: u8, dci: u8, pointer: u64, cycle: bool) -> Self {
        let mut t = Self::with_type(trb_type::SET_TR_DEQUEUE);
        t.parameter = (pointer & !0xF) | cycle as u64;
        t.control |= (dci as u32) << 16 | (slot as u32) << 24;
        t
    }
    /// Setup Stage con los 8 bytes del paquete SETUP como dato inmediato.
    /// `TRT`: 0 sin datos, 2 datos OUT, 3 datos IN.
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
    /// Status Stage; su dirección es la contraria a la de los datos (o IN si
    /// no hubo datos). Con `IOC` para saber que la transferencia acabó.
    pub fn status_stage(device_to_host: bool) -> Self {
        let mut t = Self::with_type(trb_type::STATUS_STAGE);
        if device_to_host {
            t.control |= TRB_DIR_IN;
        }
        t.control |= TRB_IOC;
        t
    }
    /// Transferencia normal (endpoint de interrupción) con aviso al terminar,
    /// también si el paquete es corto.
    pub fn normal(buffer_phys: u64, len: u32) -> Self {
        Self::normal_part(buffer_phys, len, 0, false)
    }

    /// Un TRB de un TD de varios: `td_size` = paquetes que quedan tras este
    /// TRB (acotado a 31), `chain` si no es el último. Solo el último avisa.
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

/// Anillo de productor (comandos o transferencias): `size - 1` TRB útiles y
/// un TRB de enlace al final con `Toggle Cycle`. Guarda el estado del
/// productor; el contenido lo escribe el llamador donde `next` le diga.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProducerRing {
    pub phys: u64,
    pub size: usize,
    pub enqueue: usize,
    pub cycle: bool,
}

/// Lo que hay que escribir en el anillo para encolar un TRB: la posición y
/// los bytes (con el bit de ciclo correcto) y, si toca, el TRB de enlace
/// actualizado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enqueued {
    pub index: usize,
    pub trb: Trb,
    /// Dirección física del TRB (para casar eventos de terminación).
    pub phys: u64,
    /// El enlace hay que reescribirlo con este ciclo antes de dar la vuelta.
    pub link: Option<(usize, Trb)>,
}

impl ProducerRing {
    pub fn new(phys: u64, size: usize) -> Self {
        assert!(size >= 2, "anillo demasiado pequeno");
        Self {
            phys,
            size,
            enqueue: 0,
            cycle: true,
        }
    }

    /// TRB de enlace inicial (último slot, apunta al principio, con toggle).
    pub fn initial_link(&self) -> (usize, Trb) {
        let mut link = Trb::link(self.phys, true);
        // El enlace nace con el ciclo del productor: la primera vuelta lo
        // encuentra "suyo" solo cuando el productor lo alcance y lo reescriba.
        link.control |= TRB_CYCLE;
        link.control &= !TRB_CYCLE;
        (self.size - 1, link)
    }

    /// Encola `trb` (sin bit de ciclo) y avanza.
    pub fn push(&mut self, mut trb: Trb) -> Enqueued {
        trb.control = (trb.control & !TRB_CYCLE) | if self.cycle { TRB_CYCLE } else { 0 };
        let index = self.enqueue;
        let phys = self.phys + (index * Trb::LEN) as u64;
        self.enqueue += 1;
        let mut link = None;
        if self.enqueue == self.size - 1 {
            // Enlace: se le da el ciclo actual para que el consumidor lo siga y
            // el productor pasa al ciclo contrario en la vuelta.
            let mut l = Trb::link(self.phys, true);
            if self.cycle {
                l.control |= TRB_CYCLE;
            }
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

/// Consumidor del anillo de eventos (un segmento): sabe qué TRB es el
/// siguiente y con qué bit de ciclo debe venir para ser válido.
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

    /// Si `trb` (leído en `dequeue`) es un evento nuevo, avanza y lo devuelve.
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

    /// Valor a escribir en ERDP tras procesar: puntero actual con EHB.
    pub fn dequeue_pointer(&self) -> u64 {
        (self.phys + (self.dequeue * Trb::LEN) as u64) | rt::ERDP_EHB
    }
}

/// Entrada de la Event Ring Segment Table (16 bytes).
pub fn erst_entry(segment_phys: u64, trbs: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&segment_phys.to_le_bytes());
    b[8..10].copy_from_slice(&trbs.to_le_bytes());
    b
}

/// Tipos de endpoint del contexto (bits 5:3 de la palabra 1).
pub mod ep_type {
    pub const BULK_OUT: u32 = 2;
    pub const INTERRUPT_OUT: u32 = 3;
    pub const CONTROL: u32 = 4;
    pub const BULK_IN: u32 = 6;
    pub const INTERRUPT_IN: u32 = 7;
}

/// Un TD de transferencia masiva como TRBs normales: ninguno cruza un límite
/// de 64 KiB (xHCI 4.11.7), los no finales encadenan con el tamaño de TD
/// (paquetes pendientes) y solo el último avisa. Devuelve cuántos TRBs hay;
/// `None` si no caben en `out`.
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

/// Índice de contexto de dispositivo (DCI) de un endpoint USB.
pub fn dci(endpoint_number: u8, device_to_host: bool) -> u8 {
    if endpoint_number == 0 {
        1
    } else {
        endpoint_number * 2 + device_to_host as u8
    }
}

/// Contexto de slot (las 8 primeras palabras; el resto es reservado).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotContext {
    pub route_string: u32,
    pub speed: u8,
    /// Último DCI válido del dispositivo.
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

/// Contexto de endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointContext {
    pub ep_type: u32,
    pub max_packet_size: u16,
    /// Intervalo de sondeo: `2^interval * 125 us`.
    pub interval: u8,
    pub error_count: u8,
    /// Anillo de transferencia con su Dequeue Cycle State.
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

    /// Endpoint masivo (bulk) IN u OUT.
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

/// Intervalo de un endpoint de interrupción a partir de `bInterval`.
/// Alta velocidad y superior: `2^(bInterval-1)` microframes. Baja y
/// completa: `bInterval` está en ms y el campo es `floor(log2(ms * 8))`,
/// acotado a 3..=10 (xHCI 6.2.3.6, tabla 6-12: se redondea hacia abajo, al
/// periodo igual o menor, nunca mayor).
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

/// Escribe un Input Context: control (drop/add flags), slot y los endpoints
/// dados por DCI, con contextos de `context_bytes` cada uno. Devuelve los
/// bytes usados.
pub fn write_input_context(
    out: &mut [u8],
    context_bytes: usize,
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
    // Input Control Context: palabra 0 drop flags, palabra 1 add flags.
    let mut icc = [0u32; 8];
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

/// Paquete SETUP de una petición de control (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

pub mod request {
    pub const CLEAR_FEATURE: u8 = 1;
    pub const GET_DESCRIPTOR: u8 = 6;
    pub const SET_CONFIGURATION: u8 = 9;
    /// Clase HID: protocolo de arranque (0) o de informe (1).
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
    /// `CLEAR_FEATURE(ENDPOINT_HALT)` sobre el endpoint `address` (bit 7 =
    /// IN), tras un STALL en una transferencia masiva.
    pub fn clear_endpoint_halt(address: u8) -> Self {
        Self {
            request_type: 0x02,
            request: request::CLEAR_FEATURE,
            value: 0,
            index: address as u16,
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
    /// HID `SET_PROTOCOL`: 0 = protocolo de arranque (informes fijos).
    pub fn hid_set_protocol(interface: u8, protocol: u16) -> Self {
        Self {
            request_type: 0x21,
            request: request::HID_SET_PROTOCOL,
            value: protocol,
            index: interface as u16,
            length: 0,
        }
    }
    /// HID `SET_IDLE` con duración 0: informe solo cuando cambia algo.
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

/// Descriptor de dispositivo (lo que el driver usa).
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

/// Interfaz HID de arranque encontrada en un descriptor de configuración.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidInterface {
    pub interface: u8,
    /// 1 teclado, 2 ratón (protocolo de arranque).
    pub protocol: u8,
    pub endpoint: u8,
    pub max_packet_size: u16,
    pub b_interval: u8,
}

/// Interfaz de almacenamiento masivo (clase 8, subclase 6 SCSI transparente,
/// protocolo 0x50 bulk-only) con sus endpoints masivos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageInterface {
    pub interface: u8,
    pub bulk_in: u8,
    pub bulk_out: u8,
    pub max_packet_in: u16,
    pub max_packet_out: u16,
}

/// Primera interfaz de almacenamiento bulk-only de un descriptor de
/// configuración completo, con el valor de configuración.
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

/// Recorre un descriptor de configuración completo y devuelve su valor de
/// configuración y las interfaces HID de arranque (clase 3, subclase 1) con
/// su endpoint de interrupción IN.
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
        // Sin scratchpads basta la base; con 35, 35 páginas más una de holgura.
        assert_eq!(Params::decode(0, 0, 4).dma_arena_bytes(), 64 * 1024 + 4096);
        assert_eq!(
            Params::decode(0, 1 << 21 | 3 << 27, 4).dma_arena_bytes(),
            64 * 1024 + 36 * 4096
        );
        assert_eq!(speed::default_ep0_max_packet(speed::FULL), 8);
        assert_eq!(speed::default_ep0_max_packet(speed::HIGH), 64);
        assert_eq!(speed::default_ep0_max_packet(speed::SUPER), 512);
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
            "10 ms se redondea hacia abajo a 8 ms"
        );
        assert_eq!(interrupt_interval(speed::FULL, 3), 4, "3 ms -> 2 ms");
        assert_eq!(interrupt_interval(speed::LOW, 1), 3);
        assert_eq!(interrupt_interval(speed::LOW, 0), 3);
        assert_eq!(interrupt_interval(speed::FULL, 129), 10, "tope del campo");
        assert_eq!(interrupt_interval(speed::FULL, 255), 10);
        // Auditoria 4836023, hallazgo 2: campo en rango y periodo nunca
        // mayor que el pedido (salvo el minimo de 1 ms).
        for b in 1..=255u8 {
            let i = interrupt_interval(speed::FULL, b);
            assert!((3..=10).contains(&i));
            assert!(
                (1u32 << i) <= (b as u32) * 8,
                "nunca un periodo mayor: {b} -> {i}"
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
        assert_eq!((s.control >> 16) & 3, 3, "TRT = datos IN");
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
        // Evento de terminación de comando: código 1, slot 3.
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
        // Al llenar el último util se reescribe el enlace con el ciclo actual (1)
        // y el productor pasa al ciclo 0.
        let (li, l) = c.link.expect("enlace");
        assert_eq!(li, 3);
        assert!(l.cycle());
        assert_eq!(r.enqueue, 0);
        assert!(!r.cycle);
        let d = r.push(Trb::noop_command());
        assert_eq!(d.index, 0);
        assert!(!d.trb.cycle(), "segunda vuelta con ciclo 0");
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
        assert!(!e.cycle, "vuelta: ahora los eventos nuevos llevan ciclo 0");
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
        assert_eq!(w[2], 0x3001, "dequeue con DCS = 1");
        assert_eq!(w[4], 8);
        let int = EndpointContext::interrupt_in(8, 6, 0x4000);
        let w = int.to_words();
        assert_eq!(w[0], 6 << 16);
        assert_eq!(w[1] >> 3 & 7, ep_type::INTERRUPT_IN);
        assert_eq!(w[4], 8 | 8 << 16);
        let mut buf = [0xFFu8; 33 * 64];
        let used = write_input_context(&mut buf, 32, 0b1011, &slot, &[(1, ep0), (3, int)]);
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
            "el resto queda a cero"
        );
        let used64 = write_input_context(&mut buf, 64, 0b11, &slot, &[(1, ep0)]);
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
        // Configuración con dos interfaces HID de arranque: teclado (ep 1 IN,
        // 8 bytes, 7 ms) y ratón (ep 2 IN, 4 bytes, 10 ms), más un endpoint
        // OUT que se ignora.
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
        // Sin interfaz de almacenamiento.
        assert!(parse_storage_interface(&cfg).is_none());
    }

    #[test]
    fn storage_interface_and_bulk_endpoints() {
        // Pendrive: interfaz 0 de clase 8/6/0x50 con bulk IN 0x81 (512) y
        // bulk OUT 0x02 (512), precedida de una interfaz de otra clase.
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
        let t = Trb::reset_endpoint(3, 2);
        assert_eq!((t.trb_type(), t.slot_id(), t.endpoint_id()), (14, 3, 2));
        let t = Trb::set_tr_dequeue(3, 2, 0x2340, true);
        assert_eq!((t.trb_type(), t.parameter), (16, 0x2341));
    }

    #[test]
    fn bulk_td_never_crosses_64k() {
        let mut out = [Trb::noop_command(); 4];
        // 8 KiB que cruzan un límite de 64 KiB: dos TRBs, el primero encadenado
        // con los paquetes (512 B) que quedan, el último con IOC.
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
        // Sin cruce: un solo TRB con IOC.
        assert_eq!(bulk_trbs(0x1000, 31, 512, &mut out), Some(1));
        assert_ne!(out[0].control & TRB_IOC, 0);
        assert_eq!(bulk_trbs(0x1000, 0, 512, &mut out), None);
        assert_eq!(bulk_trbs(0x1_F000, 8192, 512, &mut out[..1]), None);
    }

    extern crate std;
    use std::vec::Vec;
}
