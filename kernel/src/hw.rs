// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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
use log::{info, warn};

/// Vectors reserved for devices.
pub const DEVICE_VECTOR_FIRST: u8 = 0x30;
pub const DEVICE_VECTOR_LAST: u8 = 0x3F;

/// Source of a line: an I/O APIC GSI, an MSI-X entry (table mapped by the
/// kernel, entry index) or an MSI capability (the function's configuration
/// space in the ECAM and the capability offset; ADR 0022).
#[derive(Clone, Copy)]
pub enum IrqSource {
    Gsi { gsi: u32, level: bool },
    Msix { table_virt: u64, entry: u32 },
    Msi { cfg_virt: u64, cap: u8 },
}

impl IrqSource {
    /// The message the MSI-X entry or MSI capability holds now, as
    /// `(address, data, delivered)`: `delivered` is false while the entry
    /// is masked or MSI is disabled. `None` for an I/O APIC line.
    fn message(&self) -> Option<(u64, u32, bool)> {
        match *self {
            IrqSource::Gsi { .. } => None,
            IrqSource::Msix { table_virt, entry } => {
                let [lo, hi, data, control] = msix_read(table_virt, entry);
                Some((
                    (hi as u64) << 32 | lo as u64,
                    data,
                    control & MSIX_MASKED == 0,
                ))
            }
            IrqSource::Msi { cfg_virt, cap } => Some(msi_read(cfg_virt, cap)),
        }
    }
}

/// `true` if `source` delivers `vector`: it holds that vector towards the
/// boot LAPIC, as `register_msix` and `register_msi` program it, with the
/// MSI-X entry unmasked or MSI enabled (`test=pci_stale_line`).
pub fn delivered(source: &IrqSource, vector: u8) -> bool {
    let _programming = VECTORS.lock();
    source
        .message()
        .is_some_and(|(addr, data, on)| addr == message_address() && data == vector as u32 && on)
}

/// An interrupt line routed to a vector. When the last handle is dropped
/// the vector is freed and the source masked, unless the line's PCI
/// function was claimed again since (incident 0050).
pub struct IrqLine {
    pub source: IrqSource,
    pub vector: u8,
    /// For an MSI-X or MSI line: its PCI function (source id) and the
    /// number of the claim it was made for (`Vectors::claims`).
    claim: Option<(u16, u64)>,
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

    /// Interrupts have accumulated since they were last taken. The count
    /// is a latch: an interrupt that fires while nobody waits stays here
    /// until a wait takes it.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) > 0
    }

    /// The queue `fire` wakes (for a wait on several objects, ADR 0044).
    pub fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// Takes the accumulated interrupts and unmasks a level-triggered line,
    /// which `fire` left masked until the driver serviced it.
    fn take(&self) -> u32 {
        let n = self.pending.swap(0, Ordering::AcqRel);
        if let IrqSource::Gsi { gsi, level: true } = self.source {
            ioapic::set_masked(gsi, false);
        }
        n
    }

    /// Takes the accumulated interrupts without waiting; `None` if there
    /// are none (or another waiter took them first).
    pub fn take_pending(&self) -> Option<u32> {
        if !self.is_pending() {
            return None;
        }
        Some(self.take()).filter(|n| *n > 0)
    }

    /// Waits until there are accumulated interrupts and returns them.
    pub fn wait(&self, deadline: Option<u64>) -> Option<u32> {
        let timed_out = self
            .waiters
            .wait_until_deadline(|| self.is_pending(), deadline);
        if timed_out {
            return None;
        }
        Some(self.take())
    }
}

impl Drop for IrqLine {
    /// Masks the source, unless the line's function was claimed again
    /// since: a line is transferable, so it can outlive its claim (in
    /// another process, in a queued message), and the function's
    /// interrupts then belong to the newer claim, which masking would
    /// silence (incident 0050). Decided from the claim numbers alone,
    /// without reading the device: with `VECTORS` held and interrupts
    /// off, a read could stall on a function being reset or reach another
    /// device. The decision and the mask run under `VECTORS`, as every
    /// claim's programming does, so no entry is torn.
    fn drop(&mut self) {
        let superseded = {
            let mut vectors = VECTORS.lock();
            let superseded = self
                .claim
                .is_some_and(|(function, n)| vectors.claims.get(&function) != Some(&n));
            if !superseded {
                match self.source {
                    IrqSource::Gsi { gsi, .. } => ioapic::set_masked(gsi, true),
                    IrqSource::Msix { table_virt, entry } => {
                        msix_write(table_virt, entry, 0, 0, MSIX_MASKED)
                    }
                    IrqSource::Msi { cfg_virt, cap } => msi_enable(cfg_virt, cap, false),
                }
            }
            vectors.lines.remove(&self.vector);
            superseded
        };
        if superseded {
            info!(
                "irq: vector {:#x} released; its function was claimed again since, its interrupts left to the newer claim",
                self.vector
            );
        }
    }
}

const MSIX_MASKED: u32 = 1;

/// Reads an MSI-X table entry (addr lo, addr hi, data, control), for
/// `delivered` only.
fn msix_read(table_virt: u64, entry: u32) -> [u32; 4] {
    let base = (table_virt + MSIX_ENTRY_BYTES * entry as u64) as *const u32;
    // SAFETY: as in `msix_write`; the window stays mapped for the device's
    // life.
    core::array::from_fn(|i| unsafe { base.add(i).read_volatile() })
}

/// Writes an MSI-X table entry (16 bytes: addr lo, addr hi, data, control).
fn msix_write(table_virt: u64, entry: u32, addr: u64, data: u32, control: u32) {
    let base = (table_virt + MSIX_ENTRY_BYTES * entry as u64) as *mut u32;
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

/// Uncached kernel MMIO windows, allocated incrementally and never freed:
/// each one is mapped once for a device and kept for as long as the
/// device lives (a DMAR unit's registers, a PCI function's MSI-X table,
/// whose window the function's record keeps for every later claim;
/// incident 0047). A window is never handed to anything else, so no TLB
/// shootdown is ever needed for this range.
const UC_BASE: u64 = 0xFFFF_D200_0000_0000;
static UC_NEXT: IrqLock<u64> = IrqLock::new(UC_BASE);

/// Bytes of uncached windows mapped so far (`test=pci_reap`).
pub fn uc_window_bytes() -> u64 {
    *UC_NEXT.lock() - UC_BASE
}

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

/// The device vectors in use and, per PCI function, which claim's lines
/// own its MSI-X entry and MSI capability.
struct Vectors {
    lines: BTreeMap<u8, Weak<KernelObject>>,
    /// Per function (source id): a number that changes with each claim
    /// (`next_claim`). A line made for an earlier number no longer owns
    /// the function's interrupts and never touches them (incident 0050).
    claims: BTreeMap<u16, u64>,
}

impl Vectors {
    fn allocate(&self) -> Result<u8, &'static str> {
        (DEVICE_VECTOR_FIRST..=DEVICE_VECTOR_LAST)
            .find(|v| !self.lines.contains_key(v))
            .ok_or("hw: no free vectors")
    }

    /// A new claim of `function`: the lines of the earlier ones stop
    /// owning its interrupts.
    fn next_claim(&mut self, function: u16) -> u64 {
        let n = self.claims.entry(function).or_insert(0);
        *n += 1;
        *n
    }

    fn new_line(&mut self, source: IrqSource, vector: u8, claim: Option<(u16, u64)>) -> ObjectRef {
        let obj = KernelObject::new(ObjectKind::Irq(IrqLine {
            source,
            vector,
            claim,
            pending: AtomicU32::new(0),
            waiters: WaitQueue::new(),
        }));
        self.lines.insert(vector, alloc::sync::Arc::downgrade(&obj));
        obj
    }
}

/// Bytes of one MSI-X table entry (PCI 3.0, 6.8.2).
pub const MSIX_ENTRY_BYTES: u64 = 16;

/// Creates the IRQ object of a claim of `function` (its source id) for
/// entry 0 of its MSI-X table (the one a claim hands out) mapped at
/// `table_virt` by `map_uc`, which outlives the line: allocates a vector
/// and programs the entry towards the boot LAPIC. The function's earlier
/// lines stop owning its interrupts.
pub fn register_msix(table_virt: u64, function: u16) -> Result<ObjectRef, &'static str> {
    let entry = 0;
    let mut vectors = VECTORS.lock();
    let vector = vectors.allocate()?;
    msix_write(table_virt, entry, message_address(), vector as u32, 0);
    let claim = vectors.next_claim(function);
    Ok(vectors.new_line(
        IrqSource::Msix { table_virt, entry },
        vector,
        Some((function, claim)),
    ))
}

/// Creates the IRQ object of a claim of `function` for its MSI capability
/// (single message): allocates a vector, programs address and data towards
/// the boot LAPIC and enables MSI. For functions without MSI-X (2012 Intel
/// controllers, for example). The function's earlier lines stop owning
/// its interrupts.
pub fn register_msi(cfg_virt: u64, cap: u8, function: u16) -> Result<ObjectRef, &'static str> {
    let mut vectors = VECTORS.lock();
    let vector = vectors.allocate()?;
    msi_program(cfg_virt, cap, message_address(), vector as u16);
    let claim = vectors.next_claim(function);
    Ok(vectors.new_line(
        IrqSource::Msi { cfg_virt, cap },
        vector,
        Some((function, claim)),
    ))
}

/// A claim of `function` that hands out no interrupt line: the earlier
/// claims' lines stop owning its interrupts all the same.
pub fn claimed_without_interrupt(function: u16) {
    VECTORS.lock().next_claim(function);
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

/// The MSI capability's message now: `(address, data, enabled)`.
fn msi_read(cfg_virt: u64, cap: u8) -> (u64, u32, bool) {
    let base = cfg_virt + cap as u64;
    // SAFETY: same as `msi_program`.
    unsafe {
        let ctrl = ((base + 2) as *const u16).read_volatile();
        let lo = ((base + 4) as *const u32).read_volatile() as u64;
        let (hi, data_off) = if ctrl & MSI_64BIT != 0 {
            (((base + 8) as *const u32).read_volatile() as u64, 0xC)
        } else {
            (0, 8)
        };
        let data = ((base + data_off) as *const u16).read_volatile() as u32;
        (hi << 32 | lo, data, ctrl & MSI_ENABLE != 0)
    }
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
    /// The claim, on the configuration space `pci::claim` hands out: its
    /// drop stops the device's bus mastering.
    pub claim: Option<crate::pci::ClaimToken>,
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

static VECTORS: IrqLock<Vectors> = IrqLock::new(Vectors {
    lines: BTreeMap::new(),
    claims: BTreeMap::new(),
});

/// DMA buffers whose memory must never be reused: their driver died with
/// a device that did not stop mastering the bus (incident 0004). Kept for
/// good, deliberately; each one was reported when it got here.
static QUARANTINE: IrqLock<alloc::vec::Vec<ObjectRef>> = IrqLock::new(alloc::vec::Vec::new());

/// Keeps `dma` (a DMA buffer) alive forever. Returns its size.
pub fn quarantine(dma: ObjectRef) -> u64 {
    let len = match &dma.kind {
        ObjectKind::Dma(d) => d.region.len,
        _ => return 0,
    };
    QUARANTINE.lock().push(dma);
    len
}

/// Creates the IRQ object for a GSI: allocates a vector, routes and registers it.
pub fn register_irq(gsi: u32, level: bool, active_low: bool) -> Result<ObjectRef, &'static str> {
    let mut vectors = VECTORS.lock();
    let vector = vectors.allocate()?;
    ioapic::route(gsi, vector, apic::bsp_id(), level, active_low)?;
    Ok(vectors.new_line(IrqSource::Gsi { gsi, level }, vector, None))
}

/// An interrupt line that no device raises, for the wait-set self-test
/// (`test=wait_set`, ADR 0044): the PIT's GSI is routed to a free vector
/// and masked at once, so only `IrqLine::fire` called by the test makes it
/// pending, exactly as the handler does. Edge-triggered, so taking its
/// interrupts never unmasks it. A real IRQ0 that slipped in before the mask
/// is drained here.
pub fn quiet_line_for_selftest() -> ObjectRef {
    let (gsi, _) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(0));
    let obj = register_irq(gsi, false, false).expect("a free device vector");
    ioapic::set_masked(gsi, true);
    if let ObjectKind::Irq(l) = &obj.kind {
        let _ = l.take_pending();
    }
    obj
}

/// Entry from the APIC for device vectors.
pub fn handle_device_irq(vector: u8) {
    crate::random::stir();
    let line = VECTORS.lock().lines.get(&vector).and_then(Weak::upgrade);
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
            claim: None,
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
            (
                irq.clone(),
                Rights::READ.union(Rights::WAIT).union(Rights::DUPLICATE),
            ),
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
        VECTORS.lock().lines.is_empty(),
        "the vector must be freed when the line is dropped"
    );

    // 2. A user driver paints the framebuffer through an MMIO capability.
    // The screen is the driver's until the check: the kernel console would
    // repaint pixel (0,0) on its next scroll, which any log line (the
    // driver's exit) can cause once the screen is full of boot lines.
    let fb = info.framebuffer;
    let lent = crate::console::lend_framebuffer();
    if fb.address != 0 {
        let region = KernelObject::new(ObjectKind::Mmio(Region {
            phys: fb.address,
            len: fb.size,
            cacheable: false,
            source_id: None,
            claim: None,
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
        // SAFETY: framebuffer reachable through the physical map.
        let pixel = unsafe {
            (crate::memory::phys::phys_to_virt(fb.address) as *const u32).read_volatile()
        };
        // Back to the console before any assertion, so a failure shows on
        // the screen too.
        if let Some(surface) = lent {
            crate::console::take_back_framebuffer(surface);
        }
        assert_eq!(
            status,
            process::ExitStatus::Exited(0),
            "fb_driver: {status:?}"
        );
        assert_eq!(
            pixel, 0x00FF_8800,
            "the driver should have painted pixel (0,0)"
        );
    }
    log::info!(
        "drivers selftest ok (IRQ0 serviced in ring 3, {fired} pending at close, DMA allocated and mapped, framebuffer painted from ring 3)"
    );
}
