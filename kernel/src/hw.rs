//! Hardware objects for user-space drivers (Phase 16): interrupt lines,
//! MMIO regions and DMA buffers. A driver only gets what its capability
//! grants; the kernel lends it nothing else (section 27).

use crate::arch::x86_64::{apic, ioapic};
use crate::memory::pmm;
use crate::object::{KernelObject, ObjectKind, ObjectRef};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use alloc::collections::BTreeMap;
use alloc::sync::Weak;
use aricode_pmm::{Frame, FRAME_SIZE};
use core::sync::atomic::{AtomicU32, Ordering};
use log::warn;

/// Vectors reserved for devices.
pub const DEVICE_VECTOR_FIRST: u8 = 0x30;
pub const DEVICE_VECTOR_LAST: u8 = 0x3F;

/// Source of a line: an I/O APIC GSI, an MSI-X entry (table mapped by the
/// kernel, entry index) or an MSI capability (the function's configuration
/// space in the ECAM and the capability offset; ADR 0022).
pub enum IrqSource {
    Gsi { gsi: u32, level: bool },
    Msix { table_virt: u64, entry: u32 },
    Msi { cfg_virt: u64, cap: u8 },
}

/// An interrupt line routed to a vector. When the last handle is dropped it
/// is masked and the vector is freed.
pub struct IrqLine {
    pub source: IrqSource,
    pub vector: u8,
    pending: AtomicU32,
    waiters: WaitQueue,
}

impl IrqLine {
    /// Called from the handler: accumulates and wakes up. Level-triggered lines
    /// stay masked until the driver services them (`irq_wait`).
    pub fn fire(&self) {
        if let IrqSource::Gsi { gsi, level: true } = self.source {
            ioapic::set_masked(gsi, true);
        }
        self.pending.fetch_add(1, Ordering::Release);
        self.waiters.wake_all();
    }

    /// GSI if it is an I/O APIC line (for `object_id`).
    pub fn gsi(&self) -> u32 {
        match self.source {
            IrqSource::Gsi { gsi, .. } => gsi,
            IrqSource::Msix { entry, .. } => entry,
            IrqSource::Msi { .. } => 0,
        }
    }

    /// Waits until there are accumulated interrupts and returns them.
    pub fn wait(&self, deadline: Option<u64>) -> Option<u32> {
        let timed_out = self
            .waiters
            .wait_until_deadline(|| self.pending.load(Ordering::Acquire) > 0, deadline);
        if timed_out {
            return None;
        }
        let n = self.pending.swap(0, Ordering::AcqRel);
        if let IrqSource::Gsi { gsi, level: true } = self.source {
            ioapic::set_masked(gsi, false);
        }
        Some(n)
    }
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        match self.source {
            IrqSource::Gsi { gsi, .. } => ioapic::set_masked(gsi, true),
            IrqSource::Msix { table_virt, entry } => {
                msix_write(table_virt, entry, 0, 0, MSIX_MASKED)
            }
            IrqSource::Msi { cfg_virt, cap } => msi_enable(cfg_virt, cap, false),
        }
        VECTORS.lock().remove(&self.vector);
    }
}

const MSIX_MASKED: u32 = 1;

/// Writes an MSI-X table entry (16 bytes: addr lo, addr hi, data, control).
fn msix_write(table_virt: u64, entry: u32, addr: u64, data: u32, control: u32) {
    let base = (table_virt + 16 * entry as u64) as *mut u32;
    // SAFETY: the table was mapped uncached in `map_uc` and `entry` is a valid
    // entry of the claimed device.
    unsafe {
        base.add(3).write_volatile(MSIX_MASKED); // mask while programming
        base.write_volatile(addr as u32);
        base.add(1).write_volatile((addr >> 32) as u32);
        base.add(2).write_volatile(data);
        base.add(3).write_volatile(control);
    }
}

/// Uncached kernel MMIO windows (MSI-X tables and the like), allocated
/// incrementally and never freed: used for as long as the device lives.
static UC_NEXT: IrqLock<u64> = IrqLock::new(0xFFFF_D200_0000_0000);

pub(crate) fn map_uc(phys: u64, len: u64) -> Result<u64, &'static str> {
    use crate::memory::vmm;
    use aricode_x86_paging::{
        CACHE_DISABLE, GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE, WRITE_THROUGH,
    };
    let offset = phys & (PAGE_SIZE - 1);
    let start = phys - offset;
    let span = (offset + len).next_multiple_of(PAGE_SIZE);
    let mut next = UC_NEXT.lock();
    let virt = *next;
    vmm::with(|m| {
        m.map_range_4k(
            virt,
            start,
            span,
            WRITABLE | NO_EXECUTE | GLOBAL | CACHE_DISABLE | WRITE_THROUGH,
        )
    })
    .map_err(|_| "hw: map uncached window")?;
    *next += span;
    Ok(virt + offset)
}

/// MSI/MSI-X message address targeting the boot LAPIC (no redirection,
/// no logical mode).
fn message_address() -> u64 {
    0xFEE0_0000 | ((apic::bsp_id() as u64) << 12)
}

fn allocate_vector(vectors: &BTreeMap<u8, Weak<KernelObject>>) -> Result<u8, &'static str> {
    (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST)
        .find(|v| !vectors.contains_key(v))
        .ok_or("hw: no free vectors")
}

fn new_line(
    vectors: &mut BTreeMap<u8, Weak<KernelObject>>,
    source: IrqSource,
    vector: u8,
) -> ObjectRef {
    let obj = KernelObject::new(ObjectKind::Irq(IrqLine {
        source,
        vector,
        pending: AtomicU32::new(0),
        waiters: WaitQueue::new(),
    }));
    vectors.insert(vector, alloc::sync::Arc::downgrade(&obj));
    obj
}

/// Creates the IRQ object for an MSI-X entry: allocates a vector and
/// programs the entry towards the boot LAPIC.
pub fn register_msix(table_phys: u64, entry: u32) -> Result<ObjectRef, &'static str> {
    let table_virt = map_uc(table_phys, 16 * (entry as u64 + 1))?;
    let mut vectors = VECTORS.lock();
    let vector = allocate_vector(&vectors)?;
    msix_write(table_virt, entry, message_address(), vector as u32, 0);
    Ok(new_line(
        &mut vectors,
        IrqSource::Msix { table_virt, entry },
        vector,
    ))
}

/// Creates the IRQ object for an MSI capability (single message): allocates
/// a vector, programs address and data towards the boot LAPIC and enables
/// MSI. For functions without MSI-X (2012 Intel controllers, for example).
pub fn register_msi(cfg_virt: u64, cap: u8) -> Result<ObjectRef, &'static str> {
    let mut vectors = VECTORS.lock();
    let vector = allocate_vector(&vectors)?;
    msi_program(cfg_virt, cap, message_address(), vector as u16);
    Ok(new_line(
        &mut vectors,
        IrqSource::Msi { cfg_virt, cap },
        vector,
    ))
}

/// MSI capability (PCI 3.0, 6.8.1): control at +2 (bit 0 enable, bits 6:4
/// enabled messages, bit 7 64-bit addresses, bit 8 per-vector masking),
/// address at +4 (and the high part at +8 with 64 bits), data at +8 or +0xC
/// and mask at +0xC or +0x10.
const MSI_ENABLE: u16 = 0x0001;
const MSI_MULTIPLE_ENABLE: u16 = 0x0070;
const MSI_64BIT: u16 = 0x0080;
const MSI_MASKABLE: u16 = 0x0100;

fn msi_program(cfg_virt: u64, cap: u8, addr: u64, data: u16) {
    let base = cfg_virt + cap as u64;
    // SAFETY: the ECAM is mapped uncached by `pci::init` and `cap` is the MSI
    // capability of a claimed function; aligned accesses.
    unsafe {
        let ctrl = ((base + 2) as *const u16).read_volatile();
        // Disabled and with a single message while programming.
        ((base + 2) as *mut u16).write_volatile(ctrl & !(MSI_ENABLE | MSI_MULTIPLE_ENABLE));
        ((base + 4) as *mut u32).write_volatile(addr as u32);
        let data_off = if ctrl & MSI_64BIT != 0 {
            ((base + 8) as *mut u32).write_volatile((addr >> 32) as u32);
            0xC
        } else {
            8
        };
        ((base + data_off) as *mut u16).write_volatile(data);
        if ctrl & MSI_MASKABLE != 0 {
            ((base + data_off + 4) as *mut u32).write_volatile(0);
        }
    }
    msi_enable(cfg_virt, cap, true);
}

fn msi_enable(cfg_virt: u64, cap: u8, on: bool) {
    let ctrl_ptr = (cfg_virt + cap as u64 + 2) as *mut u16;
    // SAFETY: same as `msi_program`.
    unsafe {
        let ctrl = ctrl_ptr.read_volatile() & !MSI_MULTIPLE_ENABLE;
        ctrl_ptr.write_volatile(if on {
            ctrl | MSI_ENABLE
        } else {
            ctrl & !MSI_ENABLE
        });
    }
}

/// MMIO region of a device (physical, uncached) or a DMA buffer (contiguous
/// RAM, cached).
pub struct Region {
    pub phys: u64,
    pub len: u64,
    pub cacheable: bool,
    /// PCI function it belongs to (`bus << 8 | dev << 3 | fn`): mapping it
    /// assigns the device to the process's IOMMU domain (Phase 33).
    pub source_id: Option<u16>,
}

/// DMA buffer: contiguous frames that go back to the PMM when the object is dropped.
pub struct DmaBuffer {
    pub region: Region,
    frames: usize,
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        // Before returning the frames, no device must be able to keep
        // writing to them.
        crate::iommu::unmap_everywhere(self.region.phys, self.region.len);
        pmm::with(|p| p.free_contiguous(Frame::from_addr(self.region.phys), self.frames))
            .expect("DMA frames");
    }
}

static VECTORS: IrqLock<BTreeMap<u8, Weak<KernelObject>>> = IrqLock::new(BTreeMap::new());

/// Creates the IRQ object for a GSI: allocates a vector, routes and registers it.
pub fn register_irq(gsi: u32, level: bool, active_low: bool) -> Result<ObjectRef, &'static str> {
    let mut vectors = VECTORS.lock();
    let vector = (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST)
        .find(|v| !vectors.contains_key(v))
        .ok_or("hw: no free vectors")?;
    ioapic::route(gsi, vector, apic::bsp_id(), level, active_low)?;
    let obj = KernelObject::new(ObjectKind::Irq(IrqLine {
        source: IrqSource::Gsi { gsi, level },
        vector,
        pending: AtomicU32::new(0),
        waiters: WaitQueue::new(),
    }));
    vectors.insert(vector, alloc::sync::Arc::downgrade(&obj));
    Ok(obj)
}

/// Entry from the APIC for device vectors.
pub fn handle_device_irq(vector: u8) {
    let line = VECTORS.lock().get(&vector).and_then(Weak::upgrade);
    match line.as_ref().map(|o| &o.kind) {
        Some(ObjectKind::Irq(l)) => l.fire(),
        _ => warn!("irq: vector {vector:#x} has no registered line"),
    }
}

/// Allocates a physically contiguous DMA buffer.
pub fn dma_alloc(len: u64) -> Result<ObjectRef, &'static str> {
    if len == 0 || len > aricode_abi::DMA_MAX_LEN {
        return Err("hw: invalid DMA size");
    }
    let frames = len.div_ceil(FRAME_SIZE) as usize;
    let first =
        pmm::with(|p| p.allocate_contiguous(frames, 1)).ok_or("hw: no contiguous memory")?;
    // Zero it: a driver must not see another driver's data.
    // SAFETY: freshly allocated frames, reachable through the physical map.
    unsafe {
        core::ptr::write_bytes(
            crate::memory::phys::phys_to_virt(first.addr()) as *mut u8,
            0,
            frames * FRAME_SIZE as usize,
        )
    };
    Ok(KernelObject::new(ObjectKind::Dma(DmaBuffer {
        region: Region {
            phys: first.addr(),
            len: frames as u64 * FRAME_SIZE,
            cacheable: true,
            source_id: None,
        },
        frames,
    })))
}

/// USB test bench (`test=usb_hid`, Phase 25): the ring 3 xHCI driver
/// receives the controller (class 0c.03) and a service endpoint;
/// `input_client` reads over IPC what the host injects through QMP into the
/// USB keyboard and mouse.
pub fn usb_hid_selftest() {
    use crate::user::{process, programs};
    use alloc::vec;
    use aricode_handles::Rights;

    let index = crate::pci::find_class(0x0C, 0x03).expect("USB controller on the PCI bus");
    let claimed = crate::pci::claim(index).expect("claim the xHCI");
    let service = KernelObject::new(ObjectKind::Endpoint(crate::ipc::Endpoint::new()));
    let mut extra = vec![(
        service.clone(),
        Rights::READ.union(Rights::WRITE).union(Rights::TRANSFER),
    )];
    let mut mask = 0u64;
    for (i, slot) in claimed.handles.into_iter().enumerate() {
        if let Some(h) = slot {
            match i {
                2 => mask |= 1 << 15,
                3..=8 => mask |= 1 << (i - 3),
                _ => {}
            }
            extra.push(h);
        }
    }
    extra.push((KernelObject::new(ObjectKind::System), Rights::READ));
    let driver = process::spawn_with_handles_and_arg(
        "usb_input",
        programs::by_name("usb_input").expect("usb_input in the initrd"),
        extra,
        mask,
        0,
    )
    .expect("usb_input");
    let client = process::spawn_with_handles_and_arg(
        "input_client",
        programs::by_name("input_client").expect("input_client"),
        vec![(service, Rights::WRITE)],
        1,
        0,
    )
    .expect("input_client");
    let status = process::wait(client);
    assert_eq!(
        status,
        process::ExitStatus::Exited(0),
        "input_client: {status:?}"
    );
    let _ = driver;
    log::info!("usb hid selftest ok (xHCI and boot HID in ring 3, events over IPC)");
}

/// Test bench (`test=drivers`): PIT IRQ0, DMA and the framebuffer as MMIO,
/// all serviced by user drivers.
pub fn selftest(info: &boot_protocol::BootInfo) {
    use crate::arch::x86_64::pit;
    use crate::user::{process, programs};
    use alloc::vec;
    use aricode_handles::Rights;

    // 1. A user driver services IRQ0 (PIT at 100 Hz) and allocates DMA.
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(0));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    let irq = register_irq(gsi, level, low).expect("irq0");
    let hardware = KernelObject::new(ObjectKind::Hardware);
    pit::start_periodic(100);
    let pid = process::spawn_with_handles(
        "pit_driver",
        programs::by_name("pit_driver").expect("pit_driver"),
        vec![
            (irq.clone(), Rights::READ.union(Rights::WAIT)),
            (hardware, Rights::MANAGE),
        ],
    )
    .expect("pit_driver");
    let status = process::wait(pid);
    // Silence the source before dropping the line: first mask the GSI,
    // then stop the PIT and leave a tick so any interrupt already in
    // flight arrives while the line is still registered.
    ioapic::set_masked(gsi, true);
    pit::stop();
    crate::task::scheduler::sleep_ms(crate::time::MS_PER_TICK * 2);
    let fired = match &irq.kind {
        ObjectKind::Irq(l) => l.pending.load(Ordering::Relaxed),
        _ => 0,
    };
    drop(irq);
    assert_eq!(
        status,
        process::ExitStatus::Exited(0),
        "pit_driver: {status:?}"
    );
    assert!(
        VECTORS.lock().is_empty(),
        "the vector must be freed when the line is dropped"
    );

    // 2. A user driver paints the framebuffer through an MMIO capability.
    let fb = info.framebuffer;
    if fb.address != 0 {
        let region = KernelObject::new(ObjectKind::Mmio(Region {
            phys: fb.address,
            len: fb.size,
            cacheable: false,
            source_id: None,
        }));
        let pid = process::spawn_with_handles_and_arg(
            "fb_driver",
            programs::by_name("fb_driver").expect("fb_driver"),
            vec![(region, Rights::READ.union(Rights::WRITE))],
            fb.stride as u64,
            0,
        )
        .expect("fb_driver");
        let status = process::wait(pid);
        assert_eq!(
            status,
            process::ExitStatus::Exited(0),
            "fb_driver: {status:?}"
        );
        // SAFETY: framebuffer reachable through the physical map.
        let pixel = unsafe {
            (crate::memory::phys::phys_to_virt(fb.address) as *const u32).read_volatile()
        };
        assert_eq!(
            pixel, 0x00FF_8800,
            "the driver should have painted pixel (0,0)"
        );
    }
    log::info!(
        "drivers selftest ok (IRQ0 serviced in ring 3, {fired} pending at close, DMA allocated and mapped, framebuffer painted from ring 3)"
    );
}
