//! Driver virtio-blk (Fase 18) en ring 3. Recibe por `pci_claim`, a través de
//! `init`, el espacio de configuración, la capacidad de hardware, la IRQ
//! MSI-X y los BARs. Inicializa el dispositivo por la interfaz moderna,
//! crea la cola 0 en memoria DMA y verifica lectura y escritura del disco.
//!
//! En esta fase se ejecuta como autoprueba; en la Fase 19 pasa a servir
//! peticiones por IPC.
#![no_std]
#![no_main]

use aricode_virtio::{
    blk, cfg_type, common, find_structures, status, QueueLayout, Virtqueue, FEATURE_VERSION_1,
};
use user_testprogs::sys::{self, Error, Handle};

const CFG: Handle = Handle(0x0001_0004);
const HW: Handle = Handle(0x0001_0005);
const IRQ: Handle = Handle(0x0001_0006);
const FIRST_BAR_HANDLE: u32 = 0x0001_0007;

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
        unsafe { ((self.0 + off) as *const u16).read_volatile() }
    }
    fn r32(&self, off: u64) -> u32 {
        unsafe { ((self.0 + off) as *const u32).read_volatile() }
    }
    fn r64(&self, off: u64) -> u64 {
        self.r32(off) as u64 | (self.r32(off + 4) as u64) << 32
    }
    fn w8(&self, off: u64, v: u8) {
        unsafe { ((self.0 + off) as *mut u8).write_volatile(v) }
    }
    fn w16(&self, off: u64, v: u16) {
        unsafe { ((self.0 + off) as *mut u16).write_volatile(v) }
    }
    fn w32(&self, off: u64, v: u32) {
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
    /// Buffer DMA para cabecera (16), datos (512) y estado (1).
    req_virt: u64,
    req_phys: u64,
}

fn setup(mask: u64) -> Result<Device, u64> {
    // 1. Estructuras virtio a partir del espacio de configuración.
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
    // Todas en el mismo BAR en QEMU; se mapea una vez por BAR distinto.
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
    let offered = f_lo | f_hi;
    if offered & FEATURE_VERSION_1 == 0 {
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

    /// Una peticion sincrona de un sector: cabecera, datos y estado en el
    /// buffer DMA; espera la interrupcion MSI-X y comprueba el estado.
    fn request(
        &mut self,
        header: blk::RequestHeader,
        data_write_by_device: bool,
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
                (self.req_phys + 16, 512, data_write_by_device),
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

    fn data(&self) -> &[u8] {
        // SAFETY: 512 bytes de datos dentro del buffer DMA.
        unsafe { core::slice::from_raw_parts((self.req_virt + 16) as *const u8, 512) }
    }

    fn data_mut(&mut self) -> &mut [u8] {
        // SAFETY: idem, en exclusiva mientras no hay peticion en curso.
        unsafe { core::slice::from_raw_parts_mut((self.req_virt + 16) as *mut u8, 512) }
    }
}

fn selftest(dev: &mut Device) -> Result<(), u64> {
    // El disco de pruebas tiene en cada sector "ARIDISK\0" + numero de sector.
    for sector in [0u64, 1, 7] {
        dev.request(blk::RequestHeader::read(sector), true)?;
        let d = dev.data();
        if &d[..8] != b"ARIDISK\0" || d[8..16] != sector.to_le_bytes() {
            return Err(40);
        }
    }
    // Escribir un patron en el sector 2 y releerlo.
    {
        let d = dev.data_mut();
        for (i, b) in d.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
    }
    dev.request(blk::RequestHeader::write(2), false)?;
    dev.data_mut().fill(0);
    dev.request(blk::RequestHeader::read(2), true)?;
    if dev
        .data()
        .iter()
        .enumerate()
        .any(|(i, b)| *b != (i as u8).wrapping_mul(7).wrapping_add(3))
    {
        return Err(41);
    }
    if dev.common.r8(common::DEVICE_STATUS) & status::NEEDS_RESET != 0 {
        return Err(42);
    }
    Ok(())
}

fn main(mask: u64) -> u64 {
    let mut dev = match setup(mask) {
        Ok(d) => d,
        Err(step) => {
            say(b"fallo en la inicializacion");
            return step;
        }
    };
    let cap = dev.capacity();
    let mut buf = [0u8; 20];
    say(b"dispositivo listo");
    sys::print(b"virtio-blk: capacidad ");
    sys::print(user_testprogs::fmt_u64(cap, &mut buf));
    sys::print(b" sectores\n");
    match selftest(&mut dev) {
        Ok(()) => {
            say(b"lectura y escritura ok");
            0
        }
        Err(step) => {
            say(b"fallo en la autoprueba");
            step
        }
    }
}

user_testprogs::entry!(main);
