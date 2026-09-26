//! Driver virtio-blk (Fase 18) y servicio de bloques (Fase 19) en ring 3.
//!
//! Recibe de `init`, en este orden, el endpoint de servicio, el espacio de
//! configuración PCI, la capacidad de hardware, la IRQ MSI-X y los BARs
//! presentes (`arg` es la máscara de BARs). Inicializa el dispositivo por la
//! interfaz moderna, crea la cola 0 en memoria DMA, lee la GPT y atiende por
//! IPC (`aricode-proto::block`) peticiones de otros procesos, que aportan su
//! propio buffer DMA: el dispositivo lee y escribe directamente en él.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_proto::block::{Op, Reply, Request, Status, MAX_SECTORS};
use aricode_virtio::{
    blk, cfg_type, common, find_structures, status, QueueLayout, Virtqueue, FEATURE_VERSION_1,
};
use user_testprogs::sys::{self, Error, Handle, TIMEOUT_INFINITE};

/// Endpoint de servicio (creado por init) y capabilities del dispositivo.
const SERVICE: Handle = Handle(0x0001_0004);
const CFG: Handle = Handle(0x0001_0005);
const HW: Handle = Handle(0x0001_0006);
const IRQ: Handle = Handle(0x0001_0007);
const FIRST_BAR_HANDLE: u32 = 0x0001_0008;

/// Máscara que pasa `init` en `arg`: bit 15 = IRQ presente, bits 0..5 = BAR i
/// presente. Los handles de BAR van seguidos en ese orden tras la IRQ.
fn bar_handle(mask: u64, bar: u8) -> Option<Handle> {
    if mask & (1 << bar) == 0 || mask & (1 << 15) == 0 {
        return None;
    }
    let before = (mask & ((1u64 << bar) - 1)).count_ones();
    Some(Handle(FIRST_BAR_HANDLE + before))
}

fn say(msg: &[u8]) {
    sys::print(b"virtio-blk: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Registros MMIO de una estructura virtio mapeada.
struct Mmio(u64);

impl Mmio {
    fn r8(&self, off: u64) -> u8 {
        // SAFETY: dentro de la region mapeada por `mmio_map`.
        unsafe { ((self.0 + off) as *const u8).read_volatile() }
    }
    fn r16(&self, off: u64) -> u16 {
        // SAFETY: idem.
        unsafe { ((self.0 + off) as *const u16).read_volatile() }
    }
    fn r32(&self, off: u64) -> u32 {
        // SAFETY: idem.
        unsafe { ((self.0 + off) as *const u32).read_volatile() }
    }
    fn r64(&self, off: u64) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }
    fn w8(&self, off: u64, v: u8) {
        // SAFETY: idem.
        unsafe { ((self.0 + off) as *mut u8).write_volatile(v) }
    }
    fn w16(&self, off: u64, v: u16) {
        // SAFETY: idem.
        unsafe { ((self.0 + off) as *mut u16).write_volatile(v) }
    }
    fn w32(&self, off: u64, v: u32) {
        // SAFETY: idem.
        unsafe { ((self.0 + off) as *mut u32).write_volatile(v) }
    }
    fn w64(&self, off: u64, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }
}

struct Device {
    common: Mmio,
    notify: Mmio,
    notify_multiplier: u64,
    device_cfg: Mmio,
    queue: Virtqueue,
    queue_notify_off: u64,
    /// Buffer DMA propio: cabecera (16), un sector de datos (512) y estado (1).
    req_virt: u64,
    req_phys: u64,
}

fn setup(mask: u64) -> Result<Device, u64> {
    // 1. Estructuras virtio a partir del espacio de configuracion.
    let cfg_virt = sys::mmio_map(CFG).map_err(|_| 10u64)?;
    let mut cfg = [0u8; 256];
    for (i, b) in cfg.iter_mut().enumerate() {
        // SAFETY: pagina de configuracion mapeada.
        *b = unsafe { ((cfg_virt + i as u64) as *const u8).read_volatile() };
    }
    let structures = find_structures(&cfg);
    let common_s = structures[cfg_type::COMMON as usize].ok_or(11u64)?;
    let notify_s = structures[cfg_type::NOTIFY as usize].ok_or(12u64)?;
    let device_s = structures[cfg_type::DEVICE as usize].ok_or(13u64)?;
    let bar_virt = |bar: u8| sys::mmio_map(bar_handle(mask, bar).ok_or(14u64)?).map_err(|_| 14u64);
    let common_base = bar_virt(common_s.bar)?;
    let notify_base = if notify_s.bar == common_s.bar {
        common_base
    } else {
        bar_virt(notify_s.bar)?
    };
    let device_base = if device_s.bar == common_s.bar {
        common_base
    } else {
        bar_virt(device_s.bar)?
    };
    let common = Mmio(common_base + common_s.offset as u64);
    let notify = Mmio(notify_base + notify_s.offset as u64);
    let device_cfg = Mmio(device_base + device_s.offset as u64);

    // 2. Negociacion (virtio 1.x, seccion 3.1.1).
    common.w8(common::DEVICE_STATUS, 0);
    while common.r8(common::DEVICE_STATUS) != 0 {}
    common.w8(common::DEVICE_STATUS, status::ACKNOWLEDGE);
    common.w8(common::DEVICE_STATUS, status::ACKNOWLEDGE | status::DRIVER);
    common.w32(common::DEVICE_FEATURE_SELECT, 0);
    let f_lo = common.r32(common::DEVICE_FEATURE) as u64;
    common.w32(common::DEVICE_FEATURE_SELECT, 1);
    let f_hi = (common.r32(common::DEVICE_FEATURE) as u64) << 32;
    if (f_lo | f_hi) & FEATURE_VERSION_1 == 0 {
        return Err(15);
    }
    // Solo VERSION_1: sin caracteristicas opcionales todavia.
    common.w32(common::DRIVER_FEATURE_SELECT, 0);
    common.w32(common::DRIVER_FEATURE, 0);
    common.w32(common::DRIVER_FEATURE_SELECT, 1);
    common.w32(common::DRIVER_FEATURE, (FEATURE_VERSION_1 >> 32) as u32);
    common.w8(
        common::DEVICE_STATUS,
        status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
    );
    if common.r8(common::DEVICE_STATUS) & status::FEATURES_OK == 0 {
        return Err(16);
    }

    // 3. Cola 0 en memoria DMA.
    common.w16(common::QUEUE_SELECT, 0);
    let size = common.r16(common::QUEUE_SIZE).min(64);
    if size == 0 {
        return Err(17);
    }
    let layout = QueueLayout::new(size);
    let qdma = sys::dma_alloc(HW, layout.total_len as u64).map_err(|_| 18u64)?;
    let qvirt = sys::mmio_map(qdma).map_err(|_| 19u64)?;
    let qphys = sys::dma_phys(qdma).map_err(|_| 20u64)?;
    // SAFETY: buffer DMA a cero, alineado a pagina, exclusivo de la cola.
    let queue = unsafe { Virtqueue::new(layout, qvirt as *mut u8, qphys) };
    common.w16(common::QUEUE_SIZE, size);
    common.w64(common::QUEUE_DESC, queue.desc_phys());
    common.w64(common::QUEUE_DRIVER, queue.avail_phys());
    common.w64(common::QUEUE_DEVICE, queue.used_phys());
    common.w16(common::QUEUE_MSIX_VECTOR, 0);
    if common.r16(common::QUEUE_MSIX_VECTOR) != 0 {
        return Err(21);
    }
    common.w16(common::MSIX_CONFIG, 0xFFFF);
    let queue_notify_off = common.r16(common::QUEUE_NOTIFY_OFF) as u64;
    common.w16(common::QUEUE_ENABLE, 1);
    common.w8(
        common::DEVICE_STATUS,
        status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
    );

    let rdma = sys::dma_alloc(HW, 4096).map_err(|_| 22u64)?;
    let req_virt = sys::mmio_map(rdma).map_err(|_| 23u64)?;
    let req_phys = sys::dma_phys(rdma).map_err(|_| 24u64)?;
    Ok(Device {
        common,
        notify,
        notify_multiplier: notify_s.notify_off_multiplier as u64,
        device_cfg,
        queue,
        queue_notify_off,
        req_virt,
        req_phys,
    })
}

impl Device {
    fn capacity(&self) -> u64 {
        self.device_cfg.r64(blk::CFG_CAPACITY)
    }

    /// Una peticion sincrona: cabecera y estado en el buffer propio, datos en
    /// `(data_phys, data_len)`; espera la interrupcion MSI-X y comprueba el estado.
    fn request(
        &mut self,
        header: blk::RequestHeader,
        data_phys: u64,
        data_len: u32,
        device_writes: bool,
    ) -> Result<(), u64> {
        // SAFETY: buffer DMA de 4 KiB mapeado RW.
        unsafe {
            core::ptr::copy_nonoverlapping(
                header.to_bytes().as_ptr(),
                self.req_virt as *mut u8,
                16,
            );
            ((self.req_virt + 528) as *mut u8).write_volatile(0xFF);
        }
        let head = self
            .queue
            .add(&[
                (self.req_phys, 16, false),
                (data_phys, data_len, device_writes),
                (self.req_phys + 528, 1, true),
            ])
            .ok_or(30u64)?;
        self.notify
            .w16(self.queue_notify_off * self.notify_multiplier, 0);
        loop {
            if let Some((done, _)) = self.queue.pop_used() {
                if done != head {
                    return Err(31);
                }
                break;
            }
            match sys::irq_wait(IRQ, 2000) {
                Ok(_) => {}
                Err(Error::Timeout) => return Err(32),
                Err(_) => return Err(33),
            }
        }
        // SAFETY: byte de estado escrito por el dispositivo.
        match unsafe { ((self.req_virt + 528) as *const u8).read_volatile() } {
            blk::STATUS_OK => Ok(()),
            _ => Err(34),
        }
    }

    /// Lee un sector en el buffer propio (para la GPT).
    fn read_own(&mut self, lba: u64) -> Result<&[u8], u64> {
        self.request(blk::RequestHeader::read(lba), self.req_phys + 16, 512, true)?;
        // SAFETY: 512 bytes de datos dentro del buffer DMA.
        Ok(unsafe { core::slice::from_raw_parts((self.req_virt + 16) as *const u8, 512) })
    }
}

/// Lee la GPT y anuncia las particiones.
fn report_partitions(dev: &mut Device) {
    let mut buf = [0u8; 20];
    let header = match dev
        .read_own(1)
        .ok()
        .and_then(|s| aricode_gpt::Header::parse(s).ok())
    {
        Some(h) => h,
        None => {
            say(b"sin GPT");
            return;
        }
    };
    // La tabla (16 KiB) va al heap: la pila de usuario es pequeña.
    let mut table = alloc::vec![0u8; 128 * aricode_gpt::ENTRY_LEN];
    let sectors = header.entries_bytes() / 512;
    for i in 0..sectors.min(32) {
        match dev.read_own(header.entries_lba + i as u64) {
            Ok(s) => table[i * 512..(i + 1) * 512].copy_from_slice(s),
            Err(_) => {
                say(b"error leyendo la tabla GPT");
                return;
            }
        }
    }
    let mut parts = [None; 8];
    match aricode_gpt::partitions(&header, &table, &mut parts) {
        Ok(n) => {
            sys::print(b"virtio-blk: GPT con ");
            sys::print(user_testprogs::fmt_u64(n as u64, &mut buf));
            sys::print(b" particion(es)\n");
            for p in parts.iter().flatten() {
                sys::print(b"virtio-blk:   ");
                sys::print(p.name_str().as_bytes());
                sys::print(b" LBA ");
                sys::print(user_testprogs::fmt_u64(p.first_lba, &mut buf));
                sys::print(b"..");
                sys::print(user_testprogs::fmt_u64(p.last_lba, &mut buf));
                sys::print(b"\n");
            }
        }
        Err(_) => say(b"tabla GPT corrupta"),
    }
}

/// Mapeos de buffers DMA de clientes, reutilizados por direccion fisica
/// (cada `mmio_map` crea un mapeo nuevo que no se libera hasta salir).
struct ClientBuffers {
    entries: [(u64, u64); 8], // (phys, len)
    count: usize,
}

impl ClientBuffers {
    fn map(&mut self, dma: Handle) -> Option<(u64, u64)> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        if let Some(e) = self.entries[..self.count].iter().find(|e| e.0 == phys) {
            return Some(*e);
        }
        sys::mmio_map(dma).ok()?;
        if self.count < self.entries.len() {
            self.entries[self.count] = (phys, len);
            self.count += 1;
        }
        Some((phys, len))
    }
}

/// Bucle de servicio: una peticion por mensaje, respuesta por el endpoint
/// transferido en el mensaje.
fn serve(dev: &mut Device) -> ! {
    let capacity = dev.capacity();
    let mut buffers = ClientBuffers {
        entries: [(0, 0); 8],
        count: 0,
    };
    let mut msg = [0u8; 32];
    let mut handles = [Handle(0); 2];
    say(b"sirviendo peticiones");
    loop {
        let Ok((n, nh)) = sys::ipc_receive(SERVICE, &mut msg, &mut handles, TIMEOUT_INFINITE)
        else {
            continue;
        };
        if nh == 0 {
            continue; // sin endpoint de respuesta no hay a quien contestar
        }
        let reply_to = handles[0];
        let reply = match Request::decode(&msg[..n]) {
            None => Reply {
                status: Status::Invalid,
                value: 0,
            },
            Some(Request { op: Op::Info, .. }) => Reply {
                status: Status::Ok,
                value: capacity,
            },
            Some(Request { op, count, lba }) => {
                let count = count.min(MAX_SECTORS) as u64;
                if nh < 2 {
                    Reply {
                        status: Status::BadHandles,
                        value: 0,
                    }
                } else if count == 0 || lba.checked_add(count).is_none_or(|end| end > capacity) {
                    Reply {
                        status: Status::Invalid,
                        value: 0,
                    }
                } else {
                    match buffers.map(handles[1]) {
                        Some((phys, len)) if len >= count * 512 => {
                            let bytes = (count * 512) as u32;
                            let r = match op {
                                Op::Read => {
                                    dev.request(blk::RequestHeader::read(lba), phys, bytes, true)
                                }
                                Op::Write => {
                                    dev.request(blk::RequestHeader::write(lba), phys, bytes, false)
                                }
                                Op::Info => unreachable!(),
                            };
                            match r {
                                Ok(()) => Reply {
                                    status: Status::Ok,
                                    value: count,
                                },
                                Err(_) => {
                                    if dev.common.r8(common::DEVICE_STATUS) & status::NEEDS_RESET
                                        != 0
                                    {
                                        say(b"el dispositivo pide reset");
                                    }
                                    Reply {
                                        status: Status::Io,
                                        value: 0,
                                    }
                                }
                            }
                        }
                        _ => Reply {
                            status: Status::BadHandles,
                            value: 0,
                        },
                    }
                }
            }
        };
        if nh >= 2 {
            let _ = sys::handle_close(handles[1]);
        }
        let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

fn main(mask: u64) -> u64 {
    let mut dev = match setup(mask) {
        Ok(d) => d,
        Err(step) => {
            say(b"fallo en la inicializacion");
            return step;
        }
    };
    let mut buf = [0u8; 20];
    say(b"dispositivo listo");
    sys::print(b"virtio-blk: capacidad ");
    sys::print(user_testprogs::fmt_u64(dev.capacity(), &mut buf));
    sys::print(b" sectores\n");
    report_partitions(&mut dev);
    serve(&mut dev)
}

user_testprogs::entry!(main);
