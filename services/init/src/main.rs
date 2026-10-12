//! AriInit (Phase 15): first user process. It receives from the kernel the
//! `SPAWNER` handle (0x10004, `MANAGE`), the PCI root (0x10005, `READ|MANAGE`),
//! a hardware capability (0x10006, `MANAGE`) to hand out DMA, the `SYSTEM`
//! object (0x10007) and the console input (0x10008, `READ`). It launches the
//! boot services and supervises them; until a dedicated device manager exists
//! it also claims devices and launches their drivers (Phases 18 and 19).
//! If `arg` carries `init_arg::SHELL`, it launches the shell at the end
//! (Phase 21).
//!
//! In 0.0.8 its service list is a test one: it demonstrates launching,
//! waiting, reacting to a process killed by a fault and mounting the block
//! service.
#![no_std]
#![no_main]

extern crate alloc;

use aricode_manifest::{Caps, Manifest};
use aricode_proto::launch::{
    Reply as LaunchReply, Request as LaunchRequest, Status as LaunchStatus,
};
use aricode_proto::status;
use aricode_rt::sys::block::BlockClient;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, rights, Error, Handle, TIMEOUT_INFINITE};
use aricode_supervisor::{Action, Exit, Policy, Service, State};

/// Development public key used to sign packages (ADR 0017).
const PUBKEY_HEX: &str = include_str!("../../../keys/dev/ed25519.pub");

const SPAWNER: Handle = Handle(0x0001_0004);
const PCI_ROOT: Handle = Handle(0x0001_0005);
const HARDWARE: Handle = Handle(0x0001_0006);
const SYSTEM: Handle = Handle(0x0001_0007);
const CONSOLE_IN: Handle = Handle(0x0001_0008);
const FRAMEBUFFER: Handle = Handle(0x0001_0009);

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
    let Ok(reply) = sys::endpoint_create() else {
        return false;
    };
    let mut ok = false;
    for _ in 0..12 {
        match sys::process_wait(driver, 0) {
            Err(Error::Timeout) => {}
            _ => break, // exited (with a code or by a fault): it will not serve
        }
        let Ok(reply_w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) else {
            break;
        };
        if sys::ipc_send(service, request, &[reply_w], 250).is_err() {
            continue;
        }
        let mut msg = [0u8; 256];
        let mut none: [Handle; 0] = [];
        if let Ok((n, _)) = sys::ipc_receive(reply, &mut msg, &mut none, 250) {
            ok = accept(&msg[..n]);
            break;
        }
    }
    let _ = sys::handle_close(reply);
    ok
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
    let Ok(reply) = sys::endpoint_create() else {
        return false;
    };
    let mut ok = false;
    if let Ok(reply_w) = sys::handle_duplicate(reply, rights::WRITE | rights::TRANSFER) {
        if sys::ipc_send(service, request, &[reply_w], 500).is_ok() {
            let mut msg = [0u8; 16];
            let mut none: [Handle; 0] = [];
            if let Ok((n, _)) = sys::ipc_receive(reply, &mut msg, &mut none, 500) {
                ok = &msg[..n] == b"pong";
            }
        }
    }
    let _ = sys::handle_close(reply);
    ok
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
/// carries a mask: bit 15 = IRQ present, bits 0..5 = BAR i present.
fn start_driver(
    name: &str,
    index: u64,
    service: Handle,
    extras: &[Handle],
) -> Result<Handle, Error> {
    let handles = sys::pci_claim(PCI_ROOT, index)?;
    let mut to_send = [Handle(0); 12];
    to_send[0] = sys::handle_duplicate(service, rights::READ | rights::WRITE | rights::TRANSFER)?;
    let mut n = 1;
    let mut mask = 0u64;
    for (i, h) in handles.iter().enumerate() {
        if h.0 == 0 {
            continue;
        }
        match i {
            2 => mask |= 1 << 15,
            3..=8 => mask |= 1 << (i - 3),
            _ => {}
        }
        to_send[n] = *h;
        n += 1;
    }
    for h in extras {
        to_send[n] = *h;
        n += 1;
    }
    sys::process_spawn_with(SPAWNER, name, mask, &to_send[..n])
}

/// First and last LBA of the GPT partition `name`, reading the table through
/// the block service.
fn find_partition(blk: &mut BlockClient, name: &str) -> Option<(u64, u64)> {
    blk.read(1, 1).ok()?;
    let header = aricode_gpt::Header::parse(&blk.buffer()[..512]).ok()?;
    let sectors = header.entries_bytes().div_ceil(512) as u64;
    let mut table = alloc::vec![0u8; sectors as usize * 512];
    let mut lba = 0;
    while lba < sectors {
        let n = (sectors - lba).min(16) as u16;
        blk.read(header.entries_lba + lba, n).ok()?;
        let o = lba as usize * 512;
        table[o..o + n as usize * 512].copy_from_slice(&blk.buffer()[..n as usize * 512]);
        lba += n as u64;
    }
    let mut parts = [None; 16];
    let count = aricode_gpt::partitions(&header, &table, &mut parts).ok()?;
    parts[..count]
        .iter()
        .flatten()
        .find(|p| p.name_str() == name)
        .map(|p| (p.first_lba, p.last_lba))
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

/// On top of an already running block service: fsd on the ARI0 partition
/// (found by GPT name: the test disk and the USB image place it at different
/// LBAs), its test client and the block client. Returns the fsd endpoint or
/// init's exit code.
fn start_storage_stack(sup: &mut Supervisor, service: Handle) -> Result<Handle, u64> {
    let mut buf = [0u8; 20];
    let Ok(fs_ep) = sys::endpoint_create() else {
        return Err(12);
    };
    let ari0 = BlockClient::new(service, HARDWARE, 16)
        .ok()
        .and_then(|mut blk| find_partition(&mut blk, "ARI0"));
    let Some((ari0_lba, _)) = ari0 else {
        say(b"no ARI0 partition on the disk");
        return Err(12);
    };
    let launched = sup
        .register(
            "fsd",
            Kind::Fs {
                block_service: service,
                partition_lba: ari0_lba,
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
    match run_block_client("fs_client", fs_ep, 0) {
        Ok(0) => say(b"file service ok"),
        Ok(code) => {
            sys::print(b"AriInit: fs_client exited with ");
            sys::print(aricode_rt::fmt_u64(code, &mut buf));
            sys::print(b"\n");
            return Err(14);
        }
        Err(e) => {
            aricode_rt::print_error(b"AriInit: fs_client", e);
            return Err(14);
        }
    }
    match run_block_client("blk_client", service, ari0_lba + RAW_AREA_OFFSET) {
        Ok(0) => say(b"block service ok"),
        Ok(code) => {
            sys::print(b"AriInit: blk_client exited with ");
            sys::print(aricode_rt::fmt_u64(code, &mut buf));
            sys::print(b"\n");
            return Err(11);
        }
        Err(e) => {
            aricode_rt::print_error(b"AriInit: blk_client", e);
            return Err(11);
        }
    }
    Ok(fs_ep)
}

/// Launches fsd with its endpoint (READ|WRITE), the block service endpoint
/// (WRITE) and a hardware capability; `arg` = partition LBA.
fn start_fsd(
    fs_service: Handle,
    block_service: Handle,
    partition_lba: u64,
) -> Result<Handle, Error> {
    let own = sys::handle_duplicate(fs_service, rights::READ | rights::WRITE | rights::TRANSFER)?;
    let blk = sys::handle_duplicate(block_service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    // SYSTEM (read): clock to expire abandoned chunked replaces.
    let system = sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER)?;
    sys::process_spawn_with(SPAWNER, "fsd", partition_lba, &[own, blk, hw, system])
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
}

/// Input services that init hands out (compositor, broker, bridge).
const MAX_INPUTS: usize = 4;

/// Launches the console bridge with the console input (`MANAGE`) and a
/// duplicate (`WRITE`) of each input endpoint; `arg` = how many. If a
/// duplicate fails, closes the ones already made.
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
        sys::process_spawn_with(
            SPAWNER,
            "console_bridge",
            inputs.len() as u64,
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

/// Launches sshd with `[net (WRITE), fs (WRITE), hardware, SYSTEM (READ),
/// sessions (WRITE)]`. It gets no SPAWNER: shells are launched by init.
fn start_sshd(fs: Handle, net: Handle, sessions: Handle) -> Result<Handle, Error> {
    let w = rights::WRITE | rights::TRANSFER;
    let handles = [
        sys::handle_duplicate(net, w)?,
        sys::handle_duplicate(fs, w)?,
        sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?,
        sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER)?,
        sys::handle_duplicate(sessions, w)?,
    ];
    sys::process_spawn_with(SPAWNER, "sshd", 0, &handles)
}

/// How to check that the service is already serving.
#[derive(Clone, Copy)]
enum Probe {
    /// A test client checks it afterwards (block, files, network).
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
    sup: Service,
}

const MAX_RESIDENTS: usize = 10;

/// Supervisor of resident services (stage 1 of the post-roadmap plan):
/// states, generations, restart with increasing backoff and a budget, and
/// queryable status (`status` v1) for the shell.
/// `strict` on the command line (ADR 0028): nothing is restarted.
static STRICT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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
            sup: Service::new(policy, optional),
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
                    Err(Error::Limit)
                } else {
                    start_driver(r.name, pci_index, r.endpoint, &extras[..n])
                }
            }
            Kind::Fs {
                block_service,
                partition_lba,
            } => start_fsd(r.endpoint, block_service, partition_lba),
            Kind::Program { arg } => {
                sys::handle_duplicate(r.endpoint, rights::READ | rights::WRITE | rights::TRANSFER)
                    .and_then(|ep| sys::process_spawn_with(SPAWNER, r.name, arg, &[ep]))
            }
            Kind::ConsoleBridge { inputs, count } => start_console_bridge(&inputs[..count]),
            Kind::Sshd { fs, net, sessions } => start_sshd(fs, net, sessions),
        };
        let now = now_ms();
        let process = match spawned {
            Ok(h) => h,
            Err(e) => {
                aricode_rt::print_error(b"AriInit: launch service", e);
                let _ = r.sup.starting(now);
                self.announce_death(i, now, Exit::Code(u64::MAX));
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
            sys::print(b"AriInit: service ");
            sys::print(r.name.as_bytes());
            sys::print(b" neither responds nor exits: failed, not relaunching\n");
            r.sup.stop();
        }
        self.announce_death(i, now_ms(), Exit::StartTimeout);
    }

    /// Records the death of `i`, closes its process and announces the decision.
    fn announce_death(&mut self, i: usize, now: u64, exit: Exit) {
        let Some(r) = self.slots[i].as_mut() else {
            return;
        };
        if let Some(h) = r.process.take() {
            let _ = sys::handle_close(h);
        }
        let action = r.sup.died(now, exit);
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
    }

    /// Watches the processes: deaths, start timeouts and restarts that are due.
    fn poll(&mut self) {
        let now = now_ms();
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
                    Ok(code) => self.announce_death(i, now, Exit::Code(code)),
                    Err(Error::Killed) => self.announce_death(i, now, Exit::Killed),
                    Err(_) => self.announce_death(i, now, Exit::Code(u64::MAX)),
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

/// Offset of the raw area inside ARI0 (the test disk has it at LBA 4096
/// with ARI0 at 34; the USB image, at the same offset).
const RAW_AREA_OFFSET: u64 = 4096 - 34;

/// Launches a client of a service with its endpoint (WRITE only) and a
/// hardware capability for its DMA buffer, and waits for it to exit.
fn run_block_client(name: &str, service: Handle, arg: u64) -> Result<u64, Error> {
    let ep = sys::handle_duplicate(service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    let child = sys::process_spawn_with(SPAWNER, name, arg, &[ep, hw])?;
    let result = sys::process_wait(child, TIMEOUT_INFINITE);
    let _ = sys::handle_close(child);
    result
}

/// Launches the shell with the fsd endpoint (WRITE), hardware for its
/// buffer, the system object (READ|MANAGE), the console input, the PCI root
/// read-only, the netd endpoint (WRITE) and the launch broker for `run`;
/// `arg` = active services. Waits for it to exit.
fn run_shell(
    fs_service: Handle,
    net_service: Handle,
    services: u64,
    broker: &mut Broker,
    sup: &mut Supervisor,
    mut sessions: Option<&mut Sessions>,
) -> Result<u64, Error> {
    let console_in = sys::handle_duplicate(CONSOLE_IN, rights::READ | rights::TRANSFER)?;
    let child = spawn_shell(
        fs_service,
        net_service,
        broker.endpoint,
        console_in,
        None,
        services,
    )?;
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
        broker.poll_launches();
        if let Some(s) = sessions.as_deref_mut() {
            s.serve(fs_service, net_service, services, broker.endpoint);
        }
    }
}

/// Launches `arish` with the file endpoint (WRITE), hardware for its
/// buffer, the system (READ|MANAGE), its input (`input`, transferred: the
/// console or a session endpoint), the read-only PCI root, the network
/// endpoint (WRITE), the broker for `run`, and, in session mode, the
/// session output endpoint (`output`, transferred). `arg` = active services
/// plus the session bits.
fn spawn_shell(
    fs_service: Handle,
    net_service: Handle,
    broker: Handle,
    input: Handle,
    output: Option<Handle>,
    arg: u64,
) -> Result<Handle, Error> {
    let fsd = sys::handle_duplicate(fs_service, rights::WRITE | rights::TRANSFER)?;
    let hw = sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER)?;
    // With DUPLICATE the shell can in turn pass a read-only SYSTEM to the
    // programs it launches with `run`.
    let system = sys::handle_duplicate(
        SYSTEM,
        rights::READ | rights::MANAGE | rights::DUPLICATE | rights::TRANSFER,
    )?;
    let pci = sys::handle_duplicate(PCI_ROOT, rights::READ | rights::TRANSFER)?;
    let netd = sys::handle_duplicate(net_service, rights::WRITE | rights::TRANSFER)?;
    // The shell does not get the SPAWNER: it asks the broker (init) to launch.
    let launch = sys::handle_duplicate(broker, rights::WRITE | rights::TRANSFER)?;
    let mut handles = [fsd, hw, system, input, pci, netd, launch, Handle(0)];
    let n = match output {
        Some(out) => {
            handles[7] = out;
            8
        }
        None => 7,
    };
    sys::process_spawn_with(SPAWNER, "shell", arg, &handles[..n])
}

/// Session shells for sshd (ADR 0025): requests arrive on `endpoint`;
/// finished shells are reaped on every pass.
struct Sessions {
    endpoint: Handle,
    children: [Option<Handle>; MAX_SESSIONS],
}

const MAX_SESSIONS: usize = 4;

impl Sessions {
    fn serve(&mut self, fs: Handle, net: Handle, services: u64, broker: Handle) {
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
            return;
        }
        let [reply_to, input, output] = handles;
        let slot = self.children.iter().position(Option::is_none);
        let spawned = match (session::Request::decode(&msg[..n]), slot) {
            (Some(req), Some(slot)) => {
                let quiet = if req.kind == SessionKind::Exec {
                    session::SHELL_QUIET
                } else {
                    0
                };
                let arg = services | session::SHELL_SESSION | quiet;
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
            say(b"session shell refused (limit or invalid request)");
        }
        let _ = sys::ipc_send(reply_to, &session::reply(spawned), &[], 0);
        let _ = sys::handle_close(reply_to);
    }
}

/// Capability broker (Phase 28): launches programs according to their
/// manifest. An application receives, in this order, only what it asks for
/// and is allowed: `SYSTEM` (read), a private directory in fsd, the network
/// endpoint, the compositor endpoint, the block endpoint (trusted
/// applications only), the input endpoints, and finally `HARDWARE` if it
/// needs buffers, bounded by the manifest's memory.
struct Broker {
    endpoint: Handle,
    fs: Option<FsClient>,
    /// Main fsd endpoint, for the `install` capability (ADR 0018).
    fs_root: Option<Handle>,
    block: Option<Handle>,
    net: Handle,
    compositor: Option<Handle>,
    inputs: [Handle; 4],
    input_count: usize,
    /// Launched programs that have not exited yet (ADR 0015): process and,
    /// if someone waits for its exit, the endpoint to reply to.
    running: [Option<Running>; MAX_RUNNING],
    /// DMA staging buffer for packages installed in AriFS (ADR 0017):
    /// handle and address, allocated the first time it is needed.
    stage: Option<(Handle, u64)>,
}

/// Size of the staging buffer: an installed package cannot exceed it.
const STAGE_LEN: u64 = 256 * 1024;

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

/// Programs the broker allows running at once; the next `run` or `spawn`
/// gets `Limit`.
const MAX_RUNNING: usize = 4;

#[derive(Clone, Copy)]
struct Running {
    process: Handle,
    reply_to: Option<Handle>,
}

/// Immediate result of `launch`: an already decided reply, or a created
/// process whose exit must be awaited in a slot.
enum Launched {
    Reply(LaunchReply),
    Process(Handle),
}

impl Broker {
    fn new(
        fs_service: Option<Handle>,
        block: Option<Handle>,
        net: Handle,
        compositor: Option<Handle>,
        inputs: &[Handle],
    ) -> Result<Self, Error> {
        let fs = match fs_service {
            Some(ep) => Some(FsClient::new(ep, HARDWARE)?),
            None => None,
        };
        let mut ins = [Handle(0); 4];
        let n = inputs.len().min(4);
        ins[..n].copy_from_slice(&inputs[..n]);
        Ok(Self {
            endpoint: sys::endpoint_create()?,
            fs,
            fs_root: fs_service,
            block,
            net,
            compositor,
            inputs: ins,
            input_count: n,
            running: [None; MAX_RUNNING],
            stage: None,
        })
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
            sys::print(b"AriInit: installed package too large\n");
            return Err(LaunchStatus::Failed);
        }
        let virt = match self.stage {
            Some((_, virt)) => virt,
            None => {
                let dma = sys::dma_alloc(HARDWARE, STAGE_LEN).map_err(|_| LaunchStatus::Failed)?;
                let virt = sys::mmio_map(dma).map_err(|_| LaunchStatus::Failed)?;
                self.stage = Some((dma, virt));
                virt
            }
        };
        // SAFETY: our own DMA buffer of STAGE_LEN bytes, mapped RW in init.
        let stage = unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, STAGE_LEN as usize) };
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
        let mut pubkey = [0u8; 32];
        if aricode_crypto::unhex(PUBKEY_HEX.trim(), &mut pubkey).is_none() {
            return Err(LaunchStatus::Failed);
        }
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
    fn poll_launches(&mut self) {
        for slot in self.running.iter_mut() {
            let Some(r) = *slot else { continue };
            let reply = match sys::process_wait(r.process, 0) {
                Err(Error::Timeout) => continue,
                Ok(code) => LaunchReply::exited(code),
                Err(Error::Killed) => LaunchReply::status(LaunchStatus::Killed),
                Err(_) => LaunchReply::status(LaunchStatus::Failed),
            };
            let _ = sys::handle_close(r.process);
            if let Some(reply_to) = r.reply_to {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            *slot = None;
        }
    }

    /// Serves at most one request (waits up to `timeout_ms`).
    fn serve_one(&mut self, timeout_ms: u64, sup: &Supervisor) {
        let mut msg = [0u8; LaunchRequest::LEN];
        let mut handles = [Handle(0); 1];
        let Ok((n, nh)) = sys::ipc_receive(self.endpoint, &mut msg, &mut handles, timeout_ms)
        else {
            return;
        };
        if nh == 0 {
            return;
        }
        let reply_to = handles[0];
        // The same endpoint serves two protocols: launches (6) and service
        // status (7); the first byte tells them apart.
        if let Some(req) = status::Request::decode(&msg[..n]) {
            let reply = sup.status(req.index);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        }
        let Some(req) = LaunchRequest::decode(&msg[..n]) else {
            let reply = LaunchReply::status(LaunchStatus::Invalid);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        };
        // The slot is reserved before launching: no slot, no process.
        let Some(slot) = self.running.iter().position(Option::is_none) else {
            sys::print(b"AriInit: ");
            sys::print(req.name_str().as_bytes());
            sys::print(b": running program limit reached\n");
            let reply = LaunchReply::status(LaunchStatus::Limit);
            let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
            let _ = sys::handle_close(reply_to);
            return;
        };
        match self.launch(req.name_str(), req.arg) {
            Launched::Reply(reply) => {
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
            }
            Launched::Process(process) if req.is_detached() => {
                let reply = LaunchReply::status(LaunchStatus::Started);
                let _ = sys::ipc_send(reply_to, &reply.encode(), &[], 0);
                let _ = sys::handle_close(reply_to);
                self.running[slot] = Some(Running {
                    process,
                    reply_to: None,
                });
            }
            Launched::Process(process) => {
                self.running[slot] = Some(Running {
                    process,
                    reply_to: Some(reply_to),
                });
            }
        }
    }

    /// Reads the manifest (`NAME.manifest` in the initrd, or the one in the
    /// package `apps/NAME.aripkg` in AriFS if the program is installed,
    /// ADR 0017), applies the policy and launches the program; its exit is
    /// collected by `poll_launches`.
    fn launch(&mut self, name: &str, arg: u64) -> Launched {
        let mut path = [0u8; 48];
        let Some(path) = join(name, ".manifest", &mut path) else {
            return Launched::Reply(LaunchReply::status(LaunchStatus::Invalid));
        };
        // One byte is enough to know whether the program is in the initrd:
        // TooBig if it exists, NotFound otherwise.
        let mut probe = [0u8; 1];
        let in_initrd = !matches!(
            sys::initrd_read(SPAWNER, name, &mut probe),
            Err(Error::NotFound)
        );
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
        if !denied.is_empty() {
            sys::print(b"AriInit: denied, requests system capabilities:");
            for c in denied.names() {
                sys::print(b" ");
                sys::print(c.as_bytes());
            }
            sys::print(b"\n");
            return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
        }
        // The prepared duplicates close themselves if any error path returns
        // before transferring them (`Granted::take` hands them to the
        // kernel). Previously, a denial after granting SYSTEM leaked one table
        // entry per attempt.
        let mut granted = Granted::new();
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
                Err(sys::fs::FsError::Status(aricode_proto::fs::Status::NotFound)) => {
                    sys::print(b"AriInit: denied, shared file does not exist: ");
                    sys::print(file.as_bytes());
                    sys::print(b"\n");
                    return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
                }
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::NET) {
            match sys::handle_duplicate(self.net, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::WINDOW) {
            let Some(comp) = self.compositor else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match sys::handle_duplicate(comp, rights::WRITE | rights::TRANSFER) {
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
        if caps.contains(Caps::INSTALL) {
            let Some(root) = self.fs_root else {
                return Launched::Reply(LaunchReply::status(LaunchStatus::Denied));
            };
            match sys::handle_duplicate(root, rights::WRITE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
            needs_hw = true;
        }
        if caps.contains(Caps::INPUT) {
            for ep in &self.inputs[..self.input_count] {
                match sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER) {
                    Ok(h) => granted.push(h),
                    Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
                }
            }
        }
        if needs_hw {
            match sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER) {
                Ok(h) => granted.push(h),
                Err(_) => return Launched::Reply(LaunchReply::status(LaunchStatus::Failed)),
            }
        }
        // The kernel only takes the handles if it gets to create the process;
        // on any failure the guard closes them on exit (closing one already
        // taken returns BadHandle, which is ignored).
        let spawned = match source {
            Source::Initrd => {
                sys::process_spawn_limited(SPAWNER, name, arg, granted.handles(), memory_kib)
            }
            Source::Installed {
                elf_offset,
                elf_len,
                ..
            } => {
                let (dma, _) = self.stage.expect("staging buffer allocated");
                sys::process_spawn_from(
                    SPAWNER,
                    dma,
                    elf_offset,
                    elf_len,
                    name,
                    arg,
                    granted.handles(),
                    memory_kib,
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
        Launched::Process(child)
    }
}

/// Handles prepared for a launch: they are closed on drop unless `take` has
/// handed them to the kernel.
struct Granted {
    handles: [Handle; 12],
    count: usize,
    transferred: bool,
}

impl Granted {
    fn new() -> Self {
        Self {
            handles: [Handle(0); 12],
            count: 0,
            transferred: false,
        }
    }
    fn push(&mut self, h: Handle) {
        if self.count < self.handles.len() {
            self.handles[self.count] = h;
            self.count += 1;
        } else {
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
/// its back buffer and the input services) and two graphical applications
/// sharing the screen; waits for the applications to exit.
fn run_gui(inputs: &[Handle]) -> Result<Handle, u64> {
    let service = sys::endpoint_create().map_err(|_| 1u64)?;
    let mut handles = [Handle(0); 8];
    handles[0] = sys::handle_duplicate(service, rights::READ | rights::WRITE | rights::TRANSFER)
        .map_err(|_| 2u64)?;
    handles[1] =
        sys::handle_duplicate(FRAMEBUFFER, rights::READ | rights::WRITE | rights::TRANSFER)
            .map_err(|_| 3u64)?;
    handles[2] =
        sys::handle_duplicate(SYSTEM, rights::READ | rights::TRANSFER).map_err(|_| 4u64)?;
    handles[3] =
        sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER).map_err(|_| 5u64)?;
    let mut n = 4;
    for ep in inputs.iter().take(4) {
        handles[n] =
            sys::handle_duplicate(*ep, rights::WRITE | rights::TRANSFER).map_err(|_| 6u64)?;
        n += 1;
    }
    let _compositor =
        sys::process_spawn_with(SPAWNER, "compositor", inputs.len() as u64, &handles[..n])
            .map_err(|_| 7u64)?;
    let mut apps = [Handle(0); 2];
    for (i, app) in apps.iter_mut().enumerate() {
        let ep =
            sys::handle_duplicate(service, rights::WRITE | rights::TRANSFER).map_err(|_| 8u64)?;
        let hw =
            sys::handle_duplicate(HARDWARE, rights::MANAGE | rights::TRANSFER).map_err(|_| 8u64)?;
        *app =
            sys::process_spawn_with(SPAWNER, "gui_demo", i as u64, &[ep, hw]).map_err(|_| 9u64)?;
    }
    for app in apps {
        let code = sys::process_wait(app, TIMEOUT_INFINITE).map_err(|_| 10u64)?;
        let _ = sys::handle_close(app);
        if code != 0 {
            return Err(100 + code);
        }
    }
    Ok(service)
}

fn main(arg: u64) -> u64 {
    say(b"starting");
    let mut args = [0u8; 256];
    if sys::cmdline(SYSTEM, &mut args)
        .is_some_and(|c| aricode_cmdline::CmdLine::new(c).has(aricode_cmdline::keys::STRICT))
    {
        STRICT.store(true, core::sync::atomic::Ordering::Relaxed);
        say(b"strict mode: a failing service is not restarted");
    }
    let mut fs_service = None;
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
    // 7. Block service: resident virtio-blk driver + test client.
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
                        with_system: false,
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
            match start_storage_stack(&mut sup, service) {
                Ok(fs_ep) => {
                    services |= SERVICE_FS;
                    fs_service = Some(fs_ep);
                }
                Err(code) => return code,
            }
        }
        None => say(b"no virtio-blk"),
    }
    // 8. Network service: resident virtio-net driver + test client.
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
            match run_block_client("net_client", net_service, 0) {
                Ok(0) => say(b"network service ok"),
                Ok(code) => {
                    sys::print(b"AriInit: net_client exited with ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 18;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: net_client", e);
                    return 18;
                }
            }
            match run_block_client("tcp_client", net_service, 0) {
                Ok(0) => say(b"TCP service ok"),
                Ok(code) => {
                    sys::print(b"AriInit: tcp_client exited with ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 19;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: tcp_client", e);
                    return 19;
                }
            }
            match run_block_client("http_client", net_service, 0) {
                Ok(0) => say(b"HTTP client ok"),
                Ok(code) => {
                    sys::print(b"AriInit: http_client exited with ");
                    sys::print(aricode_rt::fmt_u64(code, &mut buf));
                    sys::print(b"\n");
                    return 20;
                }
                Err(e) => {
                    aricode_rt::print_error(b"AriInit: http_client", e);
                    return 20;
                }
            }
        }
        None => match find_pci_vendor_class(0x10EC, 0x02, 0x00) {
            // Realtek r8169 (ADR 0024): the driver is launched and gets its
            // address via DHCP; the test clients talk to the QEMU responder,
            // which does not exist on real hardware, so they are not run. The
            // shell's `net`, `ping`, `dns` and `http` serve as the test.
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
                    say(b"network service via Realtek (no test clients)");
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
    // USB controllers (class 0c.03): only xHCI (interface 0x30) has a
    // driver; usbd enumerates HID keyboards and mice and serves them as a
    // single input device, published once it responds.
    let mut usb_block: Option<Handle> = None;
    let mut next = 0;
    while let Some(index) = find_pci_class_from(0x0C, 0x03, next) {
        next = index + 1;
        let prog_if = sys::pci_info_ext(PCI_ROOT, index)
            .map(sys::abi::pci_info::prog_if)
            .unwrap_or(0xFF);
        if prog_if != sys::abi::pci_info::XHCI_PROG_IF {
            let info = sys::pci_info(PCI_ROOT, index).unwrap_or(0);
            sys::print(b"AriInit: USB controller ");
            print_bdf(info);
            sys::print(b" has no driver (interface 0x");
            print_hex8(prog_if);
            sys::print(b", xHCI only)\n");
            continue;
        }
        if inputs == input_eps.len() {
            break;
        }
        let Ok(ep) = sys::endpoint_create() else {
            return 21;
        };
        // Each xHCI also gets a block endpoint, which it serves if it finds
        // USB storage (ADR 0023); init uses the first one if there is no
        // virtio-blk.
        let Ok(block_ep) = sys::endpoint_create() else {
            return 21;
        };
        let block = Some(block_ep);
        let launched = sup
            .register(
                "usb_input",
                Kind::Driver {
                    pci_index: index,
                    with_system: true,
                    block,
                },
                Probe::Input,
                ep,
                true,
                Policy::DRIVER,
            )
            .is_some_and(|i| sup.launch(i));
        if launched {
            say(b"usb_input input ready");
            input_eps[inputs] = ep;
            inputs += 1;
            if usb_block.is_none() {
                usb_block = block;
            }
        } else {
            say(b"usb_input never became ready; dropped");
        }
    }
    if inputs > 0 {
        services |= SERVICE_INPUT;
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
        say(b"no input devices");
    }
    // 9b. No virtio-blk: USB storage, if usbd found it, is the block service
    //     and fsd runs on top of it (ADR 0023).
    if block_service.is_none() {
        if let Some(ep) = usb_block {
            match BlockClient::new(ep, HARDWARE, 1).and_then(|b| b.capacity()) {
                Ok(capacity) if capacity > 0 => {
                    sys::print(b"AriInit: block service via USB (");
                    sys::print(aricode_rt::fmt_u64(capacity, &mut buf));
                    sys::print(b" sectors)\n");
                    services |= SERVICE_BLOCK;
                    block_service = Some(ep);
                    match start_storage_stack(&mut sup, ep) {
                        Ok(fs_ep) => {
                            services |= SERVICE_FS;
                            fs_service = Some(fs_ep);
                        }
                        Err(code) => return code,
                    }
                }
                _ => say(b"no USB storage"),
            }
        }
    }
    // 10. Graphical interface: compositor with the framebuffer and the inputs,
    //     and two applications drawing at the same time.
    let mut compositor = None;
    if arg & sys::abi::init_arg::GUI != 0 {
        match run_gui(&input_eps[..inputs]) {
            Ok(ep) => {
                services |= SERVICE_GUI;
                compositor = Some(ep);
                say(b"gui ok (2 applications)");
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
        let Some(fs_ep) = fs_service else {
            return 40;
        };
        let Ok(mut fs) = FsClient::new(fs_ep, HARDWARE) else {
            return 40;
        };
        if let Err(code) = restart_storm(&mut sup, &mut fs) {
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
    if arg & sys::abi::init_arg::SHELL != 0 {
        let Some(fs) = fs_service else {
            say(b"no file service, not launching the shell");
            return 15;
        };
        let Ok(mut broker) = Broker::new(
            fs_service,
            block_service,
            net_service,
            compositor,
            &input_eps[..inputs],
        ) else {
            return 26;
        };
        // Stage 2 (ADR 0013): the user-space keyboards feed the shell console
        // as long as there is no compositor (with the GUI the events belong
        // to it, and each input service accepts a single reader).
        if inputs > 0 && compositor.is_none() {
            let mut ins = [Handle(0); MAX_INPUTS];
            ins[..inputs].copy_from_slice(&input_eps[..inputs]);
            let launched = sup
                .register(
                    "console_bridge",
                    Kind::ConsoleBridge {
                        inputs: ins,
                        count: inputs,
                    },
                    Probe::None,
                    Handle(0),
                    true,
                    Policy::DRIVER,
                )
                .is_some_and(|i| sup.launch(i));
            if launched {
                // The announcement goes here and not in the bridge: init
                // prints before the shell exists, so nothing interleaves with
                // its first prompt. The input services buffer whatever is
                // typed until the bridge reads it.
                sys::print(b"AriInit: console bridge with ");
                sys::print(aricode_rt::fmt_u64(inputs as u64, &mut buf));
                sys::print(b" input device(s)\nCONSOLE_BRIDGE_READY\n");
            } else {
                say(b"console bridge not started: the shell only reads COM1 and PS/2");
            }
        }
        // SSH (ADR 0025): with a network service, sshd serves remote shells
        // that init launches on its behalf.
        let mut sessions = None;
        if services & SERVICE_NET != 0 {
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
                    });
                } else {
                    say(b"sshd did not start");
                }
            }
        }
        match run_shell(
            fs,
            net_service,
            services,
            &mut broker,
            &mut sup,
            sessions.as_mut(),
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
