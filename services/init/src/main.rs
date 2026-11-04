// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriInit (Phase 15): first user process. It receives from the kernel the
//! `SPAWNER` handle (0x10004, `MANAGE`), the PCI root (0x10005, `READ|MANAGE`),
//! a hardware capability (0x10006, `MANAGE`) to hand out DMA, the `SYSTEM`
//! object (0x10007) and the console input (0x10008, `READ`). It launches the
//! boot services and supervises them; until a dedicated device manager exists
//! it also claims devices and launches their drivers (Phases 18 and 19).
//! If `arg` carries `init_arg::SHELL`, it launches the shell at the end
//! (Phase 21).
//!
//! Every boot runs cheap checks that touch nothing outside the machine
//! (launching, waiting, reacting to a process killed by a fault, the
//! SPAWNER rights) and read-only health checks of the block, file and
//! network services. The test clients that write to the disk and AriFS or
//! talk to the QEMU test hosts run only with `selftest` on the command line
//! (incident 0005, ADR 0032).
#![no_std]
#![no_main]

extern crate alloc;

use aricode_manifest::{Caps, Manifest};
use aricode_proto::fs::ARG_MIGRATE;
use aricode_proto::launch::{
    Entry, Reply as LaunchReply, Request as LaunchRequest, Status as LaunchStatus,
};
use aricode_proto::status;
use aricode_rt::sys::abi::driver_arg;
use aricode_rt::sys::block::{find_partition, BlockClient};
use aricode_rt::sys::fs::{FsClient, FsError};
use aricode_rt::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};
use aricode_supervisor::{Action, Exit, Policy, Service, State};
use aricode_update_index as update_index;
use aricode_update_index::session::{self, Session};

const SPAWNER: Handle = Handle(0x0001_0004);
const PCI_ROOT: Handle = Handle(0x0001_0005);
const HARDWARE: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
const CONSOLE_IN: Handle = Handle(0x0001_0008);
const FRAMEBUFFER: Handle = Handle(0x0001_0009);
/// The PS/2 stream, when the machine has an i8042 (ADR 0075).
const PS2: Handle = Handle(sys::abi::handle::INIT_PS2);

/// Bits of the service mask passed to the shell.
const SERVICE_BLOCK: u64 = 1;
const SERVICE_FS: u64 = 2;
const SERVICE_NET: u64 = 4;
const SERVICE_INPUT: u64 = 8;
const SERVICE_GUI: u64 = 16;

fn say(msg: &[u8]) {
    sys::print(b"AriInit: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Launches an initrd program and waits for it to exit.
fn run(name: &str, arg: u64) -> Result<u64, Error> {
    let child = sys::process_spawn(SPAWNER, name, arg)?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Boot check of ADR 0047 (`selftest`): `console_probe` spawned with the
/// send side of a new pair as its console writes its lines to the receive
/// side, not to the kernel console; `CONSOLE_FIRST` with a handle that is
/// not an endpoint (`Invalid`), with an endpoint side it cannot write to
/// (`Denied`) or with no handle (`Invalid`) is refused and takes nothing.
fn session_console_check() -> bool {
    let Ok((output, console)) = sys::endpoint_create_pair() else {
        say(b"session console: cannot create the pair");
        return false;
    };
    let spawned = sys::process_spawn_limited(SPAWNER, "console_probe", 0, &[console], 0, true);
    if spawned.is_err() {
        // Refused before the kernel took it (BadHandle, ignored, if taken).
        let _ = sys::handle_close(console);
    }
    let exit = spawned.and_then(|child| {
        let code = sys::process_wait(child, TIMEOUT_INFINITE);
        let _ = sys::handle_close(child);
        code
    });
    let mut text = [0u8; 256];
    let mut len = 0;
    let mut none = [Handle(0); 1];
    while let Ok((n, _)) = sys::ipc_receive(output, &mut text[len..], &mut none, 0) {
        len += n;
        if n == 0 || len == text.len() {
            break;
        }
    }
    let received = core::str::from_utf8(&text[..len]).unwrap_or("");
    if exit != Ok(0) || !received.contains("console probe: checks passed\n") {
        let _ = sys::handle_close(output);
        say(b"session console: console_probe did not write its lines to its endpoint");
        return false;
    }
    // The receive side has no WRITE: refused, and still ours afterwards.
    let receive_side = sys::process_spawn_limited(SPAWNER, "hello", 0, &[output], 0, true);
    let kept_output = sys::object_kind(output) == Ok(sys::kind::ENDPOINT);
    let _ = sys::handle_close(output);
    if receive_side != Err(Error::Denied) || !kept_output {
        say(b"session console: CONSOLE_FIRST accepted an endpoint side without WRITE");
        return false;
    }
    let Ok(system) = sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) else {
        say(b"session console: cannot duplicate SYSTEM");
        return false;
    };
    let not_endpoint = sys::process_spawn_limited(SPAWNER, "hello", 0, &[system], 0, true);
    let kept_system = sys::object_kind(system) == Ok(sys::kind::SYSTEM);
    let _ = sys::handle_close(system);
    if not_endpoint != Err(Error::Invalid) || !kept_system {
        say(b"session console: CONSOLE_FIRST accepted a handle that is not an endpoint");
        return false;
    }
    if sys::process_spawn_limited(SPAWNER, "hello", 0, &[], 0, true) != Err(Error::Invalid) {
        say(b"session console: CONSOLE_FIRST accepted a spawn without handles");
        return false;
    }
    say(b"session console: console_probe wrote to its endpoint; non-endpoint consoles refused");
    true
}

/// Finds the first PCI function with `vendor:device` and returns its index.
fn find_pci(vendor: u16, device: u16) -> Option<u64> {
    find_pci_from(vendor, device, 0)
}

/// First PCI function from vendor `vendor` with class `class.subclass`.
fn find_pci_vendor_class(vendor: u16, class: u8, subclass: u8) -> Option<u64> {
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (0..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::vendor(v) == vendor
                && sys::abi::pci_info::class(v) == class
                && sys::abi::pci_info::subclass(v) == subclass
        })
    })
}

/// First PCI function of class `class.subclass` starting at `start`.
fn find_pci_class_from(class: u8, subclass: u8, start: u64) -> Option<u64> {
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (start..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::class(v) == class && sys::abi::pci_info::subclass(v) == subclass
        })
    })
}

/// Checks that a freshly launched service answers `request` with something
/// `accept` considers valid, before publishing its endpoint (audit 4836023,
/// finding 1, generalized by the supervisor): a process that exits without
/// serving must not leave clients waiting. Returns `false` if the process
/// ended or did not answer within a few seconds.
fn probe_ready(service: Handle, driver: Handle, request: &[u8], accept: fn(&[u8]) -> bool) -> bool {
    for _ in 0..12 {
        match sys::process_wait(driver, 0) {
            Err(Error::Timeout) => {}
            _ => return false, // exited (with a code or by a fault): it will not serve
        }
        if let Some(reply) = ask(service, request, 250, &mut [0u8; 256]) {
            return accept(reply);
        }
    }
    false
}

/// One request on a fresh reply pair (ADR 0030): init keeps only the
/// receive side, so a service that dies holding the request answers
/// `PeerClosed` at once instead of letting the wait expire. Returns the
/// reply bytes, or `None` on any failure.
fn ask<'a>(service: Handle, request: &[u8], wait_ms: u64, out: &'a mut [u8]) -> Option<&'a [u8]> {
    let (reply, reply_w) = sys::endpoint_create_pair().ok()?;
    let mut none: [Handle; 0] = [];
    let got = match sys::ipc_send(service, request, &[reply_w], wait_ms) {
        Ok(()) => sys::ipc_receive(reply, out, &mut none, wait_ms).ok(),
        Err(_) => {
            let _ = sys::handle_close(reply_w);
            None
        }
    };
    let _ = sys::handle_close(reply);
    got.map(|(n, _)| &out[..n])
}

fn input_service_ready(service: Handle, driver: Handle) -> bool {
    use aricode_proto::input::{Op, Reply, Request};
    let req = Request {
        op: Op::Info,
        timeout_ms: 0,
    };
    probe_ready(service, driver, &req.encode(), |b| {
        Reply::decode(b).is_some()
    })
}

fn ping_service_ready(service: Handle, driver: Handle) -> bool {
    probe_ready(service, driver, b"ping", |b| b == b"pong")
}

/// One short request to the test service ("ping" or "die") expecting "pong".
fn ping_once(service: Handle, request: &[u8]) -> bool {
    ask(service, request, 500, &mut [0u8; 16]) == Some(b"pong".as_slice())
}

/// `bb:dd.f` of a `pci_info` value.
fn print_bdf(info: u64) {
    print_hex8(sys::abi::pci_info::bus(info));
    sys::print(b":");
    print_hex8(sys::abi::pci_info::dev(info));
    sys::print(b".");
    let mut b = [0u8; 20];
    sys::print(aricode_rt::fmt_u64(
        sys::abi::pci_info::func(info) as u64,
        &mut b,
    ));
}

fn print_hex8(v: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    sys::print(&[HEX[(v >> 4) as usize], HEX[(v & 0xF) as usize]]);
}

/// Like `find_pci`, searching from index `start`.
fn find_pci_from(vendor: u16, device: u16, start: u64) -> Option<u64> {
    let n = sys::pci_count(PCI_ROOT).ok()?;
    (start..n).find(|&i| {
        sys::pci_info(PCI_ROOT, i).is_ok_and(|v| {
            sys::abi::pci_info::vendor(v) == vendor && sys::abi::pci_info::device(v) == device
        })
    })
}

/// Claims a device and launches its driver as a resident service with the
/// transferred capabilities. Returns the process handle.
///
/// Contract with the driver: it receives, in order, the service endpoint,
/// the configuration space, the hardware capability, the IRQ (if any), only
/// the BARs that are present and then `extras` (driver specific); `arg`
/// carries a mask: bit 15 = IRQ present, bits 0..5 = BAR i present, plus
/// `arg_extra` (`abi::driver_arg`). `extras` are consumed: on any failure
/// they are closed with the claim's handles, which releases the device for
/// the next attempt (audit 2026-10-25 C, L4: a failed spawn left it claimed
/// by init for good).
fn start_driver(
    name: &str,
    index: u64,
    service: Handle,
    extras: &[Handle],
    arg_extra: u64,
) -> Result<Handle, Error> {
    let mut sent = Granted::new();
    let claimed = sys::pci_claim(PCI_ROOT, index);
    let own = sys::handle_duplicate(service, rights::READ | rights::WRITE | rights::TRANSFER);
    let mut mask = arg_extra;
    if let Ok(own) = own {
        sent.push(own);
    }
    if let Ok(handles) = claimed {
        for (i, h) in handles.iter().enumerate() {
            if h.0 == 0 {
                continue;
            }
            match i {
                2 => mask |= 1 << 15,
                3..=8 => mask |= 1 << (i - 3),
                _ => {}
            }
            sent.push(*h);
        }
    }
    for h in extras {
        sent.push(*h);
    }
    claimed?;
    own?;
    let child = sys::process_spawn_with(SPAWNER, name, mask, sent.handles())?;
    sent.take();
    Ok(child)
}

/// Reads and announces the boot state (for the no-confirmation tests).
fn report_boot_state(block_service: Handle) {
    use aricode_ab::BootState;
    let Ok(mut blk) = BlockClient::new(block_service, HARDWARE, 16) else {
        return;
    };
    let Some((first, _)) = find_partition(&mut blk, aricode_ab::PART_STATE) else {
        return;
    };
    if blk.read(first, 1).is_err() {
        return;
    }
    let Some(state) = BootState::decode(&blk.buffer()[..512]) else {
        return;
    };
    let mut buf = [0u8; 20];
    sys::print(b"AriInit: boot state: active ");
    sys::print(state.active.name().as_bytes());
    match state.pending {
        Some((slot, tries)) => {
            sys::print(b", pending ");
            sys::print(slot.name().as_bytes());
            sys::print(b" (");
            sys::print(aricode_rt::fmt_u64(tries as u64, &mut buf));
            sys::print(b" tries)\n");
        }
        None => sys::print(b", none pending\n"),
    }
}

/// Fault events about the carried kernel seed (ADR 0043); the code is the
/// `CarriedSeed` number.
const SEED_MISSING_FAULT: &str = "random seed missing";
const SEED_UNREADABLE_FAULT: &str = "random seed unreadable";
const SEED_NOT_SAVED_FAULT: &str = "random seed not saved";
const _: () = assert!(SEED_NOT_SAVED_FAULT.len() <= sys::abi::sysinfo::EVENT_NAME_LEN);
const _: () = assert!(SEED_UNREADABLE_FAULT.len() <= sys::abi::sysinfo::EVENT_NAME_LEN);

fn seed_fault(name: &str, code: sys::abi::sysinfo::CarriedSeed, why: &[u8]) {
    let _ = sys::event_record(code as u64, name);
    sys::print(b"AriInit: fault ");
    sys::print(name.as_bytes());
    sys::print(b": ");
    sys::print(why);
    sys::print(b"\n");
}

/// Says whether the kernel generator got the seed carried from the
/// previous boot (ADR 0043) and records a fault event when it did not:
/// missing (the first boot of a disk, or no `ARIBOOT` to keep one),
/// unreadable, or not used because AriBoot could not save its successor.
/// Neither the seed nor anything derived from it is ever printed.
fn report_carried_seed() {
    use sys::abi::sysinfo::CarriedSeed;
    let Some(source) = sys::random_source(SYSTEM) else {
        return say(b"the kernel does not report its random source");
    };
    match source.carried {
        CarriedSeed::Mixed => say(b"kernel generator has the seed carried from the previous boot"),
        CarriedSeed::Missing => seed_fault(
            SEED_MISSING_FAULT,
            source.carried,
            b"no seed from an earlier boot (first boot of this disk): the kernel generator starts without one",
        ),
        CarriedSeed::NoStore => seed_fault(
            SEED_MISSING_FAULT,
            source.carried,
            b"no seed store on the boot disk (no ARIBOOT partition): every boot starts without a carried seed",
        ),
        CarriedSeed::Unreadable => seed_fault(
            SEED_UNREADABLE_FAULT,
            source.carried,
            b"the seed store in ARIBOOT holds no intact copy: the kernel generator starts without one",
        ),
        CarriedSeed::NotSaved => seed_fault(
            SEED_NOT_SAVED_FAULT,
            source.carried,
            b"AriBoot could not write the successor of the carried seed, so it was not used",
        ),
    }
}

/// The seed store (ADR 0043): sectors 1 and 2 of `ARIBOOT` on the system
/// disk, with one block client kept for both refreshes of a boot.
struct SeedStore {
    blk: BlockClient,
    first: u64,
}

impl SeedStore {
    /// `None` (and says why) without a block client or an `ARIBOOT`
    /// partition: then nothing is carried to the next boot either.
    fn open(block_service: Handle) -> Option<Self> {
        let Ok(mut blk) = BlockClient::new(block_service, HARDWARE, 16) else {
            say(b"could not open the block service for the random seed");
            return None;
        };
        let Some((first, last)) = find_partition(&mut blk, aricode_ab::PART_STATE) else {
            say(b"no ARIBOOT partition: no random seed is carried to the next boot");
            return None;
        };
        if last < first + aricode_ab::seed::COPIES[1] {
            say(b"ARIBOOT has no room for the random seed");
            return None;
        }
        Some(Self { blk, first })
    }

    /// Replaces the stored seed with fresh output of the kernel generator,
    /// which by now holds the carried seed (if any), this boot's cycle
    /// counter and its interrupt timing: the next boot starts from all of
    /// it. Written to the copy not in use with the next generation, then
    /// the old copy is blanked (`aricode_ab::seed::plan`), so a power cut
    /// at any point leaves an intact copy. `when` names the moment for the
    /// console line. A failure is a `random seed not saved` fault: the
    /// next boot then gets whatever AriBoot or the previous refresh left.
    fn refresh(&mut self, when: &[u8]) {
        use aricode_ab::seed::{pick, plan, Record, COPIES};
        use sys::abi::sysinfo::CarriedSeed;
        let mut copies = [[0u8; 512]; 2];
        let mut ok = [false; 2];
        for (i, copy) in copies.iter_mut().enumerate() {
            if self.blk.read(self.first + COPIES[i], 1).is_ok() {
                copy.copy_from_slice(&self.blk.buffer()[..512]);
                ok[i] = true;
            }
        }
        aricode_crypto::wipe(&mut self.blk.buffer()[..512]);
        let mut current = pick([
            ok[0].then_some(&copies[0][..]),
            ok[1].then_some(&copies[1][..]),
        ]);
        for copy in copies.iter_mut() {
            aricode_crypto::wipe(copy);
        }
        let p = plan(&current);
        if let aricode_ab::seed::Stored::Found { record, .. } = &mut current {
            aricode_crypto::wipe(&mut record.seed);
        }
        let mut seed = [0u8; 32];
        if sys::random(SYSTEM, &mut seed) != Ok(seed.len()) {
            aricode_crypto::wipe(&mut seed);
            return seed_fault(
                SEED_NOT_SAVED_FAULT,
                CarriedSeed::NotSaved,
                b"the kernel generator did not answer",
            );
        }
        let mut sector = Record {
            generation: p.generation,
            seed,
        }
        .encode();
        aricode_crypto::wipe(&mut seed);
        self.blk.buffer()[..512].copy_from_slice(&sector);
        aricode_crypto::wipe(&mut sector);
        // Flushed before the old copy is blanked, so a device that
        // reorders writes cannot lose both.
        let written =
            self.blk.write(self.first + COPIES[p.write], 1).is_ok() && self.blk.flush().is_ok();
        // The DMA buffer held the new seed: blank it whatever happened,
        // and blank the old copy with it.
        aricode_crypto::wipe(&mut self.blk.buffer()[..512]);
        if !written {
            return seed_fault(
                SEED_NOT_SAVED_FAULT,
                CarriedSeed::NotSaved,
                b"could not write the random seed to ARIBOOT",
            );
        }
        if self.blk.write(self.first + COPIES[p.blank], 1).is_err() || self.blk.flush().is_err() {
            say(b"could not blank the old random seed copy (the new one is newer)");
        }
        let mut buf = [0u8; 20];
        sys::print(b"AriInit: random seed saved for the next boot (");
        sys::print(when);
        sys::print(b", generation ");
        sys::print(aricode_rt::fmt_u64(p.generation, &mut buf));
        sys::print(b")\n");
    }
}

/// A/B boot: if the kernel booted from a pending slot, mark it active in
/// `ARIBOOT` now that all boot services work.
fn confirm_boot_slot(arg: u64, block_service: Handle) {
    use aricode_ab::{confirm, BootState, Slot};
    let slot = if arg & sys::abi::init_arg::BOOT_SLOT_A != 0 {
        Slot::A
    } else if arg & sys::abi::init_arg::BOOT_SLOT_B != 0 {
        Slot::B
    } else {
        say(b"booted from the ESP (no slots)");
        return;
    };
    sys::print(b"AriInit: booted from slot ");
    sys::print(slot.name().as_bytes());
    if arg & sys::abi::init_arg::BOOT_PENDING == 0 {
        sys::print(b" (active)\n");
        return;
    }
    sys::print(b" (pending confirmation)\n");
    let Ok(mut blk) = BlockClient::new(block_service, HARDWARE, 16) else {
        say(b"could not open the block service to confirm");
        return;
    };
    let Some((first, _)) = find_partition(&mut blk, aricode_ab::PART_STATE) else {
        say(b"no ARIBOOT partition, not confirming");
        return;
    };
    if blk.read(first, 1).is_err() {
        return say(b"could not read the boot state");
    }
    let Some(state) = BootState::decode(&blk.buffer()[..512]) else {
        return say(b"invalid boot state, not confirming");
    };
    let confirmed = confirm(state, slot);
    blk.buffer()[..512].copy_from_slice(&confirmed.encode());
    match blk.write(first, 1) {
        Ok(()) => {
            sys::print(b"AriInit: boot confirmed on slot ");
            sys::print(slot.name().as_bytes());
            sys::print(b"\n");
        }
        Err(_) => say(b"could not write the boot state"),
    }
}

/// Whether every slot that holds a system declares that it reads AriFS v2
/// (ADR 0072): only then may fsd migrate a v1 volume, since a rollback to
/// either slot finds a system that mounts it. A slot without a valid
/// header holds no system to go back to; past the header, anything that
/// does not show `arifs = 2` (unreadable, malformed) counts as v1, as the
/// updater counts it (audit of ADR 0072, L3).
fn slots_read_arifs_v2(blk: &mut BlockClient) -> bool {
    use aricode_ab::Image;
    for part in [aricode_ab::PART_SLOT_A, aricode_ab::PART_SLOT_B] {
        let Some((first, last)) = find_partition(blk, part) else {
            continue;
        };
        if blk.read(first, 1).is_err() {
            return false;
        }
        let Ok(len) = Image::container_len(&blk.buffer()[..512]) else {
            continue;
        };
        if len as u64 > (last - first) * 512 {
            continue;
        }
        let features = Image::features(len as usize, |offset, buf| {
            read_bytes(blk, first + 1, offset, buf)
        });
        if !features.is_ok_and(|f| f.arifs >= 2) {
            return false;
        }
    }
    true
}

/// Whether the ESP's system, which AriBoot boots when no slot verifies,
/// reads AriFS v2 (audit of ADR 0072, M1): every stick written before ADR
/// 0072 keeps a v1-only system there, which nothing updates. A disk
/// without an ESP partition, or an ESP without a system, has nothing to
/// fall back to; anything else that does not show `arifs = 2` counts as v1.
fn esp_reads_arifs_v2(blk: &mut BlockClient) -> bool {
    use aricode_fat::{Fat32, FatError};
    let Some((first, _)) = find_partition(blk, "ESP") else {
        return true;
    };
    let Ok(mut fat) = Fat32::parse(EspSectors { blk, first }) else {
        return false;
    };
    let mut initrd = match fat.open("aricodeos/initrd.img") {
        Ok(f) => f,
        Err(FatError::NotFound) => return true,
        Err(_) => return false,
    };
    let len = initrd.size as usize;
    aricode_ab::initrd_features(len, |offset, buf| {
        fat.read_at(&mut initrd, offset as u32, buf).map_err(|_| ())
    })
    .is_ok_and(|f| f.arifs >= 2)
}

/// The ESP's sectors through the block client, for the FAT reader.
struct EspSectors<'a> {
    blk: &'a mut BlockClient,
    first: u64,
}

impl aricode_fat::Sectors for EspSectors<'_> {
    fn read_sector(
        &mut self,
        sector: u64,
        buf: &mut [u8; 512],
    ) -> Result<(), aricode_fat::FatError> {
        self.blk
            .read(self.first + sector, 1)
            .map_err(|_| aricode_fat::FatError::Corrupt)?;
        buf.copy_from_slice(&self.blk.buffer()[..512]);
        Ok(())
    }
}

/// Fills `buf` from byte `offset` of the area that starts at `base` (LBA),
/// up to the block client's batch at a time.
fn read_bytes(blk: &mut BlockClient, base: u64, offset: usize, buf: &mut [u8]) -> Result<(), ()> {
    let mut done = 0;
    while done < buf.len() {
        let at = offset + done;
        let in_sector = at % 512;
        let sectors = (in_sector + buf.len() - done).div_ceil(512).min(16);
        blk.read(base + (at / 512) as u64, sectors as u16)
            .map_err(|_| ())?;
        let n = (sectors * 512 - in_sector).min(buf.len() - done);
        buf[done..done + n].copy_from_slice(&blk.buffer()[in_sector..in_sector + n]);
        done += n;
    }
    Ok(())
}

/// On top of an already running block service: fsd on the ARI0 partition
/// (found by GPT name: the test disk and the USB image place it at different
/// LBAs). The read-only health checks always run: the block service answers
/// Info and a read of the GPT, the file service answers a list. With
/// `selftest` the test clients run too (`fs_client` creates and rewrites
/// `nuevo.txt`, `blk_client` writes a sector of the raw area; incident
/// 0005), and `blk_late` on the LBA of `blk_fault` when the command line
/// has it (incident 0017). Returns the fsd endpoint and the file client of
/// the health check (init keeps using it: clients are not released), or
/// init's exit code.
fn start_storage_stack(
    sup: &mut Supervisor,
    service: Handle,
    selftest: bool,
    blk_fault: Option<u64>,
) -> Result<(Handle, FsClient), u64> {
    let mut buf = [0u8; 20];
    let Ok(fs_ep) = sys::endpoint_create() else {
        return Err(12);
    };
    let disk = BlockClient::new(service, HARDWARE, 16)
        .ok()
        .and_then(|mut blk| {
            Some((
                blk.capacity().ok()?,
                find_partition(&mut blk, "ARI0"),
                slots_read_arifs_v2(&mut blk) && esp_reads_arifs_v2(&mut blk),
            ))
        });
    let Some((capacity, ari0, migrate)) = disk else {
        say(b"block service does not answer Info");
        return Err(11);
    };
    let Some((ari0_lba, _)) = ari0 else {
        say(b"no ARI0 partition on the disk");
        return Err(12);
    };
    sys::print(b"AriInit: block service answers: ");
    sys::print(aricode_rt::fmt_u64(capacity, &mut buf));
    sys::print(b" sectors, GPT read, ARI0 at LBA ");
    sys::print(aricode_rt::fmt_u64(ari0_lba, &mut buf));
    sys::print(b"\n");
    if !migrate {
        say(b"a slot or the ESP holds a system that reads only AriFS v1: ARI0 is not migrated");
    }
    let launched = sup
        .register(
            "fsd",
            Kind::Fs {
                block_service: service,
                partition_lba: ari0_lba | if migrate { ARG_MIGRATE } else { 0 },
            },
            Probe::None,
            fs_ep,
            false,
            Policy::DRIVER,
        )
        .is_some_and(|i| sup.launch(i));
    if !launched {
        say(b"fsd did not start");
        return Err(13);
    }
    let Some(files) = count_files(fs_ep, sup) else {
        say(b"file service does not answer a list");
        return Err(14);
    };
    let Ok(fs) = FsClient::new(fs_ep, HARDWARE) else {
        say(b"no buffer for the file service client");
        return Err(14);
    };
    sys::print(b"AriInit: file service answers: ");
    sys::print(aricode_rt::fmt_u64(files, &mut buf));
    sys::print(b" files\n");
    if selftest {
        run_test_client(sup, "fs_client", fs_ep, 0, b"file service ok", 14)?;
        run_test_client(
            sup,
            "blk_client",
            service,
            RAW_AREA_LBA,
            b"block service ok",
            11,
        )?;
        if let Some(lba) = blk_fault {
            run_test_client(
                sup,
                "blk_late",
                service,
                lba,
                b"late completion test ok",
                11,
            )?;
        }
    }
    Ok((fs_ep, fs))
}

/// Read-only health check of the file service: how many entries its root
/// lists, or `None` if it does not answer. Each List request waits up to the
/// file client's reply time (`fs::REPLY_WAIT_MS`, a slow stick may still be
/// mounting) while the supervisor watches fsd: if fsd dies for good (a
/// corrupt AriFS) its endpoint is retired and the request fails at once
/// (ADR 0030). No DMA buffer: List carries none.
fn count_files(fs_service: Handle, sup: &mut Supervisor) -> Option<u64> {
    use aricode_proto::fs::{Op, Reply, Request, Status};
    let mut files = 0u32;
    loop {
        let mut req = Request::new(Op::List);
        req.fd = files;
        let mut out = [0u8; Reply::LEN];
        let reply = ask_supervised(
            sup,
            fs_service,
            &req.encode(),
            sys::fs::REPLY_WAIT_MS,
            &mut out,
        )
        .and_then(Reply::decode)?;
        match reply.status {
            Status::Ok => files += 1,
            Status::NotFound => return Some(files as u64),
            _ => return None,
        }
    }
}

/// Like `ask`, but the reply wait (up to `wait_ms`) is cut into short
/// receives with `Supervisor::poll` in between: a service that dies for
/// good while holding or queueing the request has its endpoint retired and
/// the wait ends at once with `PeerClosed`.
fn ask_supervised<'a>(
    sup: &mut Supervisor,
    service: Handle,
    request: &[u8],
    wait_ms: u64,
    out: &'a mut [u8],
) -> Option<&'a [u8]> {
    let (reply, reply_w) = sys::endpoint_create_pair().ok()?;
    if sys::ipc_send(service, request, &[reply_w], 1000).is_err() {
        let _ = sys::handle_close(reply_w);
        let _ = sys::handle_close(reply);
        return None;
    }
    let deadline = now_ms() + wait_ms;
    let mut none: [Handle; 0] = [];
    let got = loop {
        match sys::ipc_receive(reply, out, &mut none, 100) {
            Ok((n, _)) => break Some(n),
            Err(Error::Timeout) if now_ms() < deadline => sup.poll(),
            Err(_) => break None,
        }
    };
    let _ = sys::handle_close(reply);
    got.map(|n| &out[..n])
}

/// Read-only health check of the network service: the address it reports.
/// It polls while DHCP is pending until `wait_ms` has passed (0: a single
/// request). Each request is bounded by `NET_CALL_MS` for the send and as
/// much for the reply, so the whole check takes at most `wait_ms + 2 *
/// NET_CALL_MS`. It uses no DMA buffer (Info carries none) and its reply
/// endpoint is closed by `rpc::call`. A machine with no cable or no DHCP
/// server is not a boot failure, only reported.
fn report_network_address(net_service: Handle, wait_ms: u64) {
    use aricode_proto::net::{Info, Op, Reply, Request, Status};
    let deadline = now_ms() + wait_ms;
    let request = Request::new(Op::Info).encode();
    loop {
        let mut msg = [0u8; Reply::LEN];
        let reply = sys::rpc::call(
            net_service,
            &request,
            &[],
            NET_CALL_MS,
            NET_CALL_MS,
            &mut msg,
        )
        .ok()
        .and_then(|n| Reply::decode(&msg[..n]));
        match reply.map(|r| (r.status, r)) {
            Some((Status::Ok, r)) => {
                let mut b = [0u8; 20];
                sys::print(b"AriInit: network service has address ");
                for (i, octet) in Info::decode(&r.data).address.iter().enumerate() {
                    if i > 0 {
                        sys::print(b".");
                    }
                    sys::print(aricode_rt::fmt_u64(*octet as u64, &mut b));
                }
                sys::print(b"\n");
                return;
            }
            Some((Status::NoRoute, _)) if now_ms() + NET_POLL_MS < deadline => {
                sys::thread_sleep(NET_POLL_MS)
            }
            Some((Status::NoRoute, _)) => {
                return say(b"network service: no address yet (DHCP pending)")
            }
            _ => return say(b"network service does not answer Info"),
        }
    }
}

/// Bound of each wait (send, reply) of a network health request.
const NET_CALL_MS: u64 = 250;
/// Pause between network health requests while DHCP is pending.
const NET_POLL_MS: u64 = 20;
/// How long the health check waits for DHCP on virtio-net, where QEMU's
/// DHCP server answers in milliseconds. On Realtek it does not wait (0):
/// the address is whatever DHCP has given by the time the USB devices are
/// up, and the shell's `net` shows it later.
const NET_WAIT_VIRTIO_MS: u64 = 2000;

/// Runs a test client (`selftest` only) with `run_block_client_supervised`
/// (the services it talks to are watched while it runs, ADR 0030) and
/// announces `ok` if it exits with 0; otherwise `Err(code)` is init's exit
/// code.
fn run_test_client(
    sup: &mut Supervisor,
    name: &str,
    service: Handle,
    arg: u64,
    ok: &[u8],
    code: u64,
) -> Result<(), u64> {
    match run_block_client_supervised(name, service, arg, sup) {
        Ok(0) => {
            say(ok);
            Ok(())
        }
        Ok(exit) => {
            let mut buf = [0u8; 20];
            sys::print(b"AriInit: ");
            sys::print(name.as_bytes());
            sys::print(b" exited with ");
            sys::print(aricode_rt::fmt_u64(exit, &mut buf));
            sys::print(b"\n");
            Err(code)
        }
        Err(e) => {
            sys::print(b"AriInit: ");
            aricode_rt::print_error(name.as_bytes(), e);
            Err(code)
        }
    }
}

/// Launches fsd with its endpoint (READ|WRITE), the block service endpoint
/// (WRITE) and a hardware capability; `arg` = partition LBA.
fn start_fsd(
    fs_service: Handle,
    block_service: Handle,
    partition_lba: u64,
) -> Result<Handle, Error> {
    // Closed again if a later step fails.
    let mut sent = Granted::new();
    sent.push(sys::handle_duplicate(
        fs_service,
        rights::READ | rights::WRITE | rights::TRANSFER,
    )?);
    sent.push(sys::handle_duplicate(
        block_service,
        rights::WRITE | rights::TRANSFER,
    )?);
    sent.push(sys::handle_duplicate(
        HARDWARE,
        rights::MANAGE | rights::TRANSFER,
    )?);
    // SYSTEM (read): clock to expire abandoned chunked replaces.
    sent.push(sys::handle_duplicate(
        SYSTEM,
        rights::READ | rights::TRANSFER,
    )?);
    // A migration is asked for on fsd's first launch only: a relaunch
    // (after a failed migration, or any exit) serves what the disk holds,
    // and never migrates later than the decision taken at boot, when the
    // updater may already have installed a system that reads only v1
    // (audit of ADR 0072, L4 and L6).
    let arg = if MIGRATE_ASKED.swap(true, core::sync::atomic::Ordering::Relaxed) {
        partition_lba & !ARG_MIGRATE
    } else {
        partition_lba
    };
    let child = sys::process_spawn_with(SPAWNER, "fsd", arg, sent.handles())?;
    sent.take();
    Ok(child)
}

/// How a resident service is (re)launched.
#[derive(Clone, Copy)]
enum Kind {
    /// PCI driver with the `start_driver` contract; `with_system` adds
    /// `SYSTEM` (read) as an extra and `block` a block service endpoint
    /// (ADR 0023: usbd serves it if it finds storage).
    Driver {
        pci_index: u64,
        with_system: bool,
        block: Option<Handle>,
    },
    /// fsd on top of the block service.
    Fs {
        block_service: Handle,
        partition_lba: u64,
    },
    /// Initrd program with its service endpoint (test services).
    Program { arg: u64 },
    /// Console bridge (ADR 0013): the console input with `MANAGE` and the
    /// input service endpoints. It does not serve requests, so its resident
    /// has no endpoint of its own.
    ConsoleBridge {
        inputs: [Handle; MAX_INPUTS],
        count: usize,
    },
    /// SSH server (ADR 0025): network and file endpoints, and the endpoint
    /// where it asks init for session shells.
    Sshd {
        fs: Handle,
        net: Handle,
        sessions: Handle,
    },
    /// The PS/2 input service (ADR 0075): its endpoint, the PS/2 stream and
    /// `SYSTEM` (read) for its clock.
    Ps2Input,
}

/// Input services that init hands out to the console bridge (and the
/// desktop's compositor, which also gets the PS/2 one).
const MAX_INPUTS: usize = 4;

/// Input services the compositor gets: the console bridge's and the PS/2
/// one, which never goes to the bridge (ADR 0075): its keys flow only while
/// a process holds the screen, and the bridge would type them at the
/// console the kernel keeps them from.
const MAX_GUI_INPUTS: usize = MAX_INPUTS + 1;

/// Launches `ps2_input` (ADR 0075) on `endpoint` (receive side) with a
/// duplicate of the PS/2 stream to read and wait on (and move, which a
/// spawn needs; the kernel lets only one process drain the stream).
fn start_ps2_input(endpoint: Handle) -> Result<Handle, Error> {
    let mut granted = Granted::new();
    granted.push(sys::handle_duplicate(
        endpoint,
        rights::READ | rights::WRITE | rights::TRANSFER,
    )?);
    granted.push(sys::handle_duplicate(
        PS2,
        rights::READ | rights::WAIT | rights::TRANSFER,
    )?);
    granted.push(sys::handle_duplicate(
        SYSTEM,
        rights::READ | rights::TRANSFER,
    )?);
    let child = sys::process_spawn_with(SPAWNER, "ps2_input", 0, granted.handles())?;
    granted.take();
    Ok(child)
}

/// Launches the console bridge with the console input (`MANAGE`) and a
/// duplicate (`WRITE`) of each input endpoint; `arg` = how many, with the
/// keyboard layout above them. If a duplicate fails, closes the ones
/// already made.
fn start_console_bridge(inputs: &[Handle]) -> Result<Handle, Error> {
    let mut handles = [Handle(0); MAX_INPUTS + 1];
    let mut made = 0;
    let result = (|| {
        handles[0] = sys::handle_duplicate(CONSOLE_IN, rights::MANAGE | rights::TRANSFER)?;
        made = 1;
        for ep in inputs {
            handles[made] = sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER)?;
            made += 1;
        }
        let layout = KBD_LAYOUT.load(core::sync::atomic::Ordering::Relaxed);
        sys::process_spawn_with(
            SPAWNER,
            "console_bridge",
            inputs.len() as u64 | layout << 8,
            &handles[..made],
        )
    })();
    if result.is_err() {
        for h in &handles[..made] {
            let _ = sys::handle_close(*h);
        }
    }
    result
}

/// sshd's private directory (ADR 0050): its host key and `random_seed`.
const SSHD_DIR: &str = "sshd";
const SSHD_FILES: [&str; 2] = ["ssh_host_ed25519_key", "random_seed"];
/// The keys sshd accepts: in the AriFS root, where the owner writes them
/// (`write authorized_keys ...`); sshd only reads them.
const AUTHORIZED_KEYS: &str = "authorized_keys";

/// How long init waits for fsd to grant sshd's sessions: it does so from
/// its supervisor loop, which must not stall behind a file service that
/// does not answer (a failed grant is a failed launch, retried later).
const SSHD_GRANT_MS: u64 = 2_000;

/// A whole small file: `None` if it does not exist; an error if it cannot
/// be read or does not fit `buf`.
fn read_small(fs: &mut FsClient, name: &str, buf: &mut [u8]) -> Result<Option<usize>, ()> {
    let fd = match fs.open(name) {
        Ok(fd) => fd,
        Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => return Ok(None),
        Err(_) => return Err(()),
    };
    let r = fs.read(fd, 0, buf);
    let _ = fs.close(fd);
    match r {
        Ok(n) if n < buf.len() => Ok(Some(n)),
        _ => Err(()),
    }
}

/// Moves sshd's files from the AriFS root, where releases before ADR 0050
/// kept them, into `sshd/`. The root copy is then overwritten with zeros
/// and emptied (AriFS cannot remove a file, and the root must not keep the
/// host key), but only once `sshd/` holds the same bytes, read back: a
/// copy that failed half-way is completed on the next boot, never trusted.
/// When both hold different non-empty contents (a key written to the root
/// after the move, by hand or by an older release), neither is touched and
/// init says so: no key is ever destroyed. Each outcome is said.
fn move_sshd_files(fs: &mut FsClient) {
    let mut root = [0u8; 256];
    let mut moved = [0u8; 256];
    let mut name_buf = [0u8; 64];
    for name in SSHD_FILES {
        let say_file = |what: &[u8]| {
            sys::print(b"AriInit: ");
            sys::print(name.as_bytes());
            sys::print(what);
        };
        let n = match read_small(fs, name, &mut root) {
            Ok(Some(n)) if n > 0 => n,
            Ok(_) => continue,
            Err(()) => {
                say_file(b": unreadable or too large in the root; not moved into sshd/\n");
                continue;
            }
        };
        let dir_len = SSHD_DIR.len();
        name_buf[..dir_len].copy_from_slice(SSHD_DIR.as_bytes());
        name_buf[dir_len] = b'/';
        name_buf[dir_len + 1..dir_len + 1 + name.len()].copy_from_slice(name.as_bytes());
        let target = core::str::from_utf8(&name_buf[..dir_len + 1 + name.len()]).unwrap_or("");
        match read_small(fs, target, &mut moved) {
            Ok(Some(m)) if moved[..m] == root[..n] => {}
            Ok(Some(m)) if m > 0 => {
                say_file(b" in the root differs from sshd/'s: both left in place\n");
                continue;
            }
            // Absent, or empty from a copy that broke off.
            Ok(_) => {
                let written = fs
                    .open(target)
                    .or_else(|_| fs.create(target))
                    .and_then(|fd| {
                        let r = fs.replace(fd, &root[..n]);
                        let _ = fs.close(fd);
                        r
                    })
                    .is_ok();
                let same = written
                    && matches!(read_small(fs, target, &mut moved), Ok(Some(m)) if moved[..m] == root[..n]);
                if !same {
                    say_file(b": could not move into sshd/; the root copy is kept\n");
                    continue;
                }
            }
            Err(()) => {
                say_file(b": sshd/'s copy is unreadable; the root copy is kept\n");
                continue;
            }
        }
        let blanked = fs.open(name).is_ok_and(|fd| {
            let zeros = [0u8; 256];
            let r = fs.replace(fd, &zeros[..n]).is_ok() && fs.replace(fd, &[]).is_ok();
            let _ = fs.close(fd);
            r
        });
        say_file(if blanked {
            b" moved into sshd/\n" as &[u8]
        } else {
            b" copied into sshd/, but the root copy could not be emptied\n"
        });
    }
}

/// Launches sshd with `[net session (WRITE), its fsd session (sshd/,
/// WRITE), hardware, SYSTEM (READ), sessions (WRITE), authorized_keys
/// (read-only fsd session)]`. It gets no SPAWNER: shells are launched by
/// init. Every generation gets network and file sessions of its own (ADR
/// 0037, 0050): the listener of one that died is closed instead of holding
/// port 22, its descriptors are released, and it sees only its own files
/// and the keys it accepts.
fn start_sshd(fs: Handle, net: Handle, sessions: Handle) -> Result<Handle, Error> {
    let w = rights::WRITE | rights::TRANSFER;
    // The guard closes every handle made so far if a later step fails (the
    // kernel takes them only when it creates the process).
    let mut granted = Granted::new();
    let fsd = file_grant(sys::fs::bind(fs, SSHD_DIR, SSHD_GRANT_MS))?;
    granted.push(fsd);
    let keys = file_grant(sys::fs::share_may_be_missing(
        fs,
        AUTHORIZED_KEYS,
        SSHD_GRANT_MS,
    ))?;
    granted.push(keys);
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    granted.push(hw);
    let system = sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER)?;
    granted.push(system);
    let shells = sys::handle_duplicate(sessions, w)?;
    granted.push(shells);
    let netd = attach_net_session(net, "sshd", aricode_proto::net::SSH_PORT)?;
    granted.push(netd);
    let child =
        sys::process_spawn_with(SPAWNER, "sshd", 0, &[netd, fsd, hw, system, shells, keys])?;
    granted.take();
    Ok(child)
}

/// How to check that the service is already serving.
#[derive(Clone, Copy)]
enum Probe {
    /// The health checks (and, with `selftest`, the test clients) check it
    /// afterwards (block, files, network).
    None,
    /// `InputServiceProtocol::Info`.
    Input,
    /// "ping" -> "pong".
    Ping,
}

/// A resident service under supervision. The endpoint belongs to init and
/// does not change across generations: a client keeps talking to "the
/// service" even if the process serving it has been restarted.
struct Resident {
    name: &'static str,
    kind: Kind,
    probe: Probe,
    endpoint: Handle,
    process: Option<Handle>,
    /// A process that outlived its kill (`Service::abandon`): watched so
    /// that its endpoints are retired once it does exit.
    abandoned: Option<Handle>,
    sup: Service,
    /// Extra driver argument bits for every (re)launch (`abi::driver_arg`).
    driver_arg: u64,
}

impl Resident {
    /// The service will not be relaunched: closes the endpoints it served
    /// (ADR 0030). init keeps their receive side for the next generation,
    /// so without this the kernel cannot tell that nobody will serve them
    /// and every client would wait its full reply timeout; closed, queued
    /// and future requests fail at once with `PeerClosed`.
    fn retire_endpoints(&self) {
        let block = match self.kind {
            Kind::Driver { block, .. } => block,
            _ => None,
        };
        let mut closed = false;
        for ep in [Some(self.endpoint), block]
            .into_iter()
            .flatten()
            .filter(|h| h.0 != 0)
        {
            closed |= sys::endpoint_close(ep).is_ok();
        }
        if closed {
            sys::print(b"AriInit: endpoints of ");
            sys::print(self.name.as_bytes());
            sys::print(b" closed: their clients fail at once\n");
        }
    }
}

const MAX_RESIDENTS: usize = 12;

/// Supervisor of resident services (stage 1 of the post-roadmap plan):
/// states, generations, restart with increasing backoff and a budget, and
/// queryable status (`status` v1) for the shell.
/// `strict` on the command line (ADR 0028): nothing is restarted.
static STRICT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The keyboard layout of `kbd=` (`Layout::code`), for the console bridge:
/// USB and virtio keyboards type at the console as the kernel's PS/2 one.
static KBD_LAYOUT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// fsd was launched once: later launches never carry `ARG_MIGRATE`.
static MIGRATE_ASKED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn strict() -> bool {
    STRICT.load(core::sync::atomic::Ordering::Relaxed)
}

struct Supervisor {
    slots: [Option<Resident>; MAX_RESIDENTS],
    count: usize,
}

fn now_ms() -> u64 {
    let mut b = [0u8; 8];
    match sys::system_info(SYSTEM, sys::abi::sysinfo::UPTIME, &mut b) {
        Ok(8) => u64::from_le_bytes(b),
        _ => 0,
    }
}

impl Supervisor {
    fn new() -> Self {
        Self {
            slots: [const { None }; MAX_RESIDENTS],
            count: 0,
        }
    }

    fn register(
        &mut self,
        name: &'static str,
        kind: Kind,
        probe: Probe,
        endpoint: Handle,
        optional: bool,
        policy: Policy,
    ) -> Option<usize> {
        if self.count == MAX_RESIDENTS {
            return None;
        }
        let mut policy = policy;
        if strict() {
            policy.max_restarts = 0;
        }
        let i = self.count;
        self.slots[i] = Some(Resident {
            name,
            kind,
            probe,
            endpoint,
            process: None,
            abandoned: None,
            sup: Service::new(policy, optional),
            driver_arg: 0,
        });
        self.count += 1;
        Some(i)
    }

    fn generation(&self, i: usize) -> u32 {
        self.slots[i].as_ref().map_or(0, |r| r.sup.generation())
    }

    fn state(&self, i: usize) -> State {
        self.slots[i]
            .as_ref()
            .map_or(State::Stopped, |r| r.sup.state())
    }

    fn endpoint(&self, i: usize) -> Handle {
        self.slots[i].as_ref().map_or(Handle(0), |r| r.endpoint)
    }

    fn has_process(&self, i: usize) -> bool {
        self.slots[i].as_ref().is_some_and(|r| r.process.is_some())
    }

    /// Extra argument bits for the next launches of driver `i`.
    fn set_driver_arg(&mut self, i: usize, bits: u64) {
        if let Some(r) = self.slots[i].as_mut() {
            r.driver_arg = bits;
        }
    }

    /// Retires service `i`: it will not be relaunched even if it dies.
    fn stop(&mut self, i: usize) {
        if let Some(r) = self.slots[i].as_mut() {
            r.sup.stop();
        }
    }

    /// Launches (or relaunches) service `i` and waits until it serves.
    /// Returns whether it became ready; if not, the supervisor has already
    /// decided what to do.
    fn launch(&mut self, i: usize) -> bool {
        let Some(r) = self.slots[i].as_mut() else {
            return false;
        };
        let spawned = match r.kind {
            Kind::Driver {
                pci_index,
                with_system,
                block,
            } => {
                let mut extras = [Handle(0); 2];
                let mut n = 0;
                let mut failed = false;
                if with_system {
                    match sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) {
                        Ok(h) => {
                            extras[n] = h;
                            n += 1;
                        }
                        Err(_) => failed = true,
                    }
                }
                if let Some(ep) = block {
                    match sys::handle_duplicate(ep, rights::READ | rights::WRITE | rights::TRANSFER)
                    {
                        Ok(h) => {
                            extras[n] = h;
                            n += 1;
                        }
                        Err(_) => failed = true,
                    }
                }
                if failed {
                    for h in &extras[..n] {
                        let _ = sys::handle_close(*h);
                    }
                    Err(Error::Limit)
                } else {
                    start_driver(r.name, pci_index, r.endpoint, &extras[..n], r.driver_arg)
                }
            }
            Kind::Fs {
                block_service,
                partition_lba,
            } => start_fsd(r.endpoint, block_service, partition_lba),
            Kind::Program { arg } => {
                sys::handle_duplicate(r.endpoint, rights::READ | rights::WRITE | rights::TRANSFER)
                    .and_then(|ep| {
                        let spawned = sys::process_spawn_with(SPAWNER, r.name, arg, &[ep]);
                        if spawned.is_err() {
                            let _ = sys::handle_close(ep);
                        }
                        spawned
                    })
            }
            Kind::ConsoleBridge { inputs, count } => start_console_bridge(&inputs[..count]),
            Kind::Sshd { fs, net, sessions } => start_sshd(fs, net, sessions),
            Kind::Ps2Input => start_ps2_input(r.endpoint),
        };
        let now = now_ms();
        let process = match spawned {
            Ok(h) => h,
            Err(e) => {
                aricode_rt::print_error(b"AriInit: launch service", e);
                let _ = r.sup.starting(now);
                self.announce_death(i, now, Exit::Code(u64::MAX), true);
                return false;
            }
        };
        r.process = Some(process);
        let generation = r.sup.starting(now);
        let ready = match r.probe {
            Probe::None => true,
            Probe::Input => input_service_ready(r.endpoint, process),
            Probe::Ping => ping_service_ready(r.endpoint, process),
        };
        if ready {
            r.sup.ready(now_ms());
            if generation > 1 {
                sys::print(b"AriInit: service ");
                sys::print(r.name.as_bytes());
                sys::print(b" restarted (generation ");
                let mut b = [0u8; 20];
                sys::print(aricode_rt::fmt_u64(generation as u64, &mut b));
                sys::print(b")\n");
            }
            true
        } else {
            self.start_timeout(i);
            false
        }
    }

    /// The service never became ready: if it is still alive it is killed
    /// and we wait for the kernel to reap it before declaring the generation
    /// dead; that way the next one does not compete with it for its devices
    /// (audit 6ee19fb, finding 4). If it cannot be killed, it is left failed
    /// and not relaunched.
    fn start_timeout(&mut self, i: usize) {
        let Some(r) = self.slots[i].as_mut() else {
            return;
        };
        let mut stuck = false;
        if let Some(h) = r.process {
            if matches!(sys::process_wait(h, 0), Err(Error::Timeout)) {
                let _ = sys::process_kill(h);
                stuck = matches!(sys::process_wait(h, 2000), Err(Error::Timeout));
            }
        }
        if stuck {
            r.abandoned = r.process.take();
        }
        // A process that could not be killed may still be serving: its
        // endpoints stay open (audit robustness-1, reviewer 1), and no new
        // generation competes with it.
        self.announce_death(i, now_ms(), Exit::StartTimeout, !stuck);
    }

    /// Records the death of `i`, closes its process and announces the
    /// decision. `gone`: the process no longer runs, so its endpoints may be
    /// closed if it will not be relaunched; if it still runs after a kill
    /// the service fails for good (audit 2026-10-25 C, L5: it used to be
    /// relaunched next to the old one).
    fn announce_death(&mut self, i: usize, now: u64, exit: Exit, gone: bool) {
        let Some(r) = self.slots[i].as_mut() else {
            return;
        };
        if let Some(h) = r.process.take() {
            let _ = sys::handle_close(h);
        }
        let action = if gone {
            r.sup.died(now, exit)
        } else {
            r.sup.abandon(exit)
        };
        sys::print(b"AriInit: service ");
        sys::print(r.name.as_bytes());
        sys::print(match exit {
            Exit::Code(_) => b" exited",
            Exit::Killed => b" died from a fault",
            Exit::StartTimeout => b" never became ready",
        });
        let mut b = [0u8; 20];
        match action {
            Action::Restart { at } => {
                sys::print(b"; restart in ");
                sys::print(aricode_rt::fmt_u64(at.saturating_sub(now), &mut b));
                sys::print(b" ms\n");
            }
            _ if !gone => sys::print(b"; still running after a kill: failed, not relaunched\n"),
            Action::Nothing => sys::print(b"; retired: not restarted\n"),
            _ if strict() => sys::print(b"; strict mode: not restarted (inspect with diag)\n"),
            _ => {
                sys::print(b"; restart budget exhausted: ");
                sys::print(if r.sup.is_optional() {
                    b"system degraded\n"
                } else {
                    b"required service failed\n"
                });
            }
        }
        if gone && !matches!(action, Action::Restart { .. }) {
            r.retire_endpoints();
        }
    }

    /// Watches the processes: deaths, start timeouts and restarts that are due.
    fn poll(&mut self) {
        let now = now_ms();
        for r in self.slots[..self.count].iter_mut().flatten() {
            let Some(h) = r.abandoned else { continue };
            if matches!(sys::process_wait(h, 0), Err(Error::Timeout)) {
                continue;
            }
            let _ = sys::handle_close(h);
            r.abandoned = None;
            sys::print(b"AriInit: service ");
            sys::print(r.name.as_bytes());
            sys::print(b": its abandoned process exited at last\n");
            r.retire_endpoints();
        }
        for i in 0..self.count {
            let (process, state) = match self.slots[i].as_ref() {
                Some(r) => (r.process, r.sup.state()),
                None => continue,
            };
            if let Some(h) = process {
                match sys::process_wait(h, 0) {
                    Err(Error::Timeout) => {
                        if let Some(r) = self.slots[i].as_mut() {
                            if r.sup.tick(now) == Action::Timeout {
                                self.start_timeout(i);
                            }
                        }
                    }
                    Ok(code) => self.announce_death(i, now, Exit::Code(code), true),
                    Err(Error::Killed) => self.announce_death(i, now, Exit::Killed, true),
                    Err(_) => self.announce_death(i, now, Exit::Code(u64::MAX), true),
                }
            } else if state == State::Stopped {
                let start = self.slots[i]
                    .as_mut()
                    .is_some_and(|r| r.sup.tick(now) == Action::Start);
                if start {
                    self.launch(i);
                }
            }
        }
    }

    /// Status of service `index` for `status` v1.
    fn status(&self, index: u8) -> status::Reply {
        let Some(r) = self.slots.get(index as usize).and_then(|s| s.as_ref()) else {
            return status::Reply::not_found();
        };
        let st = r.sup.status();
        let (last_exit, last_code) = match st.last_exit {
            None => (status::LastExit::None, 0),
            Some(Exit::Code(c)) => (status::LastExit::Code, c),
            Some(Exit::Killed) => (status::LastExit::Killed, 0),
            Some(Exit::StartTimeout) => (status::LastExit::StartTimeout, 0),
        };
        status::Reply {
            found: true,
            state: match st.state {
                State::Stopped => status::State::Stopped,
                State::Starting => status::State::Starting,
                State::Ready => status::State::Ready,
                State::Degraded => status::State::Degraded,
                State::Failed => status::State::Failed,
            },
            optional: r.sup.is_optional(),
            generation: st.generation,
            restarts: st.restarts,
            last_exit,
            last_code,
            ready_for_ms: st.ready_at.map_or(0, |t| now_ms().saturating_sub(t).max(1)),
            ..status::Reply::not_found()
        }
        .with_name(r.name)
    }
}

/// Service that never answers (`service_hang`): once the probe times out
/// the supervisor must kill it; with the budget exhausted no `hang_service`
/// process is left alive.
fn service_hang(sup: &mut Supervisor) -> Result<(), u64> {
    let ep = sys::endpoint_create().map_err(|_| 50u64)?;
    let policy = Policy {
        max_restarts: 1,
        backoff_initial_ms: 100,
        backoff_max_ms: 100,
        start_timeout_ms: 1000,
        stable_ms: 60_000,
    };
    let i = sup
        .register(
            "hang_service",
            Kind::Program { arg: 0 },
            Probe::Ping,
            ep,
            true,
            policy,
        )
        .ok_or(50u64)?;
    if sup.launch(i) {
        say(b"hang_service should never have become ready");
        return Err(51);
    }
    // Relaunches until the budget is exhausted.
    let t0 = now_ms();
    while sup.state(i) != State::Failed && now_ms().saturating_sub(t0) < 20_000 {
        sup.poll();
        sys::thread_sleep(50);
    }
    if sup.state(i) != State::Failed {
        say(b"hang_service did not reach failed");
        return Err(52);
    }
    // No hang_service process alive: the supervisor killed them.
    let mut buf = [0u8; sys::abi::sysinfo::ProcessEntry::LEN * 48];
    let n = sys::system_info(SYSTEM, sys::abi::sysinfo::PROCESSES, &mut buf).map_err(|_| 53u64)?;
    let alive = buf[..n]
        .as_chunks::<{ sys::abi::sysinfo::ProcessEntry::LEN }>()
        .0
        .iter()
        .filter_map(|c| sys::abi::sysinfo::ProcessEntry::decode(c))
        .filter(|e| {
            e.name_str() == "hang_service" && e.state == sys::abi::sysinfo::ProcessState::Running
        })
        .count();
    let mut b = [0u8; 20];
    sys::print(b"AriInit: hang_service: ");
    sys::print(aricode_rt::fmt_u64(alive as u64, &mut b));
    sys::print(b" process(es) alive after exhausting the budget\n");
    if alive != 0 {
        return Err(54);
    }
    Ok(())
}

/// Input driver that crashes holding a read (`input_restart`, ADR 0030):
/// its client must get `ReplyLost`, not `ServiceGone`, and once the
/// supervisor relaunches the driver on the same endpoint the device must
/// deliver events again (the case of console-bridge and a crashed
/// keyboard driver).
fn input_restart(sup: &mut Supervisor) -> Result<(), u64> {
    let ep = sys::endpoint_create().map_err(|_| 60u64)?;
    let policy = Policy {
        max_restarts: 3,
        backoff_initial_ms: 100,
        backoff_max_ms: 400,
        start_timeout_ms: 2000,
        stable_ms: 60_000,
    };
    let i = sup
        .register(
            "restart_input",
            Kind::Program { arg: 0 },
            Probe::Input,
            ep,
            true,
            policy,
        )
        .ok_or(60u64)?;
    if !sup.launch(i) {
        return Err(61);
    }
    match run_block_client_supervised("restart_input_client", ep, 0, sup) {
        Ok(0) => {
            say(b"input restart ok: the device survived its driver's crash");
            Ok(())
        }
        Ok(code) => {
            let mut b = [0u8; 20];
            sys::print(b"AriInit: restart_input_client exited with ");
            sys::print(aricode_rt::fmt_u64(code, &mut b));
            sys::print(b"\n");
            Err(62)
        }
        Err(e) => {
            aricode_rt::print_error(b"AriInit: restart_input_client", e);
            Err(62)
        }
    }
}

/// A reader that dies with its read waiting on each input service, before
/// the console bridge starts (`input_orphan`, audit 2026-10-25 C, M4): the
/// keys typed next must still reach the shell.
fn orphan_reads(inputs: &[Handle], sup: &mut Supervisor) {
    for ep in inputs {
        match run_block_client_supervised("input_orphan", *ep, 0, sup) {
            Ok(0) => {}
            _ => say(b"input_orphan failed"),
        }
    }
}

/// Induced restarts (`restart_storm`): a test service that dies after each
/// request is restarted 100 times under the supervisor while fsd keeps
/// serving; at the end free memory must match the initial amount.
fn restart_storm(sup: &mut Supervisor, fs: &mut FsClient) -> Result<(), u64> {
    let ep = sys::endpoint_create().map_err(|_| 40u64)?;
    let policy = Policy {
        max_restarts: 200,
        backoff_initial_ms: 1,
        backoff_max_ms: 8,
        start_timeout_ms: 2000,
        stable_ms: 60_000,
    };
    let i = sup
        .register(
            "crashy_service",
            Kind::Program { arg: 0 },
            Probe::Ping,
            ep,
            true,
            policy,
        )
        .ok_or(40u64)?;
    let mut mem = [0u8; 16];
    let free = |mem: &mut [u8; 16]| -> u64 {
        match sys::system_info(SYSTEM, sys::abi::sysinfo::MEMORY, mem) {
            Ok(16) => u64::from_le_bytes(mem[8..].try_into().unwrap()),
            _ => 0,
        }
    };
    // No test process alive yet: this way the final measurement is comparable.
    let free_before = free(&mut mem);
    let t0 = now_ms();
    if !sup.launch(i) {
        return Err(41);
    }
    let mut fs_ok = 0u32;
    let mut fs_checks = 0u32;
    let mut pings = 0u32;
    loop {
        sup.poll();
        let generation = sup.generation(i);
        if generation > 100 {
            break;
        }
        if now_ms().saturating_sub(t0) > 60_000 {
            return Err(42);
        }
        // "die": it answers and dies; the supervisor relaunches it. Before
        // requesting another death this one must have been observed: on SMP
        // the process may stay alive for a moment after answering, and a
        // second queued "die" would kill the next generation before its probe.
        if sup.state(i) == State::Ready && sup.has_process(i) {
            if ping_once(sup.endpoint(i), b"die") {
                pings += 1;
                if pings.is_multiple_of(10) {
                    fs_checks += 1;
                    let mut name = [0u8; 32];
                    if fs.list(0, &mut name).is_ok() {
                        fs_ok += 1;
                    }
                }
            }
            let seen = now_ms() + 2000;
            while sup.has_process(i) && now_ms() < seen {
                sup.poll();
            }
        }
    }
    // Last generation: one more ping kills it; it is reaped and the service
    // is retired so the supervisor does not relaunch it before measuring.
    let _ = ping_once(sup.endpoint(i), b"die");
    let deadline = now_ms() + 2000;
    while sup.has_process(i) && now_ms() < deadline {
        sup.poll();
    }
    sup.stop(i);
    let elapsed = now_ms().saturating_sub(t0);
    let free_after = free(&mut mem);
    let delta = free_before as i64 - free_after as i64;
    let mut b = [0u8; 20];
    let restarts = sup.generation(i).saturating_sub(1);
    sys::print(b"AriInit: supervisor: ");
    sys::print(aricode_rt::fmt_u64(restarts as u64, &mut b));
    sys::print(b" restarts of crashy_service in ");
    sys::print(aricode_rt::fmt_u64(elapsed, &mut b));
    sys::print(b" ms, fsd answered ");
    sys::print(aricode_rt::fmt_u64(fs_ok as u64, &mut b));
    sys::print(b"/");
    sys::print(aricode_rt::fmt_u64(fs_checks as u64, &mut b));
    sys::print(b", memory: ");
    if delta < 0 {
        sys::print(b"-");
    }
    sys::print(aricode_rt::fmt_u64(delta.unsigned_abs(), &mut b));
    sys::print(b" frames difference\n");
    if restarts != 100 || fs_checks == 0 || fs_ok != fs_checks {
        return Err(43);
    }
    if delta > 32 {
        say(b"supervisor: frames were lost across restarts");
        return Err(44);
    }
    Ok(())
}

/// The raw area that `blk_client` (`selftest` only) checks and writes: the
/// test disk has it at LBA 4096, over virtio-blk or USB, outside `ARI0`
/// since that partition went last (ADR 0072). The USB stick image has no
/// raw-area pattern.
const RAW_AREA_LBA: u64 = 4096;

/// Launches a client of a service with its endpoint (WRITE only) and a
/// hardware capability for its DMA buffer.
fn spawn_block_client(name: &str, service: Handle, arg: u64) -> Result<Handle, Error> {
    let mut sent = Granted::new();
    sent.push(sys::handle_duplicate(
        service,
        rights::WRITE | rights::TRANSFER,
    )?);
    sent.push(sys::handle_duplicate(
        HARDWARE,
        rights::MANAGE | rights::TRANSFER,
    )?);
    let child = sys::process_spawn_with(SPAWNER, name, arg, sent.handles())?;
    sent.take();
    Ok(child)
}

/// Launches a client with `spawn_block_client` and waits for it to exit,
/// polling the supervisor meanwhile (ADR 0030).
fn run_block_client_supervised(
    name: &str,
    service: Handle,
    arg: u64,
    sup: &mut Supervisor,
) -> Result<u64, Error> {
    let child = spawn_block_client(name, service, arg)?;
    let result = loop {
        match sys::process_wait(child, 100) {
            Err(Error::Timeout) => sup.poll(),
            other => break other,
        }
    };
    let _ = sys::handle_close(child);
    result
}

/// Launches the shell with its own fsd session (WRITE), hardware for its
/// buffer, the system object (READ|MANAGE), the console input, the PCI root
/// read-only, the netd endpoint (WRITE) and the launch broker for `run`;
/// `arg` = active services. Waits for it to exit, ending the desktop
/// session meanwhile if its launcher goes (ADR 0074).
fn run_shell(
    net_service: Handle,
    services: u64,
    broker: &mut Broker,
    sup: &mut Supervisor,
    mut sessions: Option<&mut Sessions>,
    desktop: &mut Option<Desktop>,
) -> Result<u64, Error> {
    let console_in = sys::handle_duplicate(CONSOLE_IN, rights::READ | rights::TRANSFER)?;
    let fs = broker.fs.as_ref().ok_or(Error::NotFound)?;
    let child = spawn_shell(fs, net_service, broker.endpoint, console_in, None, services)?;
    // Serve launch, status and session requests, and watch the resident
    // services, while the shell is alive.
    loop {
        match sys::process_wait(child, 0) {
            Err(Error::Timeout) => {}
            result => {
                let _ = sys::handle_close(child);
                return result;
            }
        }
        sup.poll();
        broker.serve_one(20, sup);
        broker.serve_apps();
        // The session ends with its launcher or with its compositor (audit
        // of phase B, M1): with the compositor gone and nothing done, the
        // input services would have no reader. The compositor is looked at
        // first, so the launches collected after it no longer ask it to
        // end their windows.
        let compositor_end = desktop
            .as_ref()
            .is_some_and(|d| !matches!(sys::process_wait(d.gui.process, 0), Err(Error::Timeout)));
        if compositor_end {
            broker.compositor = None;
        }
        let launcher_end = broker.poll_launches();
        let why = match (launcher_end, compositor_end) {
            (Some(r), _) => Some(DesktopEnd::Launcher(r)),
            (None, true) => Some(DesktopEnd::Compositor),
            (None, false) => None,
        };
        if let Some(why) = why {
            if let Some(d) = desktop.take() {
                end_desktop(d, why, broker, sup);
            }
        }
        if let (Some(s), Some(fs)) = (sessions.as_deref_mut(), broker.fs.as_ref()) {
            s.serve(fs, net_service, services, broker.endpoint);
        }
        if let Some(fs) = broker.fs.as_ref() {
            broker
                .terminals
                .serve(fs, net_service, services, broker.terminal_launches);
        }
    }
}

/// A file session for a process init launches (ADR 0036): fsd binds a new
/// endpoint to the main namespace and keeps its receive side, so the
/// descriptors the process leaves open are released when it dies. A
/// refusal is said on the console (an attach never fails silently).
fn attach_file_session(fs: &FsClient) -> Result<Handle, Error> {
    file_grant(fs.attach())
}

/// A file session fsd granted, or the refusal said on the console.
fn file_grant(granted: Result<Handle, FsError>) -> Result<Handle, Error> {
    match granted {
        Ok(h) => Ok(h),
        Err(FsError::Ipc(e)) => {
            aricode_rt::print_error(b"AriInit: file session", e);
            Err(e)
        }
        Err(FsError::Status(status)) => {
            sys::print(b"AriInit: file session refused by fsd (status ");
            let mut b = [0u8; 20];
            sys::print(aricode_rt::fmt_u64(status as u64, &mut b));
            sys::print(b")\n");
            Err(Error::Limit)
        }
    }
}

/// A network session for a process init launches (ADR 0037): netd binds a
/// new endpoint to the client called `label` and keeps its receive side, so
/// the sockets the process leaves open are closed when it dies. A refusal
/// is said on the console (an attach never fails silently).
fn attach_net_session(net_service: Handle, label: &str, port: u16) -> Result<Handle, Error> {
    use aricode_rt::sys::net::NetError;
    match aricode_rt::sys::net::attach(net_service, label, port) {
        Ok(h) => Ok(h),
        Err(NetError::Ipc(e)) => {
            aricode_rt::print_error(b"AriInit: network session", e);
            Err(e)
        }
        Err(NetError::Status(status)) => {
            sys::print(b"AriInit: network session refused by netd (status ");
            let mut b = [0u8; 20];
            sys::print(aricode_rt::fmt_u64(status as u64, &mut b));
            sys::print(b")\n");
            Err(Error::Limit)
        }
    }
}

/// Launches `arish` with a file session of its own (WRITE), hardware for
/// its buffer, the system (READ|MANAGE), its input (`input`, transferred:
/// the console or a session endpoint), the read-only PCI root, a network
/// session of its own (WRITE), the broker for `run`, and, in session mode,
/// the session output endpoint (`output`, transferred). `arg` = active
/// services plus the session bits.
fn spawn_shell(
    fs: &FsClient,
    net_service: Handle,
    broker: Handle,
    input: Handle,
    output: Option<Handle>,
    arg: u64,
) -> Result<Handle, Error> {
    // Every handle, `input` and `output` included, is closed by the guard
    // if anything below fails (the kernel takes them only when it creates
    // the process). The fsd and netd sessions come last: a failed duplicate
    // must not cost a session slot until the services notice the closed
    // handle (netd within a second; fsd keeps it, since init never exits).
    let mut granted = Granted::new();
    granted.push(input);
    if let Some(out) = output {
        granted.push(out);
    }
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    granted.push(hw);
    // With DUPLICATE the shell can in turn pass a read-only SYSTEM to the
    // programs it launches with `run`.
    let system = sys::handle_duplicate(
        SYSTEM,
        rights::READ | rights::MANAGE | rights::DUPLICATE | rights::TRANSFER,
    )?;
    granted.push(system);
    let pci = sys::handle_duplicate(PCI_ROOT, rights::READ | rights::TRANSFER)?;
    granted.push(pci);
    // The shell does not get the SPAWNER: it asks the broker (init) to launch.
    let launch = sys::handle_duplicate(broker, rights::WRITE | rights::TRANSFER)?;
    granted.push(launch);
    // Without a network service nobody would answer the attach, and a
    // duplicate of the main endpoint would let every `net` queue a request
    // (with its reply endpoint) that init never drains. The shell sees
    // that SERVICE_NET is off and does not use the handle; its slot is
    // positional, so it gets the send side of a pair whose receive side is
    // closed here: a placeholder that reaches nobody (a send fails at once
    // with `PeerClosed`).
    let netd = if arg & SERVICE_NET != 0 {
        attach_net_session(net_service, "shell", 0)?
    } else {
        let (dead, placeholder) = sys::endpoint_create_pair()?;
        let _ = sys::handle_close(dead);
        placeholder
    };
    granted.push(netd);
    let fsd = attach_file_session(fs)?;
    granted.push(fsd);
    let mut handles = [fsd, hw, system, input, pci, netd, launch, Handle(0)];
    let n = match output {
        Some(out) => {
            handles[7] = out;
            8
        }
        None => 7,
    };
    let child = sys::process_spawn_with(SPAWNER, "shell", arg, &handles[..n])?;
    granted.take();
    Ok(child)
}

/// Session shells for sshd (ADR 0025), or for the desktop's terminal (ADR
/// 0074 phase C): requests arrive on `endpoint`; finished shells are
/// reaped on every pass.
struct Sessions {
    endpoint: Handle,
    children: [Option<Handle>; MAX_SESSIONS],
    /// The terminal's: interactive shells only, never sshd's `Exec`.
    terminal: bool,
    /// A request without its three handles was said once already.
    said_malformed: bool,
}

const MAX_SESSIONS: usize = 4;

impl Sessions {
    fn serve(&mut self, fs: &FsClient, net: Handle, services: u64, broker: Handle) {
        use aricode_proto::session::{self, Kind as SessionKind};
        for c in self.children.iter_mut() {
            if let Some(h) = *c {
                if sys::process_wait(h, 0) != Err(Error::Timeout) {
                    let _ = sys::handle_close(h);
                    *c = None;
                }
            }
        }
        let mut msg = [0u8; session::Request::LEN];
        let mut handles = [Handle(0); 3];
        let Ok((n, nh)) = sys::ipc_receive(self.endpoint, &mut msg, &mut handles, 0) else {
            return;
        };
        if nh < 3 {
            for h in &handles[..nh] {
                let _ = sys::handle_close(*h);
            }
            if !core::mem::replace(&mut self.said_malformed, true) {
                say(b"a session request came without its handles; dropped (said once)");
            }
            return;
        }
        let [reply_to, input, output] = handles;
        let slot = self.children.iter().position(Option::is_none);
        let request = session::Request::decode(&msg[..n])
            .filter(|r| !self.terminal || r.kind == SessionKind::Shell);
        let spawned = match (request, slot) {
            (Some(req), Some(slot)) => {
                let quiet = if req.kind == SessionKind::Exec {
                    session::SHELL_QUIET
                } else {
                    0
                };
                let terminal = if self.terminal {
                    session::SHELL_TERMINAL
                } else {
                    0
                };
                let arg = services | session::SHELL_SESSION | quiet | terminal;
                // `spawn_shell` closes `input` and `output` if it fails.
                match spawn_shell(fs, net, broker, input, Some(output), arg) {
                    Ok(child) => {
                        self.children[slot] = Some(child);
                        true
                    }
                    Err(_) => false,
                }
            }
            _ => {
                let _ = sys::handle_close(input);
                let _ = sys::handle_close(output);
                false
            }
        };
        if !spawned {
            say(if self.terminal {
                b"terminal shell refused (limit or invalid request)"
            } else {
                b"session shell refused (limit or invalid request)"
            });
        }
        let _ = sys::ipc_send(reply_to, &session::reply(spawned), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

/// Capability broker (Phase 28): launches programs according to their
/// manifest. An application receives, in this order, only what it asks for
/// and is allowed: `SYSTEM` (read), a private directory in fsd, the network
/// endpoint, a compositor session, the block endpoint (trusted
/// applications only), the main file namespace (`installer`), the launch
/// endpoint (`launcher`, ADR 0074), the terminal endpoint (`terminal`), and
/// finally `HARDWARE` if it needs
/// buffers, bounded by the manifest's memory. The input services are never
/// granted (ADR 0053).
struct Broker {
    endpoint: Handle,
    /// Where `launch` holders ask (ADR 0074): an endpoint of its own, so a
    /// request that arrives here is known to come from the launcher, and
    /// only its narrower rule applies ([`Self::serve_apps`]).
    apps: Handle,
    fs: Option<FsClient>,
    block: Option<Handle>,
    /// The network service's main endpoint, when a driver serves it.
    net: Option<Handle>,
    compositor: Option<Handle>,
    /// Launched programs that have not exited yet (ADR 0015): process and,
    /// if someone waits for its exit, the endpoint to reply to. The shell's
    /// `run` and `spawn` take the first `MAX_RUNNING` slots, the desktop
    /// session the rest (`Pool`).
    running: [Option<Running>; MAX_RUNNING + DESKTOP_SLOTS],
    /// The installed applications the launcher may offer, as last listed.
    offered: Offered,
    /// Where `terminal` holders ask for session shells (ADR 0074 phase C).
    terminals: Sessions,
    /// Where those shells ask for launches: an endpoint of their own, so
    /// what they start is known and stopped with them when the desktop
    /// ends (audit of the terminal, M2).
    terminal_launches: Handle,
    /// DMA buffer of the package being launched (ADR 0017), the package's
    /// size: freed once the launch ends, since the kernel copies the
    /// program when it creates the process (ADR 0072, phase B).
    stage: Option<Stage>,
}

/// The largest package the broker loads: the limit every part of the
/// system shares (incident 0026).
const STAGE_LEN: u64 = aricode_pkg::PACKAGE_MAX as u64;
// The broker's buffer is one DMA buffer: the package limit must stay within
// what the kernel gives one (audit of ADR 0072 phase B, L2).
const _: () = assert!(STAGE_LEN <= sys::abi::DMA_MAX_LEN);

/// A DMA buffer mapped in init, unmapped and released on drop.
struct Stage {
    dma: Handle,
    virt: u64,
    len: u64,
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = sys::mmio_unmap(self.virt);
        let _ = sys::handle_close(self.dma);
    }
}

/// Where the launched program comes from.
#[derive(Clone, Copy)]
enum Source {
    Initrd,
    /// `apps/NAME.aripkg` in AriFS, already copied and verified in the
    /// buffer: ELF offset and length, package version.
    Installed {
        elf_offset: u32,
        elf_len: u32,
        version: u32,
        package_len: u32,
    },
}

/// Programs the shell may have running at once through the broker; the
/// next `run` or `spawn` gets `Limit`.
const MAX_RUNNING: usize = 4;

/// Applications the desktop's launcher may have running at once, apart
/// from the shell's (the owner, 2026-11-04, ADR 0074 phase C): with the
/// launcher, seven of the compositor's eight sessions, so a graphical
/// program started from the shell still finds one.
const MAX_LAUNCHED: usize = 6;

/// The desktop session's slots: the launcher and its applications.
const DESKTOP_SLOTS: usize = 1 + MAX_LAUNCHED;

/// Which slots a launch may take: neither the shell nor the desktop can
/// leave the other without one.
#[derive(Clone, Copy)]
enum Pool {
    /// The shell's `run` and `spawn`.
    Shell,
    /// The desktop session's launcher.
    Launcher,
    /// What the launcher starts.
    Apps,
}

/// Installed applications the launcher may offer at most.
const MAX_OFFERED: usize = 16;

/// The installed applications the launcher may offer (ADR 0074 phase C):
/// signed packages that verify, run on this system, open a window and ask
/// for nothing an application may not have.
struct Offered {
    names: [[u8; aricode_proto::launch::MAX_NAME]; MAX_OFFERED],
    lens: [u8; MAX_OFFERED],
    count: usize,
    /// When it was made (ms since boot), if it was.
    made_ms: Option<u64>,
}

/// A list younger than this is answered again as it is: a launcher asking
/// in a loop cannot keep init reading packages (audit of phase C, M1).
const OFFERED_FRESH_MS: u64 = 5_000;

impl Offered {
    const fn new() -> Self {
        Self {
            names: [[0; aricode_proto::launch::MAX_NAME]; MAX_OFFERED],
            lens: [0; MAX_OFFERED],
            count: 0,
            made_ms: None,
        }
    }

    fn push(&mut self, name: &str) -> bool {
        if self.count == MAX_OFFERED || name.len() > aricode_proto::launch::MAX_NAME {
            return false;
        }
        self.names[self.count][..name.len()].copy_from_slice(name.as_bytes());
        self.lens[self.count] = name.len() as u8;
        self.count += 1;
        true
    }

    fn get(&self, i: usize) -> Option<&str> {
        (i < self.count)
            .then(|| core::str::from_utf8(&self.names[i][..self.lens[i] as usize]).ok())
            .flatten()
    }
}

#[derive(Clone, Copy)]
struct Running {
    process: Handle,
    reply_to: Option<Handle>,
    /// Its compositor session, ended when it exits (ADR 0052).
    window: Option<u32>,
    /// The desktop session's launcher: its exit ends the session (ADR
    /// 0074).
    ends_session: bool,
    /// Started by the launcher: stopped when the session ends.
    from_launcher: bool,
    /// Started by a desktop terminal's shell: stopped with it.
    from_terminal: bool,
}

/// Immediate result of `launch`: an already decided reply, or a created
/// process (and its compositor session) whose exit must be awaited in a
/// slot.
enum Launched {
    Reply(LaunchReply),
    Process(Handle, Option<u32>),
}

/// A compositor session made for a launch: ended on drop unless `keep`
/// took it, so a launch refused after `Attach` frees its slot at once.
struct WindowSession {
    compositor: Handle,
    id: Option<u32>,
}

impl WindowSession {
    fn keep(&mut self) -> Option<u32> {
        self.id.take()
    }
}

impl Drop for WindowSession {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            let _ = sys::window::detach(self.compositor, id);
        }
    }
}

impl Broker {
    /// `fs`: the file client of the storage health check, reused; it asks
    /// fsd for the sessions of the programs the broker launches.
    fn new(
        fs: Option<FsClient>,
        block: Option<Handle>,
        net: Option<Handle>,
        compositor: Option<Handle>,
    ) -> Result<Self, Error> {
        Ok(Self {
            endpoint: sys::endpoint_create()?,
            apps: sys::endpoint_create()?,
            fs,
            block,
            net,
            compositor,
            running: [None; MAX_RUNNING + DESKTOP_SLOTS],
            offered: Offered::new(),
            terminals: Sessions {
                endpoint: sys::endpoint_create()?,
                children: [None; MAX_SESSIONS],
                terminal: true,
                said_malformed: false,
            },
            terminal_launches: sys::endpoint_create()?,
            stage: None,
        })
    }

    /// A free slot of `pool`.
    fn free_slot(&self, pool: Pool) -> Option<usize> {
        let range = match pool {
            Pool::Shell => 0..MAX_RUNNING,
            Pool::Launcher => MAX_RUNNING..MAX_RUNNING + 1,
            Pool::Apps => MAX_RUNNING + 1..MAX_RUNNING + DESKTOP_SLOTS,
        };
        range.into_iter().find(|&i| self.running[i].is_none())
    }

    /// Lists again the installed applications the launcher may offer:
    /// `apps/NAME.aripkg` for each, never the launcher itself or a name an
    /// initrd program has, and only those `offers` accepts. Says each one
    /// left out and why, as a launch would.
    fn refresh_offered(&mut self) {
        let now = now_ms();
        if self
            .offered
            .made_ms
            .is_some_and(|t| now.saturating_sub(t) < OFFERED_FRESH_MS)
        {
            return;
        }
        self.offered = Offered::new();
        self.offered.made_ms = Some(now);
        let Some(fs) = self.fs.as_mut() else {
            return;
        };
        let mut entry = [0u8; 32];
        let mut i = 0;
        while let Ok(Some((n, size))) = fs.list(i, &mut entry) {
            i += 1;
            let Some(name) = core::str::from_utf8(&entry[..n])
                .ok()
                .and_then(|e| e.strip_prefix("apps/"))
                .and_then(|e| e.strip_suffix(aricode_pkg::EXTENSION))
            else {
                continue;
            };
            if name.is_empty() || Caps::TRUSTED_LAUNCH.contains(&name) || in_initrd(name) {
                continue;
            }
            let mut copy = [0u8; 32];
            copy[..name.len()].copy_from_slice(name.as_bytes());
            let name = core::str::from_utf8(&copy[..name.len()]).unwrap_or("");
            if offers(fs, name, size) && !self.offered.push(name) {
                sys::print(b"AriInit: the launcher is offered the first 16 applications only\n");
                break;
            }
        }
    }

    /// Installed package (`apps/NAME.aripkg`): copies it to the staging
    /// buffer, verifies it and returns its application manifest and where
    /// the ELF lies. A package that fails verification is denied.
    fn load_installed(&mut self, name: &str) -> Result<(Manifest, Source), LaunchStatus> {
        let fs = self.fs.as_mut().ok_or(LaunchStatus::NotFound)?;
        let mut path = [0u8; 48];
        let Some(pkg_path) = join("apps/", name, &mut path) else {
            return Err(LaunchStatus::Invalid);
        };
        let mut full = [0u8; 48];
        let Some(pkg_path) = join(pkg_path, aricode_pkg::EXTENSION, &mut full) else {
            return Err(LaunchStatus::Invalid);
        };
        // Size from the listing of the main namespace.
        let mut size = None;
        let mut entry = [0u8; 32];
        let mut i = 0;
        while let Ok(Some((n, s))) = fs.list(i, &mut entry) {
            if &entry[..n] == pkg_path.as_bytes() {
                size = Some(s);
                break;
            }
            i += 1;
        }
        let Some(size) = size else {
            return Err(LaunchStatus::NotFound);
        };
        if size == 0 || size > STAGE_LEN {
            let mut b = [0u8; 20];
            sys::print(b"AriInit: refused, package ");
            sys::print(pkg_path.as_bytes());
            sys::print(b" is ");
            sys::print(aricode_rt::fmt_u64(size, &mut b));
            sys::print(b" bytes; packages hold 1 to ");
            sys::print(aricode_rt::fmt_u64(STAGE_LEN, &mut b));
            sys::print(b" (the system's package limit)\n");
            return Err(LaunchStatus::Failed);
        }
        let len = size.div_ceil(4096) * 4096;
        let Ok(dma) = sys::dma_alloc(HARDWARE, len) else {
            let mut b = [0u8; 20];
            sys::print(b"AriInit: no memory for package ");
            sys::print(pkg_path.as_bytes());
            sys::print(b" (");
            sys::print(aricode_rt::fmt_u64(len, &mut b));
            sys::print(b" bytes of contiguous DMA)\n");
            return Err(LaunchStatus::Failed);
        };
        let virt = match sys::mmio_map(dma) {
            Ok(virt) => virt,
            Err(_) => {
                let _ = sys::handle_close(dma);
                return Err(LaunchStatus::Failed);
            }
        };
        let fresh = Stage { dma, virt, len };
        // SAFETY: our own DMA buffer of `len` bytes, mapped RW in init,
        // alive while `self.stage` holds it.
        let stage =
            unsafe { core::slice::from_raw_parts_mut(fresh.virt as *mut u8, fresh.len as usize) };
        self.stage = Some(fresh);
        let fd = fs.open(pkg_path).map_err(|_| LaunchStatus::NotFound)?;
        let total = size as usize;
        let mut done = 0usize;
        while done < total {
            let want = (total - done).min(4096);
            match fs.read(fd, done as u32, &mut stage[done..done + want]) {
                Ok(n) if n > 0 => done += n,
                _ => {
                    let _ = fs.close(fd);
                    return Err(LaunchStatus::Failed);
                }
            }
        }
        let _ = fs.close(fd);
        let pubkey = aricode_crypto::trust::public_key();
        let package = &stage[..total];
        let verified = match aricode_pkg::verify(package, &pubkey) {
            Ok(v) => v,
            Err(_) => {
                sys::print(b"AriInit: denied, package ");
                sys::print(pkg_path.as_bytes());
                sys::print(b" fails verification (signature or hashes)\n");
                return Err(LaunchStatus::Denied);
            }
        };
        // ADR 0074: a package for a newer application interface never runs
        // here, whatever brought it (a rollback to an older system, a copy
        // made by hand).
        if !verified.manifest.runs_on(aricode_ab::APPS_LEVEL) {
            let mut b = [0u8; 20];
            sys::print(b"AriInit: denied, package ");
            sys::print(pkg_path.as_bytes());
            sys::print(b" needs application interface ");
            sys::print(aricode_rt::fmt_u64(
                verified.manifest.requires as u64,
                &mut b,
            ));
            sys::print(b" and this system offers ");
            sys::print(aricode_rt::fmt_u64(aricode_ab::APPS_LEVEL as u64, &mut b));
            sys::print(b"\n");
            return Err(LaunchStatus::Denied);
        }
        let manifest = core::str::from_utf8(verified.app_manifest)
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
            .ok_or(LaunchStatus::BadManifest)?;
        if verified.manifest.name_str() != name || manifest.name_str() != name {
            return Err(LaunchStatus::BadManifest);
        }
        Ok((
            manifest,
            Source::Installed {
                elf_offset: verified.program_offset(package) as u32,
                elf_len: verified.program.len() as u32,
                version: verified.manifest.version,
                package_len: total as u32,
            },
        ))
    }

    /// Replies for the programs that have exited and frees their slots.
    /// Returns how the desktop session's launcher ended, if it did.
    fn poll_launches(&mut self) -> Option<LaunchReply> {
        let mut session_end = None;
        for slot in self.running.iter_mut() {
            let Some(r) = *slot else { continue };
            let reply = match sys::process_wait(r.process, 0) {
                Err(Error::Timeout) => continue,
                Ok(code) => LaunchReply::exited(code),
                Err(Error::Killed) => LaunchReply::status(LaunchStatus::Killed),
                Err(_) => LaunchReply::status(LaunchStatus::Failed),
            };
            let _ = sys::handle_close(r.process);
            if let (Some(comp), Some(id)) = (self.compositor, r.window) {
                // Whoever still holds its session is cut off with it.
                let _ = sys::window::detach(comp, id);
            }
            if let Some(reply_to) = r.reply_to {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            if r.ends_session {
                session_end = Some(reply);
            }
            *slot = None;
        }
        session_end
    }

    /// Stops the desktop session's launcher, every app it started and the
    /// terminals' shells with what they ran (ADR 0074): their windows are
    /// gone with the compositor. Their exits are collected by
    /// `poll_launches` as usual.
    fn stop_desktop_apps(&mut self) {
        for r in self.running.iter().flatten() {
            if r.ends_session || r.from_launcher || r.from_terminal {
                let _ = sys::process_kill(r.process);
            }
        }
        // The terminals' shells go with the terminals, even one waiting in
        // `run` for a program that never ends (audit of the terminal, M2);
        // they are reaped by the next `Sessions::serve`.
        for shell in self.terminals.children.iter().flatten() {
            let _ = sys::process_kill(*shell);
        }
    }

    /// Starts the desktop session's launcher in the background with its
    /// own manifest (ADR 0074); its exit ends the session
    /// (`poll_launches`). `false`, said, when it does not start.
    fn launch_session(&mut self) -> bool {
        let Some(slot) = self.free_slot(Pool::Launcher) else {
            say(b"the desktop's launcher did not start: running program limit reached");
            return false;
        };
        match self.launch(session::LAUNCHER, 0, None, false) {
            Launched::Reply(r) => {
                let mut b = [0u8; 20];
                sys::print(b"AriInit: the desktop's launcher did not start (launch status ");
                sys::print(aricode_rt::fmt_u64(r.status as u64, &mut b));
                sys::print(b")\n");
                false
            }
            Launched::Process(process, window) => {
                self.running[slot] = Some(Running {
                    process,
                    reply_to: None,
                    window,
                    ends_session: true,
                    from_launcher: false,
                    from_terminal: false,
                });
                true
            }
        }
    }

    /// Serves at most one request (waits up to `timeout_ms`).
    fn serve_one(&mut self, timeout_ms: u64, sup: &Supervisor) {
        let mut msg = [0u8; LaunchRequest::LEN];
        let mut handles = [Handle(0); sys::abi::IPC_MAX_HANDLES];
        let Ok((which, n, nh)) = sys::ipc_receive_any(
            &[self.endpoint, self.terminal_launches],
            &mut msg,
            &mut handles,
            timeout_ms,
        ) else {
            return;
        };
        let from_terminal = which == 1;
        // A reply endpoint and, for a launch with a console, the console.
        for h in handles.iter().take(nh).skip(2) {
            let _ = sys::handle_close(*h);
        }
        let nh = nh.min(2);
        if nh == 0 {
            return;
        }
        let reply_to = handles[0];
        // A launch's console (ADR 0047); closed on every refusal below.
        let console = (nh == 2).then_some(handles[1]);
        let refuse = |status| {
            if let Some(c) = console {
                let _ = sys::handle_close(c);
            }
            let reply = LaunchReply::status(status);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
        };
        // The same endpoint serves two protocols: launches (6) and service
        // status (7); the first byte tells them apart.
        if let Some(req) = status::Request::decode(&msg[..n]) {
            if let Some(c) = console {
                let _ = sys::handle_close(c);
            }
            let reply = sup.status(req.index);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        }
        let Some(req) = LaunchRequest::decode(&msg[..n]) else {
            return refuse(LaunchStatus::Invalid);
        };
        // A console comes only with the flag that announces it, and never
        // for a detached program, which may outlive the session. A list is
        // the launcher's, and unknown flags are nobody's.
        if req.has_console() != console.is_some()
            || (req.has_console() && req.is_detached())
            || req.flags & !(LaunchRequest::DETACH | LaunchRequest::CONSOLE) != 0
        {
            return refuse(LaunchStatus::Invalid);
        }
        // The slot is reserved before launching: no slot, no process.
        let Some(slot) = self.free_slot(Pool::Shell) else {
            sys::print(b"AriInit: ");
            sys::print(req.name_str().as_bytes());
            sys::print(b": running program limit reached\n");
            return refuse(LaunchStatus::Limit);
        };
        match self.launch(req.name_str(), req.arg, console, false) {
            Launched::Reply(reply) => {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            Launched::Process(process, window) if req.is_detached() => {
                let reply = LaunchReply::status(LaunchStatus::Started);
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
                self.running[slot] = Some(Running {
                    process,
                    reply_to: None,
                    window,
                    ends_session: false,
                    from_launcher: false,
                    from_terminal,
                });
            }
            Launched::Process(process, window) => {
                self.running[slot] = Some(Running {
                    process,
                    reply_to: Some(reply_to),
                    window,
                    ends_session: false,
                    from_launcher: false,
                    from_terminal,
                });
            }
        }
    }

    /// Serves at most one request from a `launch` holder, without waiting
    /// (ADR 0074). The launcher starts installed applications in the
    /// background, each with its own manifest, and nothing more: no initrd
    /// program (the installer, the updater, the shell), not itself, no
    /// console and no wait for an exit; service status is not served here.
    fn serve_apps(&mut self) {
        let mut msg = [0u8; LaunchRequest::LEN];
        let mut handles = [Handle(0); 2];
        let Ok((n, nh)) = sys::ipc_receive(self.apps, &mut msg, &mut handles, 0) else {
            return;
        };
        if nh == 0 {
            return;
        }
        let reply_to = handles[0];
        if nh == 2 {
            let _ = sys::handle_close(handles[1]);
        }
        let refuse = |status| reply(reply_to, LaunchReply::status(status));
        let Some(req) = LaunchRequest::decode(&msg[..n]) else {
            sys::print(b"AriInit: refused, the launcher sent something other than a launch\n");
            return refuse(LaunchStatus::Invalid);
        };
        if req.is_list() {
            if nh == 2 || req.flags != LaunchRequest::LIST {
                let _ = sys::ipc_send(
                    reply_to,
                    &Entry::status(LaunchStatus::Invalid).encode(),
                    &[],
                    0,
                );
                let _ = sys::handle_close(reply_to);
                return;
            }
            // The list is made again at its start (at most every 5 s), so
            // a launcher that reads it from 0 sees what is installed now.
            if req.arg == 0 {
                self.refresh_offered();
            }
            let entry = usize::try_from(req.arg)
                .ok()
                .and_then(|i| self.offered.get(i))
                .and_then(Entry::named)
                .unwrap_or(Entry::status(LaunchStatus::NotFound));
            let _ = sys::ipc_send(reply_to, &entry.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        }
        let name = req.name_str();
        if !req.is_detached() || req.has_console() || nh == 2 || req.flags != LaunchRequest::DETACH
        {
            sys::print(b"AriInit: refused, the launcher asked to start ");
            sys::print(name.as_bytes());
            sys::print(b" other than in the background without a console\n");
            return refuse(LaunchStatus::Invalid);
        }
        if Caps::TRUSTED_LAUNCH.contains(&name) {
            sys::print(b"AriInit: refused, the launcher asked to start itself\n");
            return refuse(LaunchStatus::Denied);
        }
        if in_initrd(name) {
            sys::print(
                b"AriInit: refused, the launcher may start installed applications only, and ",
            );
            sys::print(name.as_bytes());
            sys::print(b" is a system program\n");
            return refuse(LaunchStatus::Denied);
        }
        let Some(slot) = self.free_slot(Pool::Apps) else {
            sys::print(b"AriInit: ");
            sys::print(name.as_bytes());
            sys::print(b": the desktop's 6 application slots are full\n");
            return refuse(LaunchStatus::Limit);
        };
        sys::print(b"AriInit: the launcher asks to start ");
        sys::print(name.as_bytes());
        sys::print(b"\n");
        match self.launch(name, req.arg, None, true) {
            Launched::Reply(r) => reply(reply_to, r),
            Launched::Process(process, window) => {
                reply(reply_to, LaunchReply::status(LaunchStatus::Started));
                self.running[slot] = Some(Running {
                    process,
                    reply_to: None,
                    window,
                    ends_session: false,
                    from_launcher: true,
                    from_terminal: false,
                });
            }
        }
    }

    /// Reads the manifest (`NAME.manifest` in the initrd, or the one in the
    /// package `apps/NAME.aripkg` in AriFS if the program is installed,
    /// ADR 0017), applies the policy and launches the program; its exit is
    /// collected by `poll_launches`. `console`, if any, becomes the
    /// program's console (ADR 0047) and is closed if the launch fails.
    /// [`Self::launch_staged`], then the package's buffer goes, whatever the
    /// outcome.
    /// Launches `name`; `for_launcher` when the desktop's launcher asked,
    /// which may start only applications that open a window.
    fn launch(
        &mut self,
        name: &str,
        arg: u64,
        console: Option<Handle>,
        for_launcher: bool,
    ) -> Launched {
        let launched = self.launch_staged(name, arg, console, for_launcher);
        self.stage = None;
        launched
    }

    fn launch_staged(
        &mut self,
        name: &str,
        arg: u64,
        console: Option<Handle>,
        for_launcher: bool,
    ) -> Launched {
        // The prepared duplicates close themselves if any error path returns
        // before transferring them (`Granted::take` hands them to the
        // kernel). Previously, a denial after granting SYSTEM leaked one table
        // entry per attempt. The console goes first: the kernel takes it as
        // the first handle.
        let mut granted = Granted::new();
        if let Some(c) = console {
            granted.push(c);
        }
        let mut window: Option<WindowSession> = None;
        let mut path = [0u8; 48];
        let Some(path) = join(name, ".manifest", &mut path) else {
            return Launched::Reply(LaunchReply::status(LaunchStatus::Invalid));
        };
        let in_initrd = in_initrd(name);
        let mut text = [0u8; 1024];
        let (manifest, source) = if in_initrd {
            let manifest = match sys::initrd_read(SPAWNER, path, &mut text) {
                Ok(n) => match core::str::from_utf8(&text[..n])
                    .ok()
                    .and_then(|t| Manifest::parse(t).ok())
                {
                    Some(m) => Some(m),
                    None => return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest)),
                },
                Err(Error::NotFound) => None,
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest)),
            };
            (manifest, Source::Initrd)
        } else {
            match self.load_installed(name) {
                Ok((m, source)) => (Some(m), source),
                Err(status) => return Launched::Reply(LaunchReply::status(status)),
            }
        };
        let (caps, memory_kib) = match manifest {
            Some(m) if m.name_str() != name => {
                return Launched::Reply(LaunchReply::status(LaunchStatus::BadManifest))
            }
            Some(m) => (m.caps, m.memory_kib),
            // No manifest: console and clock only, minimal memory.
            None => (Caps::SYSTEM, 64),
        };
        let read_files = manifest.as_ref().map(Manifest::read_files);
        let denied = caps.difference(Caps::allowed_for(name));
        sys::print(b"AriInit: launch ");
        sys::print(name.as_bytes());
        if let Source::Installed {
            version,
            package_len,
            ..
        } = source
        {
            let mut b = [0u8; 20];
            sys::print(b" from AriFS (apps/");
            sys::print(name.as_bytes());
            sys::print(aricode_pkg::EXTENSION.as_bytes());
            sys::print(b" version ");
            sys::print(aricode_rt::fmt_u64(version as u64, &mut b));
            sys::print(b", ");
            sys::print(aricode_rt::fmt_u64(package_len as u64, &mut b));
            sys::print(b" bytes)");
        }
        sys::print(if manifest.is_some() {
            b" with manifest ["
        } else {
            b" without manifest ["
        });
        let mut first = true;
        for c in caps.names() {
            if !first {
                sys::print(b" ");
            }
            first = false;
            sys::print(c.as_bytes());
        }
        sys::print(b"] memory ");
        let mut buf = [0u8; 20];
        sys::print(aricode_rt::fmt_u64(memory_kib as u64, &mut buf));
        sys::print(b" KiB");
        if let Some(files) = read_files.clone().filter(|f| f.clone().count() > 0) {
            sys::print(b" read-only [");
            for (i, f) in files.enumerate() {
                if i > 0 {
                    sys::print(b" ");
                }
                sys::print(f.as_bytes());
            }
            sys::print(b"]");
        }
        sys::print(b"\n");
        if for_launcher && !caps.contains(Caps::WINDOW) {
            sys::print(
                b"AriInit: denied, the launcher starts applications with a window only, and ",
            );
            sys::print(name.as_bytes());
            sys::print(b" opens none\n");
            return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
        }
        if !denied.is_empty() {
            sys::print(b"AriInit: denied, requests system capabilities:");
            for c in denied.names() {
                sys::print(b" ");
                sys::print(c.as_bytes());
            }
            sys::print(b"\n");
            return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
        }
        // `launch` and `terminal` go only to an installed package, which
        // verified against the release key: never to an initrd program of
        // that name (ADR 0074).
        for (cap, what) in [
            (Caps::LAUNCH, &b"launch"[..]),
            (Caps::TERMINAL, b"terminal"),
        ] {
            if caps.contains(cap) && !matches!(source, Source::Installed { .. }) {
                sys::print(b"AriInit: denied, ");
                sys::print(what);
                sys::print(b" is granted only to an installed, signed ");
                sys::print(name.as_bytes());
                sys::print(b"\n");
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            }
        }
        let mut needs_hw = false;
        if caps.contains(Caps::SYSTEM) {
            match sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        if caps.contains(Caps::FS) {
            let Some(fs) = &self.fs else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match fs.bind(name) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        // Read-only shared files (ADR 0016): fsd binds an endpoint to each
        // one; a missing one denies the launch.
        for file in read_files.into_iter().flatten() {
            let Some(fs) = &self.fs else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match fs.share(file) {
                Ok(h) => granted.push(h),
                Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                    sys::print(b"AriInit: denied, shared file does not exist: ");
                    sys::print(file.as_bytes());
                    sys::print(b"\n");
                    return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
                }
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        // A network session of its own (ADR 0037): the sockets the program
        // leaves open die with it. Without a network service there is
        // nobody to ask, and the launch is denied like any absent service.
        if caps.contains(Caps::NET) {
            let Some(net) = self.net else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match attach_net_session(net, name, 0) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::WINDOW) {
            let Some(comp) = self.compositor else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            // A session of its own (ADR 0052): its windows are its alone and
            // close when it is gone. Same rights as before: send only.
            let session = match sys::window::attach(comp) {
                Ok((h, id)) => {
                    window = Some(WindowSession {
                        compositor: comp,
                        id: Some(id),
                    });
                    h
                }
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            };
            let narrowed = sys::handle_duplicate(session, rights::WRITE | rights::TRANSFER);
            let _ = sys::handle_close(session);
            match narrowed {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::BLOCK) {
            let Some(block) = self.block else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match sys::handle_duplicate(block, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        // The main namespace (ADR 0018) as a session of its own (ADR 0036):
        // the installer's descriptors die with it.
        if caps.contains(Caps::INSTALL) {
            let Some(fs) = &self.fs else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match attach_file_session(fs) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        // Send only: the launcher asks, the broker decides (`serve_apps`).
        if caps.contains(Caps::LAUNCH) {
            match sys::handle_duplicate(self.apps, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        if caps.contains(Caps::TERMINAL) {
            match sys::handle_duplicate(self.terminals.endpoint, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        if needs_hw {
            match sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        if granted.overflowed {
            sys::print(b"AriInit: denied, ");
            sys::print(name.as_bytes());
            sys::print(b" would need more handles than a launch carries\n");
            return Launched::Reply(LaunchReply::status(LaunchStatus::Failed));
        }
        // The kernel only takes the handles if it gets to create the process;
        // on any failure the guard closes them on exit (closing one already
        // taken returns BadHandle, which is ignored).
        let spawned = match source {
            Source::Initrd => sys::process_spawn_limited(
                SPAWNER,
                name,
                arg,
                granted.handles(),
                memory_kib,
                console.is_some(),
            ),
            Source::Installed {
                elf_offset,
                elf_len,
                ..
            } => {
                let Some(stage) = self.stage.as_ref() else {
                    return Launched::Reply(LaunchReply::status(LaunchStatus::Failed));
                };
                sys::process_spawn_from(
                    SPAWNER,
                    stage.dma,
                    elf_offset,
                    elf_len,
                    name,
                    arg,
                    granted.handles(),
                    memory_kib,
                    console.is_some(),
                )
            }
        };
        let child = match spawned {
            Ok(c) => {
                granted.take();
                c
            }
            Err(e) => {
                return Launched::Reply(LaunchReply::status(match e {
                    Error::NotFound => LaunchStatus::NotFound,
                    _ => LaunchStatus::Failed,
                }));
            }
        };
        Launched::Process(child, window.as_mut().and_then(WindowSession::keep))
    }
}

/// Handles prepared for a launch: they are closed on drop unless `take` has
/// handed them to the kernel. As many as a spawn transfers; one more is
/// closed and marks the list `overflowed`, which refuses the launch
/// instead of starting the program without it (audit of ADR 0047, L3).
struct Granted {
    handles: [Handle; sys::abi::IPC_MAX_HANDLES * 4],
    count: usize,
    overflowed: bool,
    transferred: bool,
}

impl Granted {
    fn new() -> Self {
        Self {
            handles: [Handle(0); sys::abi::IPC_MAX_HANDLES * 4],
            count: 0,
            overflowed: false,
            transferred: false,
        }
    }
    fn push(&mut self, h: Handle) {
        if self.count < self.handles.len() {
            self.handles[self.count] = h;
            self.count += 1;
        } else {
            self.overflowed = true;
            let _ = sys::handle_close(h);
        }
    }
    fn handles(&self) -> &[Handle] {
        &self.handles[..self.count]
    }
    fn take(&mut self) {
        self.transferred = true;
    }
}

impl Drop for Granted {
    fn drop(&mut self) {
        if !self.transferred {
            for h in &self.handles[..self.count] {
                let _ = sys::handle_close(*h);
            }
        }
    }
}

/// Whether `name` is an initrd program. One byte is enough to know: TooBig
/// if it exists, NotFound otherwise.
fn in_initrd(name: &str) -> bool {
    let mut probe = [0u8; 1];
    !matches!(
        sys::initrd_read(SPAWNER, name, &mut probe),
        Err(Error::NotFound)
    )
}

/// Whether the launcher may offer the installed package `apps/NAME.aripkg`
/// of `size` bytes: its signed manifest and its application manifest verify
/// (`inspect_streaming`: a few KiB per package; the program is not hashed,
/// the launch verifies the whole package), it runs on this system, is
/// `name` in both manifests, opens a window and asks for nothing an
/// application may not have. Says why when it does not.
fn offers(fs: &mut FsClient, name: &str, size: u64) -> bool {
    let left_out = |why: &[u8]| {
        sys::print(b"AriInit: the launcher is not offered ");
        sys::print(name.as_bytes());
        sys::print(b": ");
        sys::print(why);
        sys::print(b"\n");
        false
    };
    let mut path = [0u8; 48];
    let mut full = [0u8; 48];
    let Some(path) =
        join("apps/", name, &mut path).and_then(|p| join(p, aricode_pkg::EXTENSION, &mut full))
    else {
        return false;
    };
    if size == 0 || size > STAGE_LEN {
        return left_out(b"its package size is out of range");
    }
    let Ok(fd) = fs.open(path) else {
        return left_out(b"its package cannot be opened");
    };
    let mut read = |offset: usize, buf: &mut [u8]| -> Result<(), ()> {
        let mut done = 0;
        while done < buf.len() {
            let want = (buf.len() - done).min(4096);
            match fs.read(fd, (offset + done) as u32, &mut buf[done..done + want]) {
                Ok(n) if n > 0 => done += n,
                _ => return Err(()),
            }
        }
        Ok(())
    };
    let pubkey = aricode_crypto::trust::public_key();
    let mut text = [0u8; 1024];
    let verdict = match aricode_pkg::inspect_streaming(size as usize, &mut read, &pubkey, &mut text)
    {
        Err(_) => Err(&b"it fails verification (signature or manifest hash)"[..]),
        Ok((m, _)) if !m.runs_on(aricode_ab::APPS_LEVEL) => {
            Err(&b"it needs a newer application interface"[..])
        }
        Ok((m, _)) if m.name_str() != name => Err(&b"its package is named otherwise"[..]),
        Ok((_, len)) => match core::str::from_utf8(&text[..len])
            .ok()
            .and_then(|t| Manifest::parse(t).ok())
        {
            None => Err(&b"its manifest cannot be read"[..]),
            Some(m) if m.name_str() != name => Err(&b"its manifest is named otherwise"[..]),
            Some(m) if !m.caps.contains(Caps::WINDOW) => Err(&b"it opens no window"[..]),
            Some(m) if !m.caps.difference(Caps::allowed_for(name)).is_empty() => {
                Err(&b"it asks for system capabilities"[..])
            }
            Some(_) => Ok(()),
        },
    };
    let _ = fs.close(fd);
    match verdict {
        Ok(()) => true,
        Err(why) => left_out(why),
    }
}

/// Sends a launch reply and gives up the reply endpoint.
fn reply(reply_to: Handle, reply: LaunchReply) {
    let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
    let _ = sys::handle_close(reply_to);
}

/// `a` + `b` into `buf`.
fn join<'a>(a: &str, b: &str, buf: &'a mut [u8; 48]) -> Option<&'a str> {
    let total = a.len() + b.len();
    if total > buf.len() {
        return None;
    }
    buf[..a.len()].copy_from_slice(a.as_bytes());
    buf[a.len()..total].copy_from_slice(b.as_bytes());
    core::str::from_utf8(&buf[..total]).ok()
}

/// Launches the compositor (own endpoint, framebuffer, SYSTEM, hardware for
/// its back buffer and the input services) and returns its endpoint. The
/// applications get their sessions from the broker (ADR 0052); the
/// graphical demos are a test package since ADR 0074.
fn run_gui(inputs: &[Handle]) -> Result<Gui, u64> {
    // The compositor gets the receive side and init keeps only the send
    // side (ADR 0030): once the compositor is gone, init's requests
    // (`window::attach`/`detach`) answer `PeerClosed` at once instead of
    // waiting on a queue nobody reads (found with the desktop session's
    // Ctrl+Alt+Backspace, ADR 0074 phase B).
    let (receive, service) = sys::endpoint_create_pair().map_err(|_| 1u64)?;
    let mut handles = [Handle(0); 4 + MAX_GUI_INPUTS];
    handles[0] = receive;
    let mut made = 1;
    let spawned = (|| {
        let mut grant = |h: Result<Handle, Error>, code: u64| -> Result<(), u64> {
            handles[made] = h.map_err(|_| code)?;
            made += 1;
            Ok(())
        };
        grant(
            sys::handle_duplicate(FRAMEBUFFER, rights::READ | rights::WRITE | rights::TRANSFER),
            3,
        )?;
        grant(
            sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER),
            4,
        )?;
        grant(
            sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER),
            5,
        )?;
        for ep in inputs.iter().take(MAX_GUI_INPUTS) {
            grant(
                sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER),
                6,
            )?;
        }
        let count = inputs.len().min(MAX_GUI_INPUTS) as u64;
        sys::process_spawn_with(SPAWNER, "compositor", count, &handles[..made]).map_err(|_| 7u64)
    })();
    match spawned {
        Ok(process) => Ok(Gui {
            endpoint: service,
            process,
        }),
        // The kernel takes the handles only when it creates the process.
        Err(code) => {
            for h in &handles[..made] {
                let _ = sys::handle_close(*h);
            }
            let _ = sys::handle_close(service);
            Err(code)
        }
    }
}

/// The compositor init started: its service endpoint and its process.
struct Gui {
    endpoint: Handle,
    process: Handle,
}

/// The desktop session while it runs (ADR 0074): the compositor, and the
/// input services it took, which go back to the console bridge when the
/// session ends.
struct Desktop {
    gui: Gui,
    inputs: [Handle; MAX_INPUTS],
    count: usize,
}

/// Whether to boot into the desktop session (ADR 0074): `session` asks for
/// it, the machine follows the `desktop` profile, its launcher is
/// installed, and there are a display and a keyboard the compositor can
/// read. Says why when the file asks for the desktop and
/// it cannot start; never boots into a desktop that is not there.
fn desktop_wanted(fs: &mut FsClient, inputs: &[Handle]) -> bool {
    let mut text = [0u8; 256];
    let chosen = match read_small(fs, session::FILE, &mut text) {
        Ok(None) => return false,
        Ok(Some(n)) => core::str::from_utf8(&text[..n])
            .ok()
            .and_then(Session::parse),
        Err(()) => {
            say(b"the session file cannot be read: the text console");
            return false;
        }
    };
    match chosen {
        Some(Session::Shell) => return false,
        Some(Session::Desktop) => {}
        None => {
            say(b"the session file is damaged: the text console (session shell|desktop writes it again)");
            return false;
        }
    }
    let mut profiles = [0u8; 1024];
    let path = update_index::main_path(update_index::PROFILES_FILE);
    match read_small(fs, &path, &mut profiles) {
        Ok(Some(n)) if core::str::from_utf8(&profiles[..n]).is_ok_and(session::follows_desktop) => {
        }
        Ok(_) => {
            say(b"session = desktop, but this machine does not follow the desktop profile: the text console");
            return false;
        }
        Err(()) => {
            say(b"session = desktop, but the profile list cannot be read: the text console");
            return false;
        }
    }
    let path = update_index::installed_path(session::LAUNCHER);
    match path.as_deref().map(|p| exists(fs, p)) {
        Some(Ok(true)) => {}
        Some(Ok(false)) | None => {
            say(b"session = desktop, but the desktop's launcher is not installed yet (update, then upgrade): the text console");
            return false;
        }
        Some(Err(())) => {
            say(b"session = desktop, but whether the launcher is installed cannot be read: the text console");
            return false;
        }
    }
    // The compositor reads only the input services (the PS/2 keyboard
    // among them since ADR 0075): without a keyboard there the desktop
    // could not be typed at (audit of phase B, M2).
    if !inputs
        .iter()
        .any(|&ep| input_kinds(ep) & aricode_proto::input::KIND_KEYBOARD != 0)
    {
        say(b"session = desktop, but there is no keyboard for it: the text console");
        return false;
    }
    let mut fb = [0u8; sys::abi::sysinfo::Framebuffer::LEN];
    if sys::system_info(SYSTEM, sys::abi::sysinfo::FRAMEBUFFER, &mut fb).is_err() {
        say(b"session = desktop, but there is no display: the text console");
        return false;
    }
    true
}

/// What an input service's devices are (`input::KIND_*`, from its `Info`
/// reply); 0 when it does not answer.
fn input_kinds(service: Handle) -> u8 {
    use aricode_proto::input::{Op, Reply, Request};
    let req = Request {
        op: Op::Info,
        timeout_ms: 0,
    };
    ask(service, &req.encode(), 500, &mut [0u8; Reply::LEN])
        .and_then(Reply::decode)
        .map_or(0, |r| r.kinds)
}

/// Whether `name` exists: `Err` when fsd's answer says neither.
fn exists(fs: &mut FsClient, name: &str) -> Result<bool, ()> {
    match fs.open(name) {
        Ok(fd) => {
            let _ = fs.close(fd);
            Ok(true)
        }
        Err(FsError::Status(aricode_proto::fs::Status::NotFound)) => Ok(false),
        Err(_) => Err(()),
    }
}

/// Why the desktop session ends.
enum DesktopEnd {
    /// Its launcher's process ended, as `poll_launches` reported it.
    Launcher(LaunchReply),
    /// The compositor ended: the owner typed Ctrl+Alt+Backspace, the kernel
    /// stopped it for the same chord on the PS/2 keyboard, or it failed.
    Compositor,
}

/// Ends the desktop session (ADR 0074): the launcher and the apps it
/// started are stopped, the compositor goes, so the kernel gives the
/// console back its screen and its PS/2 keyboard, and the console bridge
/// feeds the shell from the input services again.
fn end_desktop(desktop: Desktop, why: DesktopEnd, broker: &mut Broker, sup: &mut Supervisor) {
    let mut b = [0u8; 20];
    sys::print(b"AriInit: the desktop ended (");
    match why {
        DesktopEnd::Launcher(r) => {
            sys::print(b"its launcher ");
            match r.status {
                LaunchStatus::Ok => {
                    sys::print(b"exited with ");
                    sys::print(aricode_rt::fmt_u64(r.value, &mut b));
                }
                LaunchStatus::Killed => sys::print(b"was killed"),
                _ => sys::print(b"failed"),
            }
        }
        DesktopEnd::Compositor => sys::print(b"the compositor ended"),
    }
    sys::print(b"): back to the text console\n");
    broker.stop_desktop_apps();
    broker.compositor = None;
    let Desktop { gui, inputs, count } = desktop;
    let _ = sys::process_kill(gui.process);
    // A killed process's wait answers `Killed`: only a timeout means it is
    // still there, and then it may still read the input services, which
    // are not shared with a second reader (audit of phase B, L3).
    let ended = !matches!(sys::process_wait(gui.process, 5000), Err(Error::Timeout));
    let _ = sys::handle_close(gui.process);
    let _ = sys::handle_close(gui.endpoint);
    if !ended {
        say(b"the compositor did not end within 5 s: its input services are not handed to the console bridge");
    } else if count > 0 {
        // With no input service there is nothing to bridge: the console
        // reads COM1 and the PS/2 keyboard itself.
        launch_console_bridge(sup, &inputs[..count]);
    }
}

/// Registers and launches the console bridge over `inputs` under the
/// supervisor (stage 2, ADR 0013): the user-space keyboards feed the shell
/// console. Says what it did.
fn launch_console_bridge(sup: &mut Supervisor, inputs: &[Handle]) {
    let mut ins = [Handle(0); MAX_INPUTS];
    let count = inputs.len().min(MAX_INPUTS);
    ins[..count].copy_from_slice(&inputs[..count]);
    let launched = sup
        .register(
            "console_bridge",
            Kind::ConsoleBridge { inputs: ins, count },
            Probe::None,
            Handle(0),
            true,
            Policy::DRIVER,
        )
        .is_some_and(|i| sup.launch(i));
    if launched {
        // The announcement goes here and not in the bridge: init prints
        // before the shell exists, so nothing interleaves with its first
        // prompt. The input services buffer whatever is typed until the
        // bridge reads it.
        let mut b = [0u8; 20];
        sys::print(b"AriInit: console bridge with ");
        sys::print(aricode_rt::fmt_u64(count as u64, &mut b));
        sys::print(b" input device(s)\nCONSOLE_BRIDGE_READY\n");
    } else {
        say(b"console bridge not started: the shell only reads COM1 and PS/2");
    }
}

fn main(arg: u64) -> u64 {
    say(b"starting");
    let mut args = [0u8; sys::CMDLINE_MAX];
    let cmdline = aricode_cmdline::CmdLine::new(match sys::cmdline(SYSTEM, &mut args) {
        Ok(text) => text,
        Err(_) => {
            // Said and recorded: boot flags such as `strict` are then off.
            say(b"cannot read the command line (too long or not text): boot flags ignored");
            let _ = sys::event_record(sys::CMDLINE_MAX as u64, "cmdline unreadable");
            ""
        }
    });
    if let Some(layout) = cmdline
        .get(aricode_cmdline::keys::KBD)
        .and_then(aricode_input::Layout::parse)
    {
        KBD_LAYOUT.store(layout.code(), core::sync::atomic::Ordering::Relaxed);
    }
    if cmdline.has(aricode_cmdline::keys::STRICT) {
        STRICT.store(true, core::sync::atomic::Ordering::Relaxed);
        say(b"strict mode: a failing service is not restarted");
    }
    // Incident 0005, ADR 0032: the checks that write to the disk and AriFS
    // or talk to the QEMU test hosts run only when the command line asks.
    let selftest = cmdline.has_flag(aricode_cmdline::keys::SELFTEST);
    if selftest {
        say(b"selftest: running the test-only boot checks (QEMU test environment)");
    }
    let input_restart_test = cmdline.has(aricode_cmdline::keys::INPUT_RESTART);
    let input_orphan_test = cmdline.has(aricode_cmdline::keys::INPUT_ORPHAN);
    let blk_fault = cmdline
        .get(aricode_cmdline::keys::BLK_FAULT)
        .and_then(|v| v.parse().ok());
    let mut fs_service = None;
    let mut fs_client = None;
    let mut net_wait_ms = None;
    let mut block_service = None;
    let mut services = 0u64;
    let mut sup = Supervisor::new();
    // 1. A service that exits cleanly.
    match run("hello", 0) {
        Ok(42) => say(b"hello exited with 42"),
        _ => return 1,
    }
    // 2. A service that dies from a fault: the supervisor sees it as Killed.
    match run("kernel_read", 0) {
        Err(Error::Killed) => say(b"kernel_read was killed by the kernel"),
        _ => return 2,
    }
    // 3. Without MANAGE nothing can be launched: a duplicate without rights fails.
    let Ok(weak) = sys::handle_duplicate(SPAWNER, 0) else {
        return 3;
    };
    if sys::process_spawn(weak, "hello", 0) != Err(Error::Denied) {
        return 4;
    }
    // 4. A name that does not exist in the initrd.
    if sys::process_spawn(SPAWNER, "no-existe", 0) != Err(Error::NotFound) {
        return 5;
    }
    // 5. A driver that dies is restarted (Phase 16): the second attempt starts.
    match run("crashy_driver", 0) {
        Err(Error::Killed) => say(b"crashy_driver crashed, restarting"),
        _ => return 6,
    }
    match run("crashy_driver", 1) {
        Ok(0) => say(b"crashy_driver restarted successfully"),
        _ => return 7,
    }
    // 5b. A program's console can be an endpoint (ADR 0047).
    if selftest && !session_console_check() {
        return 47;
    }
    // 6. PCI devices as seen from user space.
    let mut buf = [0u8; 20];
    match sys::pci_count(PCI_ROOT) {
        Ok(n) => {
            sys::print(b"AriInit: ");
            sys::print(aricode_rt::fmt_u64(n, &mut buf));
            sys::print(b" PCI functions\n");
        }
        Err(_) => return 8,
    }
    // 7. Block service: resident virtio-blk driver, health checks and, with
    //    `selftest`, the test clients.
    match find_pci(0x1AF4, 0x1042) {
        Some(index) => {
            let Ok(service) = sys::endpoint_create() else {
                return 9;
            };
            let launched = sup
                .register(
                    "virtio_blk",
                    Kind::Driver {
                        pci_index: index,
                        // `SYSTEM` only to read `blk_fault` (tests) or
                        // `strict` from the command line: least privilege.
                        with_system: blk_fault.is_some()
                            || cmdline.has(aricode_cmdline::keys::STRICT),
                        block: None,
                    },
                    Probe::None,
                    service,
                    false,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if !launched {
                return 10;
            }
            services |= SERVICE_BLOCK;
            block_service = Some(service);
            match start_storage_stack(&mut sup, service, selftest, blk_fault) {
                Ok((fs_ep, fs)) => {
                    services |= SERVICE_FS;
                    fs_service = Some(fs_ep);
                    fs_client = Some(fs);
                }
                Err(code) => return code,
            }
        }
        None => say(b"no virtio-blk"),
    }
    // 8. Network service: resident virtio-net driver, its address and, with
    //    `selftest`, the test clients against the QEMU test hosts.
    let Ok(net_service) = sys::endpoint_create() else {
        return 16;
    };
    match find_pci(0x1AF4, 0x1041) {
        Some(index) => {
            let launched = sup
                .register(
                    "virtio_net",
                    Kind::Driver {
                        pci_index: index,
                        with_system: true,
                        block: None,
                    },
                    Probe::None,
                    net_service,
                    true,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if !launched {
                say(b"virtio_net did not start");
                return 17;
            }
            services |= SERVICE_NET;
            net_wait_ms = Some(NET_WAIT_VIRTIO_MS);
            if selftest {
                // The QEMU test hosts at 10.0.2.2: UDP responder, TCP echo
                // and HTTP server (incident 0005).
                let clients: [(&str, &[u8], u64); 3] = [
                    ("net_client", b"network service ok", 18),
                    ("tcp_client", b"TCP service ok", 19),
                    ("http_client", b"HTTP client ok", 20),
                ];
                for (name, ok, code) in clients {
                    if let Err(code) = run_test_client(&mut sup, name, net_service, 0, ok, code) {
                        return code;
                    }
                }
            }
        }
        None => match find_pci_vendor_class(0x10EC, 0x02, 0x00) {
            // Realtek r8169 (ADR 0024): the driver is launched and gets its
            // address via DHCP; the test clients talk to the QEMU test hosts,
            // which do not exist on real hardware, so they never run here,
            // not even with `selftest`. The shell's `net`, `ping`, `dns` and
            // `http` serve as the test.
            Some(index) => {
                let launched = sup
                    .register(
                        "rtl_net",
                        Kind::Driver {
                            pci_index: index,
                            with_system: true,
                            block: None,
                        },
                        Probe::None,
                        net_service,
                        true,
                        Policy::DRIVER,
                    )
                    .is_some_and(|i| sup.launch(i));
                if launched {
                    services |= SERVICE_NET;
                    say(b"network service via Realtek");
                    net_wait_ms = Some(0);
                } else {
                    say(b"rtl_net did not start");
                }
            }
            None => say(b"no virtio-net or Realtek"),
        },
    }
    // 9. virtio input devices (keyboard, mouse): one driver for each.
    let mut input_eps = [Handle(0); 4];
    let mut inputs = 0;
    let mut next = 0;
    while let Some(index) = find_pci_from(0x1AF4, 0x1052, next) {
        next = index + 1;
        if inputs == input_eps.len() {
            break;
        }
        let Ok(ep) = sys::endpoint_create() else {
            return 21;
        };
        let launched = sup
            .register(
                "virtio_input",
                Kind::Driver {
                    pci_index: index,
                    with_system: true,
                    block: None,
                },
                Probe::Input,
                ep,
                true,
                Policy::DRIVER,
            )
            .is_some_and(|i| sup.launch(i));
        if launched {
            input_eps[inputs] = ep;
            inputs += 1;
        } else {
            say(b"virtio_input never became ready; dropped");
        }
    }
    // USB controllers (class 0c.03): the xHCI (interface 0x30) gets usbd
    // and the EHCI (0x20) usb_ehci (ADR 0031). Each serves its HID
    // keyboards and mice as a single input device, published once it
    // responds, and a block endpoint for its USB storage (ADR 0023). The
    // xHCIs go first: on Intel 7 series usbd routes the shared ports to the
    // xHCI before the EHCI driver looks at the ports left to it.
    use sys::abi::pci_info::{EHCI_PROG_IF, XHCI_PROG_IF};
    let mut usb_blocks = [Handle(0); MAX_INPUTS];
    let mut usb_slots = [0usize; MAX_INPUTS];
    let mut usb_block_count = 0;
    for (wanted, name) in [(XHCI_PROG_IF, "usb_input"), (EHCI_PROG_IF, "usb_ehci")] {
        let mut next = 0;
        while let Some(index) = find_pci_class_from(0x0C, 0x03, next) {
            next = index + 1;
            let prog_if = sys::pci_info_ext(PCI_ROOT, index)
                .map(sys::abi::pci_info::prog_if)
                .unwrap_or(0xFF);
            let info = sys::pci_info(PCI_ROOT, index).unwrap_or(0);
            if prog_if != wanted {
                if wanted == XHCI_PROG_IF && prog_if != EHCI_PROG_IF {
                    sys::print(b"AriInit: USB controller ");
                    print_bdf(info);
                    sys::print(b" has no driver (interface 0x");
                    print_hex8(prog_if);
                    sys::print(b", xHCI and EHCI only)\n");
                }
                continue;
            }
            if inputs == input_eps.len() {
                sys::print(b"AriInit: USB controller ");
                print_bdf(info);
                sys::print(b" skipped: no input service slot left\n");
                continue;
            }
            let Ok(ep) = sys::endpoint_create() else {
                return 21;
            };
            // Each controller also gets a block endpoint, which its driver
            // serves if it finds USB storage (ADR 0023); without virtio-blk
            // init uses the first one that reports a capacity.
            let Ok(block_ep) = sys::endpoint_create() else {
                return 21;
            };
            // The endpoints go through the supervisor, which owns them
            // from here on (restarts reuse them); only a driver that could
            // not even be registered leaves them to be closed here.
            let slot = sup.register(
                name,
                Kind::Driver {
                    pci_index: index,
                    with_system: true,
                    block: Some(block_ep),
                },
                Probe::Input,
                ep,
                true,
                Policy::DRIVER,
            );
            if slot.is_none() {
                let _ = sys::handle_close(ep);
                let _ = sys::handle_close(block_ep);
            }
            let launched = slot.is_some_and(|i| sup.launch(i));
            sys::print(b"AriInit: ");
            sys::print(name.as_bytes());
            if launched {
                sys::print(b" input ready\n");
                input_eps[inputs] = ep;
                inputs += 1;
                usb_blocks[usb_block_count] = block_ep;
                usb_slots[usb_block_count] = slot.unwrap_or(0);
                usb_block_count += 1;
            } else {
                sys::print(b" never became ready; dropped\n");
            }
        }
    }
    // The PS/2 keyboard and touchpad (ADR 0075): an input service for the
    // compositor only, never for the console bridge or input_client.
    let mut ps2_input = None;
    if sys::object_kind(PS2) == Ok(sys::kind::PS2) {
        let Ok(ep) = sys::endpoint_create() else {
            return 21;
        };
        let slot = sup.register(
            "ps2_input",
            Kind::Ps2Input,
            Probe::Input,
            ep,
            true,
            Policy::DRIVER,
        );
        if slot.is_none() {
            let _ = sys::handle_close(ep);
        }
        if slot.is_some_and(|i| sup.launch(i)) {
            say(b"ps2_input ready (the PS/2 keyboard and pointer, for the screen's holder)");
            ps2_input = Some(ep);
        } else {
            say(b"ps2_input never became ready; dropped");
        }
    }
    let mut gui_eps = [Handle(0); MAX_GUI_INPUTS];
    gui_eps[..inputs].copy_from_slice(&input_eps[..inputs]);
    let mut gui_inputs = inputs;
    if let Some(ep) = ps2_input {
        gui_eps[gui_inputs] = ep;
        gui_inputs += 1;
    }
    let gui_eps = &gui_eps[..gui_inputs];
    if inputs > 0 {
        services |= SERVICE_INPUT;
        // Every input service answered Info before it was published (its
        // start probe): that is the health check. input_client decodes the
        // keys and mouse events QEMU injects and would swallow whatever the
        // owner types during its idle rounds: `selftest` only.
        if selftest {
            let mut dups = [Handle(0); 4];
            for (d, ep) in dups.iter_mut().zip(&input_eps[..inputs]) {
                let Ok(h) = sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER) else {
                    return 23;
                };
                *d = h;
            }
            let result =
                sys::process_spawn_with(SPAWNER, "input_client", inputs as u64, &dups[..inputs])
                    .and_then(|child| {
                        let r = sys::process_wait(child, TIMEOUT_INFINITE);
                        let _ = sys::handle_close(child);
                        r
                    });
            match result {
                Ok(0) => say(b"input service ok"),
                Ok(code) => {
                    sys::print(b"AriInit: input_client exited with ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 24;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: input_client", e);
                    return 24;
                }
            }
        } else {
            sys::print(b"AriInit: ");
            sys::print(aricode_rt::fmt_u64(inputs as u64, &mut buf));
            sys::print(b" input device(s) ready\n");
        }
    } else {
        say(b"no input devices");
    }
    // 9b. No virtio-blk: the first USB storage that carries the system's
    //     ARI0 partition is the block service and fsd runs on top of it
    //     (ADR 0023, 0031). A stick without ARI0 (any other USB disk) is
    //     skipped; with none, the system goes on without storage.
    if block_service.is_none() && usb_block_count > 0 {
        // One client (one DMA buffer) probes every candidate.
        let mut probe = BlockClient::new(usb_blocks[0], HARDWARE, 16).ok();
        let found = probe.as_mut().and_then(|blk| {
            (0..usb_block_count).find_map(|k| {
                blk.set_service(usb_blocks[k]);
                let capacity = blk.capacity().ok().filter(|&c| c > 0)?;
                let mut b = [0u8; 20];
                let guid = match blk.read(1, 1) {
                    Err(e) => {
                        aricode_rt::print_error(b"AriInit: USB storage: GPT header read failed", e);
                        return None;
                    }
                    Ok(_) => {
                        aricode_gpt::Header::parse(&blk.buffer()[..512])
                            .ok()?
                            .disk_guid
                    }
                };
                if find_partition(blk, "ARI0").is_none() {
                    sys::print(b"AriInit: USB storage of ");
                    sys::print(aricode_rt::fmt_u64(capacity, &mut b));
                    sys::print(b" sectors without an ARI0 partition: not the system disk\n");
                    return None;
                }
                Some((k, capacity, guid))
            })
        });
        match found {
            Some((k, capacity, guid)) => {
                let ep = usb_blocks[k];
                // A relaunched driver serves this disk only (ADR 0031).
                sup.set_driver_arg(usb_slots[k], driver_arg::expected_disk(&guid));
                sys::print(b"AriInit: block service via USB (");
                sys::print(aricode_rt::fmt_u64(capacity, &mut buf));
                sys::print(b" sectors)\n");
                services |= SERVICE_BLOCK;
                block_service = Some(ep);
                // `blk_fault` belongs to the virtio-blk driver only.
                match start_storage_stack(&mut sup, ep, selftest, None) {
                    Ok((fs_ep, fs)) => {
                        services |= SERVICE_FS;
                        fs_service = Some(fs_ep);
                        fs_client = Some(fs);
                    }
                    Err(code) => return code,
                }
            }
            None => say(b"no USB storage with an ARI0 partition"),
        }
    }
    // 9b'. Carried kernel seed (ADR 0043): AriBoot already handed the
    //      kernel its share and wrote a successor; once the system disk is
    //      known (virtio-blk or USB, so after the network driver started)
    //      that successor is replaced with output of the kernel generator,
    //      and again once the boot services ran.
    report_carried_seed();
    let mut seed_store = block_service.and_then(SeedStore::open);
    if let Some(store) = seed_store.as_mut() {
        store.refresh(b"early");
    }
    // 9c. Network health check, after the USB devices so that a machine
    //     waiting for DHCP does not delay its stick and keyboard.
    if let Some(wait_ms) = net_wait_ms {
        report_network_address(net_service, wait_ms);
    }
    // 10. Graphical interface: the compositor with the framebuffer and the
    //     inputs; applications ask the broker for their windows.
    let mut gui = None;
    if arg & sys::abi::init_arg::GUI != 0 {
        match run_gui(gui_eps) {
            Ok(g) => {
                services |= SERVICE_GUI;
                gui = Some(g);
                say(b"gui ok (compositor started)");
            }
            Err(code) => {
                sys::print(b"AriInit: gui failed with ");
                sys::print(aricode_rt::fmt_u64(code, &mut buf));
                sys::print(b"\n");
                return 25;
            }
        }
    }
    // 10b. Restart storm (tests only, `restart_storm`): 100 restarts of a
    //      test service under the supervisor without leaking resources.
    if arg & sys::abi::init_arg::RESTART_STORM != 0 {
        let Some(fs) = fs_client.as_mut() else {
            return 40;
        };
        if let Err(code) = restart_storm(&mut sup, fs) {
            return code;
        }
    }
    // 10c. Service that does not answer (tests only, `service_hang`): the
    //      start probe times out, the supervisor kills it and none of its
    //      processes is left alive (audit 6ee19fb, finding 4).
    if arg & sys::abi::init_arg::SERVICE_HANG != 0 {
        if let Err(code) = service_hang(&mut sup) {
            return code;
        }
    }
    // 10d. Input driver that crashes holding a read (tests only,
    //      `input_restart`, ADR 0030): the client keeps the device.
    if input_restart_test {
        if let Err(code) = input_restart(&mut sup) {
            return code;
        }
    }
    // 11. A/B boot: only now, with every service checked, is the pending slot
    //     confirmed. Any earlier error return leaves the state pending so
    //     AriBoot runs out of tries and rolls back.
    if arg & sys::abi::init_arg::FAIL_LATE != 0 {
        say(b"induced late failure (init_fail_late): slot not confirmed");
        if let Some(service) = block_service {
            report_boot_state(service);
        }
        return 99;
    }
    match block_service {
        Some(service) => confirm_boot_slot(arg, service),
        None => say(b"no block service: boot not confirmed"),
    }
    say(b"boot services completed");
    // The second refresh, with the interrupts of the whole boot (network,
    // USB, disk) in the generator.
    if let Some(store) = seed_store.as_mut() {
        store.refresh(b"after the boot services");
    }
    if arg & sys::abi::init_arg::SHELL != 0 {
        let Some(fs) = fs_service else {
            say(b"no file service, not launching the shell");
            return 15;
        };
        // The desktop session (ADR 0074), decided before the console
        // bridge: the compositor takes the input services.
        // With `gui` on the command line the compositor is the command
        // line's, and stays: no desktop session is started on it.
        let desktop_session = gui.is_none()
            && fs_client
                .as_mut()
                .is_some_and(|fs| desktop_wanted(fs, gui_eps));
        if desktop_session {
            match run_gui(gui_eps) {
                Ok(g) => {
                    services |= SERVICE_GUI;
                    gui = Some(g);
                    say(b"desktop session: compositor started");
                }
                Err(code) => {
                    sys::print(b"AriInit: the desktop's compositor did not start (code ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"): the text console\n");
                }
            }
        }
        let Ok(mut broker) = Broker::new(
            fs_client,
            block_service,
            (services & SERVICE_NET != 0).then_some(net_service),
            gui.as_ref().map(|g| g.endpoint),
        ) else {
            return 26;
        };
        // Stage 2 (ADR 0013): the user-space keyboards feed the shell console
        // as long as there is no compositor (with the GUI the events belong
        // to it, and each input service accepts a single reader).
        if inputs > 0 && gui.is_none() {
            if input_orphan_test {
                orphan_reads(&input_eps[..inputs], &mut sup);
            }
            launch_console_bridge(&mut sup, &input_eps[..inputs]);
        }
        let mut desktop = None;
        if let Some(g) = gui.take().filter(|_| desktop_session) {
            let mut ins = [Handle(0); MAX_INPUTS];
            ins[..inputs].copy_from_slice(&input_eps[..inputs]);
            let d = Desktop {
                gui: g,
                inputs: ins,
                count: inputs,
            };
            if broker.launch_session() {
                say(b"desktop session: launcher started");
                desktop = Some(d);
            } else {
                end_desktop(
                    d,
                    DesktopEnd::Launcher(LaunchReply::status(LaunchStatus::Failed)),
                    &mut broker,
                    &mut sup,
                );
            }
        }
        // SSH (ADR 0025): with a network service, sshd serves remote shells
        // that init launches on its behalf.
        let mut sessions = None;
        if services & SERVICE_NET != 0 {
            if let Some(fs) = broker.fs.as_mut() {
                move_sshd_files(fs);
            }
            if let Ok(endpoint) = sys::endpoint_create() {
                let launched = sup
                    .register(
                        "sshd",
                        Kind::Sshd {
                            fs,
                            net: net_service,
                            sessions: endpoint,
                        },
                        Probe::None,
                        Handle(0),
                        true,
                        Policy::DRIVER,
                    )
                    .is_some_and(|i| sup.launch(i));
                if launched {
                    // Wait (bounded) for sshd to report that it listens, so
                    // its boot lines do not interleave with the shell prompt.
                    let mut msg = [0u8; aricode_proto::session::Request::LEN];
                    let mut none = [Handle(0); 1];
                    let _ = sys::ipc_receive(endpoint, &mut msg, &mut none, 5000);
                    sessions = Some(Sessions {
                        endpoint,
                        children: [None; MAX_SESSIONS],
                        terminal: false,
                        said_malformed: false,
                    });
                } else {
                    say(b"sshd did not start");
                }
            }
        }
        match run_shell(
            net_service,
            services,
            &mut broker,
            &mut sup,
            sessions.as_mut(),
            &mut desktop,
        ) {
            Ok(code) => {
                sys::print(b"AriInit: shell exited with ");
                sys::print(aricode_rt::fmt_u64(code, &mut buf));
                sys::print(b"\n");
            }
            Err(e) => aricode_rt::print_error(b"AriInit: shell", e),
        }
    }
    0
}

aricode_rt::entry!(main);
