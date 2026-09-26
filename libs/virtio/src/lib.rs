//! Virtio 1.x (sección 40). Todo lo que se puede probar en host: el layout de
//! una virtqueue split sobre un buffer DMA, el descubrimiento de las
//! estructuras virtio-pci modernas en el espacio de configuración y las
//! peticiones de virtio-blk. Los accesos MMIO reales los hace el driver.

#![no_std]

use core::sync::atomic::{fence, Ordering};

/// Estados del dispositivo (registro `device_status`).
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// Bit de característica obligatorio en virtio 1.x (palabra alta, bit 0).
pub const FEATURE_VERSION_1: u64 = 1 << 32;

/// Desplazamientos de la estructura `virtio_pci_common_cfg`.
pub mod common {
    pub const DEVICE_FEATURE_SELECT: u64 = 0x00;
    pub const DEVICE_FEATURE: u64 = 0x04;
    pub const DRIVER_FEATURE_SELECT: u64 = 0x08;
    pub const DRIVER_FEATURE: u64 = 0x0C;
    pub const MSIX_CONFIG: u64 = 0x10;
    pub const NUM_QUEUES: u64 = 0x12;
    pub const DEVICE_STATUS: u64 = 0x14;
    pub const CONFIG_GENERATION: u64 = 0x15;
    pub const QUEUE_SELECT: u64 = 0x16;
    pub const QUEUE_SIZE: u64 = 0x18;
    pub const QUEUE_MSIX_VECTOR: u64 = 0x1A;
    pub const QUEUE_ENABLE: u64 = 0x1C;
    pub const QUEUE_NOTIFY_OFF: u64 = 0x1E;
    pub const QUEUE_DESC: u64 = 0x20;
    pub const QUEUE_DRIVER: u64 = 0x28;
    pub const QUEUE_DEVICE: u64 = 0x30;
}

/// Tipos de estructura anunciados por las capabilities vendor de virtio-pci.
pub mod cfg_type {
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
}

/// Una estructura virtio localizada en un BAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciStructure {
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    /// Solo para `NOTIFY`.
    pub notify_off_multiplier: u32,
}

/// Localiza las estructuras virtio en un espacio de configuración PCI.
pub fn find_structures(cfg: &[u8]) -> [Option<PciStructure>; 5] {
    let mut out = [None; 5];
    for cap in aricode_pci::capabilities(cfg) {
        if cap.id != aricode_pci::CAP_VENDOR {
            continue;
        }
        let o = cap.offset as usize;
        if o + 16 > cfg.len() {
            continue;
        }
        let cfg_type = cfg[o + 3];
        if !(1..=4).contains(&cfg_type) {
            continue;
        }
        let u32_at = |p: usize| u32::from_le_bytes(cfg[p..p + 4].try_into().unwrap());
        let notify_off_multiplier = if cfg_type == cfg_type::NOTIFY && o + 20 <= cfg.len() {
            u32_at(o + 16)
        } else {
            0
        };
        let s = PciStructure {
            cfg_type,
            bar: cfg[o + 4],
            offset: u32_at(o + 8),
            length: u32_at(o + 12),
            notify_off_multiplier,
        };
        // La primera capability de cada tipo es la que manda.
        if out[cfg_type as usize].is_none() {
            out[cfg_type as usize] = Some(s);
        }
    }
    out
}

/// Descriptor de una virtqueue split (16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// Layout de una virtqueue split de `size` entradas en memoria contigua:
/// tabla de descriptores, anillo del driver (avail) y anillo del dispositivo
/// (used), cada uno alineado como exige la especificación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLayout {
    pub size: u16,
    pub desc_offset: usize,
    pub avail_offset: usize,
    pub used_offset: usize,
    pub total_len: usize,
}

impl QueueLayout {
    pub fn new(size: u16) -> Self {
        let n = size as usize;
        let desc_offset = 0;
        let avail_offset = 16 * n; // alineado a 2 (ya lo está)
        let avail_len = 6 + 2 * n;
        let used_offset = (avail_offset + avail_len).next_multiple_of(4096); // alineado a 4 (usamos página)
        let used_len = 6 + 8 * n;
        let total_len = (used_offset + used_len).next_multiple_of(4096);
        Self {
            size,
            desc_offset,
            avail_offset,
            used_offset,
            total_len,
        }
    }
}

/// Una virtqueue split que vive en un buffer DMA mapeado en `virt` con
/// dirección física `phys`. Un solo productor (el driver).
pub struct Virtqueue {
    layout: QueueLayout,
    virt: *mut u8,
    phys: u64,
    free_head: u16,
    num_free: u16,
    avail_idx: u16,
    last_used: u16,
}

impl Virtqueue {
    /// # Safety
    ///
    /// `virt` debe apuntar a `layout.total_len` bytes escribibles, alineados a
    /// página, que reflejan la física `phys`, exclusivos de esta cola.
    pub unsafe fn new(layout: QueueLayout, virt: *mut u8, phys: u64) -> Self {
        core::ptr::write_bytes(virt, 0, layout.total_len);
        let mut q = Self {
            layout,
            virt,
            phys,
            free_head: 0,
            num_free: layout.size,
            avail_idx: 0,
            last_used: 0,
        };
        for i in 0..layout.size {
            q.desc_mut(i).next = i.wrapping_add(1);
        }
        q
    }

    pub fn size(&self) -> u16 {
        self.layout.size
    }
    pub fn desc_phys(&self) -> u64 {
        self.phys + self.layout.desc_offset as u64
    }
    pub fn avail_phys(&self) -> u64 {
        self.phys + self.layout.avail_offset as u64
    }
    pub fn used_phys(&self) -> u64 {
        self.phys + self.layout.used_offset as u64
    }

    fn desc_mut(&mut self, i: u16) -> &mut Descriptor {
        // SAFETY: dentro de la tabla de descriptores, por contrato de `new`.
        unsafe {
            &mut *(self.virt.add(self.layout.desc_offset + 16 * i as usize) as *mut Descriptor)
        }
    }
    fn desc(&self, i: u16) -> Descriptor {
        // SAFETY: ídem.
        unsafe {
            (self.virt.add(self.layout.desc_offset + 16 * i as usize) as *const Descriptor)
                .read_volatile()
        }
    }
    fn write_u16(&mut self, off: usize, v: u16) {
        // SAFETY: dentro del buffer de la cola.
        unsafe { (self.virt.add(off) as *mut u16).write_volatile(v) }
    }
    fn read_u16(&self, off: usize) -> u16 {
        // SAFETY: ídem.
        unsafe { (self.virt.add(off) as *const u16).read_volatile() }
    }
    fn read_u32(&self, off: usize) -> u32 {
        // SAFETY: ídem.
        unsafe { (self.virt.add(off) as *const u32).read_volatile() }
    }

    /// Encadena buffers `(phys, len, escribible por el dispositivo)` y los
    /// publica en el anillo avail. Devuelve el descriptor cabeza, o `None` si
    /// no hay sitio.
    pub fn add(&mut self, buffers: &[(u64, u32, bool)]) -> Option<u16> {
        if buffers.is_empty() || (buffers.len() as u16) > self.num_free {
            return None;
        }
        let head = self.free_head;
        let mut i = head;
        for (n, &(addr, len, write)) in buffers.iter().enumerate() {
            let next_free = self.desc(i).next;
            let last = n + 1 == buffers.len();
            let d = self.desc_mut(i);
            d.addr = addr;
            d.len = len;
            d.flags = if write { DESC_WRITE } else { 0 } | if last { 0 } else { DESC_NEXT };
            if !last {
                d.next = next_free;
            }
            i = next_free;
        }
        self.free_head = i;
        self.num_free -= buffers.len() as u16;
        let slot = self.avail_idx % self.layout.size;
        self.write_u16(self.layout.avail_offset + 4 + 2 * slot as usize, head);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.write_u16(self.layout.avail_offset + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        Some(head)
    }

    /// Siguiente elemento consumido por el dispositivo: (cabeza, bytes escritos).
    /// Devuelve los descriptores de la cadena a la lista libre.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        let used_idx = self.read_u16(self.layout.used_offset + 2);
        if used_idx == self.last_used {
            return None;
        }
        let slot = self.last_used % self.layout.size;
        let base = self.layout.used_offset + 4 + 8 * slot as usize;
        let head = self.read_u32(base) as u16;
        let len = self.read_u32(base + 4);
        self.last_used = self.last_used.wrapping_add(1);
        // Devolver la cadena a la lista libre.
        let mut i = head;
        let mut count = 1;
        loop {
            let d = self.desc(i);
            if d.flags & DESC_NEXT == 0 {
                break;
            }
            i = d.next;
            count += 1;
        }
        self.desc_mut(i).next = self.free_head;
        self.free_head = head;
        self.num_free += count;
        Some((head, len))
    }

    pub fn num_free(&self) -> u16 {
        self.num_free
    }
}

/// Cabecera de petición virtio-blk (16 bytes) seguida de datos y un byte de estado.
pub mod blk {
    pub const T_IN: u32 = 0;
    pub const T_OUT: u32 = 1;
    pub const T_FLUSH: u32 = 4;
    pub const STATUS_OK: u8 = 0;
    pub const STATUS_IOERR: u8 = 1;
    pub const STATUS_UNSUPP: u8 = 2;
    pub const SECTOR_SIZE: u64 = 512;
    /// Desplazamiento de `capacity` (u64, sectores) en la configuración del dispositivo.
    pub const CFG_CAPACITY: u64 = 0;

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct RequestHeader {
        pub kind: u32,
        pub reserved: u32,
        pub sector: u64,
    }

    impl RequestHeader {
        pub const fn read(sector: u64) -> Self {
            Self {
                kind: T_IN,
                reserved: 0,
                sector,
            }
        }
        pub const fn write(sector: u64) -> Self {
            Self {
                kind: T_OUT,
                reserved: 0,
                sector,
            }
        }
        pub fn to_bytes(self) -> [u8; 16] {
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&self.kind.to_le_bytes());
            b[8..16].copy_from_slice(&self.sector.to_le_bytes());
            b
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::vec;

    struct Buf {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Buf {
        fn new(len: usize) -> Self {
            let layout = Layout::from_size_align(len, 4096).unwrap();
            Self {
                ptr: unsafe { alloc_zeroed(layout) },
                layout,
            }
        }
    }
    impl Drop for Buf {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    #[test]
    fn layout_alignment() {
        let l = QueueLayout::new(8);
        assert_eq!(l.avail_offset, 128);
        assert_eq!(l.used_offset, 4096);
        assert_eq!(l.total_len, 8192);
        let l = QueueLayout::new(256);
        assert_eq!(l.avail_offset, 4096);
        assert_eq!(l.used_offset, 8192);
        assert!(l.total_len % 4096 == 0);
    }

    #[test]
    fn add_chain_and_pop_used_recycles_descriptors() {
        let l = QueueLayout::new(4);
        let b = Buf::new(l.total_len);
        let mut q = unsafe { Virtqueue::new(l, b.ptr, 0x10_0000) };
        assert_eq!(q.desc_phys(), 0x10_0000);
        assert_eq!(q.avail_phys(), 0x10_0000 + 64);
        let head = q
            .add(&[(0x1000, 16, false), (0x2000, 512, true), (0x3000, 1, true)])
            .unwrap();
        assert_eq!(head, 0);
        assert_eq!(q.num_free(), 1);
        // Descriptores encadenados con flags correctos.
        let d0 = q.desc(0);
        assert_eq!(
            (d0.addr, d0.len, d0.flags, d0.next),
            (0x1000, 16, DESC_NEXT, 1)
        );
        let d1 = q.desc(1);
        assert_eq!((d1.flags, d1.next), (DESC_NEXT | DESC_WRITE, 2));
        let d2 = q.desc(2);
        assert_eq!(d2.flags, DESC_WRITE);
        // avail: idx 1, ring[0] = 0
        assert_eq!(q.read_u16(l.avail_offset + 2), 1);
        assert_eq!(q.read_u16(l.avail_offset + 4), 0);
        // Sin sitio para una cadena de 2.
        assert!(q.add(&[(0, 1, false), (0, 1, false)]).is_none());
        assert!(q.pop_used().is_none());
        // El "dispositivo" consume: used.ring[0] = {id 0, len 513}, used.idx = 1
        unsafe {
            (b.ptr.add(l.used_offset + 4) as *mut u32).write(0);
            (b.ptr.add(l.used_offset + 8) as *mut u32).write(513);
            (b.ptr.add(l.used_offset + 2) as *mut u16).write(1);
        }
        assert_eq!(q.pop_used(), Some((0, 513)));
        assert_eq!(q.num_free(), 4, "la cadena vuelve a la lista libre");
        assert!(q.pop_used().is_none());
        // Se puede volver a usar la cola entera.
        assert!(q
            .add(&[(0, 1, false), (0, 1, false), (0, 1, false), (0, 1, false)])
            .is_some());
        assert_eq!(q.num_free(), 0);
    }

    #[test]
    fn finds_virtio_pci_structures() {
        let mut cfg = vec![0u8; 256];
        cfg[0..2].copy_from_slice(&0x1AF4u16.to_le_bytes());
        cfg[6] |= 0x10;
        cfg[0x34] = 0x40;
        // cap vendor: common en BAR4 offset 0 len 0x38
        cfg[0x40] = 0x09;
        cfg[0x41] = 0x50;
        cfg[0x42] = 16;
        cfg[0x43] = cfg_type::COMMON;
        cfg[0x44] = 4;
        cfg[0x4C..0x50].copy_from_slice(&0x38u32.to_le_bytes());
        // cap vendor: notify en BAR4 offset 0x3000, multiplier 4
        cfg[0x50] = 0x09;
        cfg[0x51] = 0x64;
        cfg[0x52] = 20;
        cfg[0x53] = cfg_type::NOTIFY;
        cfg[0x54] = 4;
        cfg[0x58..0x5C].copy_from_slice(&0x3000u32.to_le_bytes());
        cfg[0x5C..0x60].copy_from_slice(&0x1000u32.to_le_bytes());
        cfg[0x60..0x64].copy_from_slice(&4u32.to_le_bytes());
        // cap MSI-X (no vendor) al final
        cfg[0x64] = 0x11;
        cfg[0x65] = 0;
        let s = find_structures(&cfg);
        assert_eq!(
            s[cfg_type::COMMON as usize],
            Some(PciStructure {
                cfg_type: 1,
                bar: 4,
                offset: 0,
                length: 0x38,
                notify_off_multiplier: 0
            })
        );
        let n = s[cfg_type::NOTIFY as usize].unwrap();
        assert_eq!((n.bar, n.offset, n.notify_off_multiplier), (4, 0x3000, 4));
        assert!(s[cfg_type::ISR as usize].is_none());
    }

    #[test]
    fn blk_request_header_bytes() {
        let h = blk::RequestHeader::read(7).to_bytes();
        assert_eq!(&h[0..4], &[0, 0, 0, 0]);
        assert_eq!(&h[8..16], &7u64.to_le_bytes());
        assert_eq!(blk::RequestHeader::write(1).kind, blk::T_OUT);
    }
}
