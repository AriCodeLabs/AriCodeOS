// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! AriCodeOS syscall ABI (section 22 of the master document).
//!
//! x86-64 convention: number in `rax`, arguments in `rdi, rsi, rdx, r10, r8,
//! r9`, result in `rax`. `rcx` and `r11` are clobbered by the `syscall`
//! instruction; all other registers are preserved. A negative result in
//! the range `-4095..=-1` is an [`Error`].
//!
//! Resources are named by handles (section 24): opaque 32-bit integers
//! with associated rights; `0` is never valid. Every process starts with
//! the handles [`handle::SELF_PROCESS`], [`handle::MAIN_THREAD`] and
//! [`handle::CONSOLE`].
//!
//! During `0.x` the ABI may change (section 74). Current version: 5
//! (ADR 0038: `mmio_unmap`; a mapping keeps its DMA buffer alive). Version
//! 4 added `Error::PeerClosed`, `endpoint_create_pair` and `endpoint_close`
//! (ADR 0030).

#![no_std]

pub const ABI_VERSION: u32 = 5;

/// Kernel base signature (SHA-512), required to check and verify the origin
/// and authorship of an AriCodeOS build. It is part of the fixed ABI: it is
/// frozen, never changes between versions, and the ABI tests and the system
/// that reports it depend on its exact value. Do not remove or modify it.
pub const KERNEL_BASE_SIGNATURE: &str =
    "8f404d2264f6e48a55d552bd1139211707c3dbd502cf918ff1338299940d075a6679eddd5dc069a384607c0cc822ca121d913c6e970a3902edf499e2d30e31ff";

/// Initial handles of every process.
pub mod handle {
    /// Handle through which `init` receives the `SPAWNER` object (`MANAGE`).
    pub const INIT_SPAWNER: u32 = 0x0001_0004;
    /// Handle through which `init` receives the PCI root (`READ|MANAGE`).
    pub const INIT_PCI_ROOT: u32 = 0x0001_0005;
    /// The process itself (`MANAGE | READ | DUPLICATE`).
    pub const SELF_PROCESS: u32 = 0x0001_0001;
    /// The main thread (`READ | WAIT | DUPLICATE`).
    pub const MAIN_THREAD: u32 = 0x0001_0002;
    /// Kernel console (`WRITE | DUPLICATE`).
    pub const CONSOLE: u32 = 0x0001_0003;
    /// Handle through which `init` receives the hardware capability (`MANAGE`).
    pub const INIT_HARDWARE: u32 = 0x0001_0006;
    /// Handle through which `init` receives the `SYSTEM` object (`READ|MANAGE`).
    pub const INIT_SYSTEM: u32 = 0x0001_0007;
    /// Handle through which `init` receives the console input (`READ`).
    pub const INIT_CONSOLE_IN: u32 = 0x0001_0008;
    /// Handle through which `init` receives the framebuffer (`MMIO`, `READ|WRITE`).
    pub const INIT_FRAMEBUFFER: u32 = 0x0001_0009;
}

/// Bits of the `arg` the kernel passes to `init`.
pub mod init_arg {
    /// The cmdline asks for an interactive shell (`shell`).
    pub const SHELL: u64 = 1;
    /// The cmdline asks for the graphical interface (`gui`): the kernel does not
    /// draw on the framebuffer and init launches the compositor.
    pub const GUI: u64 = 2;
    /// The system booted from slot A (or B) of a system partition.
    pub const BOOT_SLOT_A: u64 = 4;
    pub const BOOT_SLOT_B: u64 = 8;
    /// The booted slot awaits confirmation: init must mark it active.
    pub const BOOT_PENDING: u64 = 16;
    /// Tests only (`restart_storm`): init restarts a test service 100 times
    /// under the supervisor and checks that no resources are leaked.
    pub const RESTART_STORM: u64 = 64;
    /// Tests only (`service_hang`): init launches a service that never answers
    /// and checks that the supervisor kills it and leaves no live processes.
    pub const SERVICE_HANG: u64 = 128;
    /// Tests only (`init_fail_late`): init fails at the end of startup, before
    /// confirming the slot, to check that the state stays pending.
    pub const FAIL_LATE: u64 = 32;
}

/// Bits of the `arg` init passes to a PCI driver (`start_driver`), above
/// the BAR (bits 0..5) and IRQ (bit 15) bits.
pub mod driver_arg {
    /// The driver is relaunched after serving the system disk (ADR 0031):
    /// bits 17..63 carry the first 47 bits of that disk's GPT disk GUID,
    /// and the driver serves no other disk.
    pub const EXPECTED_DISK: u64 = 1 << 16;
    const DIGEST_BITS: u32 = 47;

    /// The first 47 bits of a GPT disk GUID.
    pub const fn digest(guid: &[u8; 16]) -> u64 {
        u64::from_le_bytes([
            guid[0], guid[1], guid[2], guid[3], guid[4], guid[5], guid[6], guid[7],
        ]) & ((1 << DIGEST_BITS) - 1)
    }

    /// The bits that ask a relaunched driver for the disk `guid`.
    pub const fn expected_disk(guid: &[u8; 16]) -> u64 {
        EXPECTED_DISK | digest(guid) << 17
    }

    /// The expected disk digest in `arg`, if any.
    pub const fn expected_digest(arg: u64) -> Option<u64> {
        if arg & EXPECTED_DISK != 0 {
            Some(arg >> 17)
        } else {
            None
        }
    }
}

/// Rights (bits) as seen by user space.
pub mod rights {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const DUPLICATE: u32 = 1 << 2;
    pub const TRANSFER: u32 = 1 << 3;
    pub const WAIT: u32 = 1 << 4;
    pub const SIGNAL: u32 = 1 << 5;
    pub const MANAGE: u32 = 1 << 6;
}

/// Object kinds returned by `object_kind`.
pub mod kind {
    pub const THREAD: u64 = 1;
    pub const PROCESS: u64 = 2;
    pub const CONSOLE: u64 = 3;
    pub const ENDPOINT: u64 = 4;
    /// Capability to create processes from the initrd (only `init` receives it).
    pub const SPAWNER: u64 = 5;
    /// Interrupt line of a device.
    pub const IRQ: u64 = 6;
    /// MMIO region of a device.
    pub const MMIO: u64 = 7;
    /// DMA buffer (contiguous physical memory).
    pub const DMA: u64 = 8;
    /// Capability to allocate hardware resources (DMA buffers).
    pub const HARDWARE: u64 = 9;
    /// PCI root: device enumeration and claiming (`init` only).
    pub const PCI_ROOT: u64 = 10;
    /// System state (`READ`: information; `MANAGE`: power off, reboot).
    pub const SYSTEM: u64 = 11;
}

/// Exclusive limit of user space (canonical lower half).
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

/// Syscall numbers.
pub mod nr {
    /// `process_exit(code)`: never returns.
    pub const PROCESS_EXIT: u64 = 0;
    /// `console_write(handle, ptr, len) -> bytes written`; requires `WRITE`.
    pub const CONSOLE_WRITE: u64 = 1;
    /// `thread_yield()`.
    pub const THREAD_YIELD: u64 = 2;
    /// `thread_sleep(ms)`.
    pub const THREAD_SLEEP: u64 = 3;
    /// `thread_get_tid() -> tid`.
    pub const THREAD_GET_TID: u64 = 4;
    /// `abi_version() -> ABI_VERSION`.
    pub const ABI_VERSION: u64 = 5;
    /// `handle_close(handle)`.
    pub const HANDLE_CLOSE: u64 = 6;
    /// `handle_duplicate(handle, rights) -> handle`; requires `DUPLICATE` and
    /// `rights` a subset of the source's.
    pub const HANDLE_DUPLICATE: u64 = 7;
    /// `object_kind(handle) -> kind::*`.
    pub const OBJECT_KIND: u64 = 8;
    /// `object_id(handle) -> tid | pid`; requires `READ`. The console returns 0.
    pub const OBJECT_ID: u64 = 9;
    /// `object_revoke(handle)`: invalidates the object for all its handles,
    /// in every process; requires `MANAGE`. Revoking an endpoint also closes
    /// both of its sides: queued messages are discarded and every thread
    /// waiting on it wakes with [`Error::PeerClosed`] (ADR 0030).
    pub const OBJECT_REVOKE: u64 = 10;
    /// `endpoint_create() -> handle` with `READ|WRITE|DUPLICATE|TRANSFER|MANAGE`.
    pub const ENDPOINT_CREATE: u64 = 11;
    /// `endpoint_create_pair() -> receive | send << 32`: a new endpoint with
    /// its two sides in separate handles, the receive one with
    /// `READ|DUPLICATE|TRANSFER|MANAGE` and the send one with
    /// `WRITE|DUPLICATE|TRANSFER` (ADR 0030). A reply endpoint made this way
    /// reports [`Error::PeerClosed`] to its receiver once every send handle
    /// is gone, instead of letting it wait for its deadline.
    pub const ENDPOINT_CREATE_PAIR: u64 = 34;
    /// `endpoint_close(ep)`: requires `MANAGE` on the endpoint. Closes both
    /// of its sides for every holder (ADR 0030): queued messages are
    /// discarded, and sends and receives, waiting or not, fail with
    /// [`Error::PeerClosed`] from then on. Unlike `object_revoke` the
    /// handles stay valid. For a supervisor that gives up on a service
    /// whose endpoint it keeps.
    pub const ENDPOINT_CLOSE: u64 = 35;
    /// `ipc_send(ep, data_ptr, data_len, handles_ptr, handles_len, timeout_ms)`;
    /// requires `WRITE` on `ep` and `TRANSFER` on each handle sent (which
    /// is removed from the sender). `timeout_ms`: 0 = do not block,
    /// [`TIMEOUT_INFINITE`] = wait forever. [`Error::PeerClosed`] once no
    /// `READ` handle on the endpoint exists any more (ADR 0030), also for a
    /// sender already waiting for room in the queue. A handle listed twice
    /// is `Invalid` (nothing is transferred).
    pub const IPC_SEND: u64 = 12;
    /// `ipc_receive(ep, buf_ptr, buf_len, handles_out_ptr, handles_max, timeout_ms)
    /// -> data_len | (handles_received << 32)`; requires `READ` on `ep`.
    /// Queued messages are delivered first; then [`Error::PeerClosed`] once
    /// no `WRITE` handle on the endpoint exists any more (ADR 0030).
    pub const IPC_RECEIVE: u64 = 13;
    /// `process_spawn(spawner, name_ptr, name_len, arg, handles_ptr, handles_len)
    /// -> process handle`; requires `MANAGE` on a `SPAWNER` object and
    /// `TRANSFER` on each handle, which is removed from the parent and appears in
    /// the child from `0x0001_0004` on, in the same order. The child receives `arg`
    /// in `rdi`. A handle listed twice is `Invalid` (nothing is transferred).
    pub const PROCESS_SPAWN: u64 = 14;
    /// `process_wait(process, timeout_ms) -> exit code`; requires `WAIT`.
    /// A process terminated by the kernel returns `Error::Killed`.
    pub const PROCESS_WAIT: u64 = 15;
    /// `irq_wait(irq, timeout_ms) -> accumulated interrupts`; requires `WAIT`.
    pub const IRQ_WAIT: u64 = 16;
    /// `mmio_map(region) -> virtual address`; requires `READ|WRITE` on an
    /// MMIO or DMA object. Mapping a region the process already maps
    /// returns the existing mapping. A DMA buffer stays allocated while any
    /// process maps it, even after every handle to it is closed (ADR 0038);
    /// with a memory quota, the DMA bytes a process keeps mapped count
    /// against it (`Limit` beyond).
    pub const MMIO_MAP: u64 = 17;
    /// `mmio_unmap(virt)`: removes the mapping `mmio_map` returned at
    /// `virt` (`Invalid` if there is none); a DMA buffer nobody else holds
    /// is freed (ADR 0038). Servers that keep client buffers mapped after
    /// closing their handles release them with this.
    pub const MMIO_UNMAP: u64 = 36;
    /// `region_len(region) -> bytes`; requires `READ`.
    pub const REGION_LEN: u64 = 18;
    /// `dma_alloc(hardware, len) -> DMA handle`; requires `MANAGE` on a
    /// `HARDWARE` object. Contiguous physical memory, at most `DMA_MAX_LEN`.
    pub const DMA_ALLOC: u64 = 19;
    /// `dma_phys(dma) -> physical address`; requires `READ`.
    pub const DMA_PHYS: u64 = 20;
    /// `pci_count(root) -> enumerated functions`; requires `READ` on `PCI_ROOT`.
    pub const PCI_COUNT: u64 = 21;
    /// `pci_info(root, index) -> packed PciInfo` (see [`pci_info`]); requires `READ`.
    pub const PCI_INFO: u64 = 22;
    /// `pci_claim(root, index, out_ptr) -> handles written`; requires `MANAGE`.
    /// Writes [`PCI_CLAIM_HANDLES`] `u32`s to `out_ptr`: configuration
    /// space (MMIO), `HARDWARE`, MSI-X IRQ (0 if none) and the six memory
    /// BARs (0 if absent or I/O). The device is left with memory and bus
    /// master enabled and MSI-X on entry 0.
    pub const PCI_CLAIM: u64 = 23;
    /// `console_read(console, ptr, len, timeout_ms) -> bytes read (>= 1)`;
    /// requires `READ`. Blocks until there is input or the deadline expires (`Timeout`).
    pub const CONSOLE_READ: u64 = 24;
    /// `system_info(system, kind, ptr, len) -> bytes written` (see [`sysinfo`]);
    /// requires `READ`.
    pub const SYSTEM_INFO: u64 = 25;
    /// `system_power(system, action)` (see [`power`]); requires `MANAGE`. Does not return.
    pub const SYSTEM_POWER: u64 = 26;
    /// `initrd_read(spawner, name_ptr, name_len, buf, len) -> bytes copied`;
    /// requires `READ` on the `SPAWNER`. `TooBig` if the file does not fit.
    pub const INITRD_READ: u64 = 27;
    /// `pci_info_ext(root, index) -> prog_if | revision << 8 | header_type << 16`
    /// (see `pci_info::prog_if`); requires `READ` on the PCI root.
    pub const PCI_INFO_EXT: u64 = 28;
    /// `console_inject(console, ptr, len) -> bytes accepted`; requires `MANAGE`
    /// on a `Console`. Writes text from a user-space keyboard into the console
    /// input ring (console bridge, ADR 0013); bytes that do not fit are
    /// dropped and counted.
    pub const CONSOLE_INJECT: u64 = 29;
    /// `event_record(code, name_ptr, name_len)`: a `Fault` event in the
    /// kernel ring for the calling process (ADR 0028).
    pub const EVENT_RECORD: u64 = 33;
    /// `process_spawn_from(spawner, req_ptr, arg, handles_ptr, limits) -> handle`;
    /// like `PROCESS_SPAWN` with the ELF in a caller DMA buffer described
    /// by [`spawn_from::Request`] (programs installed outside the initrd,
    /// ADR 0017). Requires `MANAGE` on the `SPAWNER` and `READ` on the buffer.
    pub const PROCESS_SPAWN_FROM: u64 = 30;
    /// `ipc_receive_any(handles_ptr, count, buf_ptr, buf_len, out_ptr, timeout_ms)
    /// -> len | n << 32 | index << 48`: receives from the first of up to
    /// `IPC_MAX_ANY` endpoints (`READ` on all) that has a message; `out_ptr`
    /// holds `IPC_MAX_HANDLES` handles (ADR 0020). [`Error::PeerClosed`]
    /// only when every endpoint is empty and has lost its send side.
    pub const IPC_RECEIVE_ANY: u64 = 31;
    /// `process_kill(process)`: requires `MANAGE` on the process. Terminates it
    /// as if it had faulted (`Killed`, reason "terminated by the
    /// supervisor"): its thread exits as soon as it returns to ring 0 or wakes
    /// from a wait. `process_wait` returns `Killed` (audit 6ee19fb, 4).
    pub const PROCESS_KILL: u64 = 32;
}

/// `process_spawn_from` request (48 bytes in caller memory).
pub mod spawn_from {
    pub const LEN: usize = 48;
    pub const MAX_NAME: usize = 32;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Request {
        /// Handle of the DMA buffer containing the ELF (`READ`).
        pub dma: u32,
        /// Offset of the ELF within the buffer.
        pub elf_offset: u32,
        /// ELF size in bytes.
        pub elf_len: u32,
        pub name: [u8; MAX_NAME],
        pub name_len: u8,
    }

    impl Request {
        /// `None` if the name is empty or does not fit.
        pub fn new(dma: u32, elf_offset: u32, elf_len: u32, name: &str) -> Option<Self> {
            if name.is_empty() || name.len() > MAX_NAME {
                return None;
            }
            let mut n = [0u8; MAX_NAME];
            n[..name.len()].copy_from_slice(name.as_bytes());
            Some(Self {
                dma,
                elf_offset,
                elf_len,
                name: n,
                name_len: name.len() as u8,
            })
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; LEN] {
            let mut b = [0u8; LEN];
            b[0..4].copy_from_slice(&self.dma.to_le_bytes());
            b[4..8].copy_from_slice(&self.elf_offset.to_le_bytes());
            b[8..12].copy_from_slice(&self.elf_len.to_le_bytes());
            b[12] = self.name_len;
            b[16..48].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < LEN || b[12] as usize > MAX_NAME {
                return None;
            }
            let mut name = [0u8; MAX_NAME];
            name.copy_from_slice(&b[16..48]);
            Some(Self {
                dma: u32::from_le_bytes(b[0..4].try_into().ok()?),
                elf_offset: u32::from_le_bytes(b[4..8].try_into().ok()?),
                elf_len: u32::from_le_bytes(b[8..12].try_into().ok()?),
                name,
                name_len: b[12],
            })
        }
    }
}

/// Sixth argument of `process_spawn`: number of handles (bits 0..7) and
/// DMA memory limit of the child in KiB (bits 8..). 0 KiB = unlimited.
pub mod spawn_limits {
    pub const fn pack(handles: usize, memory_kib: u32) -> u64 {
        (handles as u64 & 0xFF) | (memory_kib as u64) << 8
    }
    pub const fn handles(raw: u64) -> usize {
        (raw & 0xFF) as usize
    }
    pub const fn memory_kib(raw: u64) -> u32 {
        (raw >> 8) as u32
    }
}

/// `system_info` queries and the format of their replies.
pub mod sysinfo {
    /// One `u64`: milliseconds since boot.
    pub const UPTIME: u64 = 0;
    /// Two `u64`s: usable RAM frames and free frames.
    pub const MEMORY: u64 = 1;
    /// As many [`ProcessEntry`] as fit in the buffer.
    pub const PROCESSES: u64 = 2;
    /// One [`Framebuffer`] (40 bytes): geometry and format of the GOP screen.
    pub const FRAMEBUFFER: u64 = 3;
    /// Up to `RANDOM_MAX` random bytes from the kernel generator (the
    /// requested length is the buffer's).
    pub const RANDOM: u64 = 4;
    pub const RANDOM_MAX: u64 = 256;
    /// One `u64`: online CPUs (Phase 32).
    pub const CPUS: u64 = 5;
    /// Three `u64`s: active IOMMU units, live domains and recorded DMA
    /// faults (Phase 33).
    pub const IOMMU: u64 = 6;
    /// Per-CPU idle time (stage 5): `u64` TSC cycles since boot, `u64`
    /// online CPUs and, for each one, `u64` cycles its idle thread has been
    /// its running thread (interrupts serviced meanwhile included; another
    /// thread's time never), all sampled at the same TSC instant, so each
    /// is at most the first value (incident 0020).
    pub const IDLE: u64 = 7;
    /// Recent kernel events (stage 5): [`EventEntry`] entries from oldest
    /// to newest, as many as fit in the buffer.
    pub const EVENTS: u64 = 8;
    /// One `u64`: microseconds since boot (calibrated TSC; without
    /// calibration, the ms tick times 1000).
    pub const UPTIME_US: u64 = 9;
    /// One `u64`: Unix seconds from the CMOS real-time clock (read at boot,
    /// advanced with the uptime); 0 if the RTC could not be read.
    pub const WALLCLOCK: u64 = 10;
    /// The kernel command line as given by the loader (UTF-8 bytes): any
    /// program can read boot flags such as `strict` (ADR 0028).
    pub const CMDLINE: u64 = 11;
    /// One [`RandomSource`] (32 bytes): what seeded the kernel generator
    /// that `RANDOM` reads, and how much has been mixed into it since
    /// (ADR 0039). Only the source and counters: never generator state,
    /// seed or pool material. Later fields are only appended after
    /// [`RandomSource::LEN`]: readers pass a larger buffer and decode the
    /// first `LEN` bytes.
    pub const RANDOM_SOURCE: u64 = 12;

    /// What seeded the kernel generator. The number is the code of the
    /// `net tcp secret weak` and `ssh entropy weak` fault events.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EntropySource {
        /// The CPU's `rdrand` returned data for the seed (CPUID.1:ECX.30).
        Rdrand = 1,
        /// No hardware source: cycle-counter samples and the timing of
        /// device, keyboard and serial interrupts only. Weak: an observer
        /// who knows the boot timing can narrow the generator's state.
        TscInterrupts = 2,
    }

    impl EntropySource {
        pub fn from_u8(v: u8) -> Option<Self> {
            match v {
                1 => Some(Self::Rdrand),
                2 => Some(Self::TscInterrupts),
                _ => None,
            }
        }

        /// A hardware random source seeded the generator.
        pub fn is_hardware(self) -> bool {
            matches!(self, Self::Rdrand)
        }

        /// Short name for console and `diag` lines.
        pub fn label(self) -> &'static str {
            match self {
                Self::Rdrand => "rdrand",
                Self::TscInterrupts => "TSC + interrupts",
            }
        }
    }

    /// Reply of `RANDOM_SOURCE` (32 bytes): the seed's source and the
    /// inputs mixed into the generator since it was seeded, seed included.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RandomSource {
        pub source: EntropySource,
        /// `rdrand` values mixed in (the seed's and one per read since).
        pub hardware_samples: u64,
        /// Cycle-counter samples mixed in (64 in the seed, one per read).
        pub tsc_samples: u64,
        /// Interrupts whose timing reached the generator through the pool
        /// (counted up to the last read).
        pub interrupt_events: u64,
    }

    impl RandomSource {
        pub const LEN: usize = 32;
        /// Room for the longest [`RandomSource::summary`].
        pub const SUMMARY_MAX: usize = 80;

        /// `rdrand`, or `TSC + interrupts (N samples, M events)`: the
        /// wording netd, sshd and `diag` share.
        pub fn summary<'a>(&self, buf: &'a mut [u8; Self::SUMMARY_MAX]) -> &'a str {
            struct Cursor<'b> {
                buf: &'b mut [u8],
                len: usize,
            }
            impl core::fmt::Write for Cursor<'_> {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    let end = self.len + s.len();
                    self.buf
                        .get_mut(self.len..end)
                        .ok_or(core::fmt::Error)?
                        .copy_from_slice(s.as_bytes());
                    self.len = end;
                    Ok(())
                }
            }
            let mut c = Cursor { buf, len: 0 };
            let _ = if self.source.is_hardware() {
                core::fmt::Write::write_str(&mut c, self.source.label())
            } else {
                core::fmt::write(
                    &mut c,
                    format_args!(
                        "{} ({} samples, {} events)",
                        self.source.label(),
                        self.tsc_samples,
                        self.interrupt_events
                    ),
                )
            };
            let len = c.len;
            // Only whole `&str`s were copied in.
            core::str::from_utf8(&c.buf[..len]).unwrap_or("")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0] = self.source as u8;
            b[8..16].copy_from_slice(&self.hardware_samples.to_le_bytes());
            b[16..24].copy_from_slice(&self.tsc_samples.to_le_bytes());
            b[24..32].copy_from_slice(&self.interrupt_events.to_le_bytes());
            b
        }

        /// `None` for a short reply or a source this build does not know.
        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            Some(Self {
                source: EntropySource::from_u8(b[0])?,
                hardware_samples: u64::from_le_bytes(b[8..16].try_into().ok()?),
                tsc_samples: u64::from_le_bytes(b[16..24].try_into().ok()?),
                interrupt_events: u64::from_le_bytes(b[24..32].try_into().ok()?),
            })
        }
    }

    /// Event class of the kernel ring.
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EventKind {
        None = 0,
        /// `pid`, `name`.
        ProcessCreated = 1,
        /// `pid`, `name`, `code` = exit code.
        ProcessExited = 2,
        /// `pid`, `name` = kernel reason (exception).
        ProcessKilled = 3,
        /// `code` = `bus << 8 | dev << 3 | fn` of the device, `name` = reason.
        DmaFault = 4,
        /// `pid`, `name`, `code` = bytes requested above the quota.
        DmaQuota = 5,
        /// Reported by a program with `event_record` (ADR 0028): `pid`,
        /// `name` = what failed, `code` = its reason code.
        Fault = 6,
    }

    impl EventKind {
        pub fn from_u8(v: u8) -> Self {
            match v {
                1 => Self::ProcessCreated,
                2 => Self::ProcessExited,
                3 => Self::ProcessKilled,
                4 => Self::DmaFault,
                5 => Self::DmaQuota,
                6 => Self::Fault,
                _ => Self::None,
            }
        }
    }

    pub const EVENT_NAME_LEN: usize = 24;

    /// Kernel event (48 bytes): timestamp in us, class, pid, code and
    /// name or reason.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EventEntry {
        pub at_us: u64,
        pub kind: EventKind,
        pub pid: u32,
        pub code: u64,
        pub name: [u8; EVENT_NAME_LEN],
        pub name_len: u8,
    }

    impl EventEntry {
        pub const LEN: usize = 48;
        pub const EMPTY: EventEntry = EventEntry {
            at_us: 0,
            kind: EventKind::None,
            pid: 0,
            code: 0,
            name: [0; EVENT_NAME_LEN],
            name_len: 0,
        };

        pub fn new(at_us: u64, kind: EventKind, pid: u32, code: u64, name: &str) -> Self {
            let n = name.len().min(EVENT_NAME_LEN);
            let mut buf = [0u8; EVENT_NAME_LEN];
            buf[..n].copy_from_slice(&name.as_bytes()[..n]);
            Self {
                at_us,
                kind,
                pid,
                code,
                name: buf,
                name_len: n as u8,
            }
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[..8].copy_from_slice(&self.at_us.to_le_bytes());
            b[8] = self.kind as u8;
            b[9] = self.name_len;
            b[12..16].copy_from_slice(&self.pid.to_le_bytes());
            b[16..24].copy_from_slice(&self.code.to_le_bytes());
            b[24..].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            let mut name = [0u8; EVENT_NAME_LEN];
            name.copy_from_slice(&b[24..48]);
            Some(Self {
                at_us: u64::from_le_bytes(b[..8].try_into().ok()?),
                kind: EventKind::from_u8(b[8]),
                pid: u32::from_le_bytes(b[12..16].try_into().ok()?),
                code: u64::from_le_bytes(b[16..24].try_into().ok()?),
                name,
                name_len: b[9].min(EVENT_NAME_LEN as u8),
            })
        }
    }

    /// Framebuffer pixel format (32 bpp).
    pub const FB_FORMAT_RGB: u8 = 0;
    pub const FB_FORMAT_BGR: u8 = 1;
    pub const FB_FORMAT_BITMASK: u8 = 2;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Framebuffer {
        pub width: u32,
        pub height: u32,
        /// Pixels per row.
        pub stride: u32,
        pub format: u8,
        pub size: u64,
        pub red_mask: u32,
        pub green_mask: u32,
        pub blue_mask: u32,
    }

    impl Framebuffer {
        pub const LEN: usize = 40;

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[0..4].copy_from_slice(&self.width.to_le_bytes());
            b[4..8].copy_from_slice(&self.height.to_le_bytes());
            b[8..12].copy_from_slice(&self.stride.to_le_bytes());
            b[12] = self.format;
            b[16..24].copy_from_slice(&self.size.to_le_bytes());
            b[24..28].copy_from_slice(&self.red_mask.to_le_bytes());
            b[28..32].copy_from_slice(&self.green_mask.to_le_bytes());
            b[32..36].copy_from_slice(&self.blue_mask.to_le_bytes());
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            Some(Self {
                width: u32::from_le_bytes(b[0..4].try_into().ok()?),
                height: u32::from_le_bytes(b[4..8].try_into().ok()?),
                stride: u32::from_le_bytes(b[8..12].try_into().ok()?),
                format: b[12],
                size: u64::from_le_bytes(b[16..24].try_into().ok()?),
                red_mask: u32::from_le_bytes(b[24..28].try_into().ok()?),
                green_mask: u32::from_le_bytes(b[28..32].try_into().ok()?),
                blue_mask: u32::from_le_bytes(b[32..36].try_into().ok()?),
            })
        }
    }

    pub const PROCESS_NAME_LEN: usize = 24;

    /// State of a process in [`ProcessEntry`].
    #[repr(u8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProcessState {
        Running = 0,
        Exited = 1,
        Killed = 2,
    }

    /// Process list entry (32 bytes): pid, state, exit code
    /// (if it finished) and name.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ProcessEntry {
        pub pid: u32,
        pub state: ProcessState,
        pub exit_code: u16,
        pub name: [u8; PROCESS_NAME_LEN],
        pub name_len: u8,
    }

    impl ProcessEntry {
        pub const LEN: usize = 32;

        pub fn new(pid: u32, state: ProcessState, exit_code: u16, name: &str) -> Self {
            let n = name.len().min(PROCESS_NAME_LEN);
            let mut buf = [0u8; PROCESS_NAME_LEN];
            buf[..n].copy_from_slice(&name.as_bytes()[..n]);
            Self {
                pid,
                state,
                exit_code,
                name: buf,
                name_len: n as u8,
            }
        }

        pub fn name_str(&self) -> &str {
            core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
        }

        pub fn encode(&self) -> [u8; Self::LEN] {
            let mut b = [0u8; Self::LEN];
            b[..4].copy_from_slice(&self.pid.to_le_bytes());
            b[4] = self.state as u8;
            b[5] = self.name_len;
            b[6..8].copy_from_slice(&self.exit_code.to_le_bytes());
            b[8..].copy_from_slice(&self.name);
            b
        }

        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.len() < Self::LEN {
                return None;
            }
            let state = match b[4] {
                0 => ProcessState::Running,
                1 => ProcessState::Exited,
                2 => ProcessState::Killed,
                _ => return None,
            };
            let mut name = [0u8; PROCESS_NAME_LEN];
            name.copy_from_slice(&b[8..Self::LEN]);
            Some(Self {
                pid: u32::from_le_bytes(b[..4].try_into().ok()?),
                state,
                exit_code: u16::from_le_bytes(b[6..8].try_into().ok()?),
                name,
                name_len: b[5].min(PROCESS_NAME_LEN as u8),
            })
        }
    }
}

/// `system_power` actions.
pub mod power {
    pub const SHUTDOWN: u64 = 0;
    pub const REBOOT: u64 = 1;
}

/// Number of handles written by `pci_claim`.
pub const PCI_CLAIM_HANDLES: usize = 9;

/// `pci_info` packing: `vendor << 48 | device << 32 | class << 24 |
/// subclass << 16 | bus << 8 | dev << 3 | func`.
pub mod pci_info {
    pub const fn pack(
        vendor: u16,
        device: u16,
        class: u8,
        subclass: u8,
        bus: u8,
        dev: u8,
        func: u8,
    ) -> u64 {
        (vendor as u64) << 48
            | (device as u64) << 32
            | (class as u64) << 24
            | (subclass as u64) << 16
            | (bus as u64) << 8
            | (dev as u64) << 3
            | func as u64
    }
    pub const fn vendor(v: u64) -> u16 {
        (v >> 48) as u16
    }
    pub const fn device(v: u64) -> u16 {
        (v >> 32) as u16
    }
    pub const fn class(v: u64) -> u8 {
        (v >> 24) as u8
    }
    pub const fn subclass(v: u64) -> u8 {
        (v >> 16) as u8
    }
    pub const fn bus(v: u64) -> u8 {
        (v >> 8) as u8
    }
    pub const fn dev(v: u64) -> u8 {
        ((v >> 3) & 0x1F) as u8
    }
    pub const fn func(v: u64) -> u8 {
        (v & 7) as u8
    }

    /// `pci_info_ext` fields.
    pub const fn pack_ext(prog_if: u8, revision: u8, header_type: u8) -> u64 {
        prog_if as u64 | (revision as u64) << 8 | (header_type as u64) << 16
    }
    /// Programming Interface: tells apart variants of the same class (xHCI
    /// is `0c.03` with interface `0x30`; EHCI `0x20`, OHCI `0x10`, UHCI `0x00`).
    pub const fn prog_if(ext: u64) -> u8 {
        ext as u8
    }
    pub const fn revision(ext: u64) -> u8 {
        (ext >> 8) as u8
    }
    /// xHCI programming interface.
    pub const XHCI_PROG_IF: u8 = 0x30;
    /// EHCI programming interface (USB 2.0, ADR 0031).
    pub const EHCI_PROG_IF: u8 = 0x20;
}

/// Maximum DMA buffer size per call (section 52).
pub const DMA_MAX_LEN: u64 = 8 * 1024 * 1024;

/// IPC message limits (section 23: small, fast messages).
pub const IPC_MAX_DATA: usize = 256;
pub const IPC_MAX_HANDLES: usize = 4;
/// Endpoint queue capacity (protection against exhaustion).
pub const IPC_QUEUE_CAPACITY: usize = 16;
/// Endpoints per `ipc_receive_any` call.
pub const IPC_MAX_ANY: usize = 16;
/// Timeout meaning "wait indefinitely".
pub const TIMEOUT_INFINITE: u64 = u64::MAX;

/// Splits the result of `ipc_receive`.
pub const fn receive_result(raw: u64) -> (usize, usize) {
    ((raw & 0xFFFF_FFFF) as usize, (raw >> 32) as usize)
}

/// Errors returned by the kernel (negated in `rax`).
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Unknown syscall number.
    NoSys = 1,
    /// Pointer or range outside user space or not mapped.
    Fault = 2,
    /// Invalid argument.
    Invalid = 3,
    /// Nonexistent, closed or forged handle.
    BadHandle = 4,
    /// The handle lacks the required rights.
    Denied = 5,
    /// Attempt to obtain more rights than are held.
    Escalation = 6,
    /// Process resource limit.
    Limit = 7,
    /// The object was revoked: the handle exists but no longer grants access.
    Revoked = 8,
    /// The deadline expired (or `timeout_ms == 0` and the operation would block).
    Timeout = 9,
    /// Message or buffer above the limits.
    TooBig = 10,
    /// The awaited process was terminated by the kernel (exception in ring 3).
    Killed = 11,
    /// No file with that name exists in the initrd.
    NotFound = 12,
    /// The other side of the endpoint is gone (ADR 0030): a send finds no
    /// `READ` handle left (the server exited or closed it, or the endpoint
    /// was revoked), or a receive finds the queue empty and no `WRITE`
    /// handle left (for a reply endpoint: the server dropped the request
    /// without answering, typically because it died).
    PeerClosed = 13,
    /// SDK level, never returned by a syscall (`rpc::call`, ADR 0030): the
    /// service endpoint refused the request because nobody can receive on
    /// it any more (`PeerClosed` or `Revoked` on the send). Permanent: the
    /// caller must stop using that endpoint.
    ServiceGone = 14,
    /// SDK level, never returned by a syscall (`rpc::call`, ADR 0030): the
    /// request was accepted but dropped unanswered (the server died holding
    /// it, or it was discarded with a queue). Not permanent: a supervised
    /// service is relaunched on the same endpoint, so a later request may
    /// be served; retry after a short pause.
    ReplyLost = 15,
}

impl Error {
    /// For a raw syscall on an endpoint the caller holds (a server
    /// receiving on its own endpoint): the endpoint can never deliver again
    /// (ADR 0030), its other side being gone (`PeerClosed`) or the endpoint
    /// revoked. Retrying is pointless. Clients of `rpc::call` get the
    /// finer `ServiceGone` (permanent) and `ReplyLost` (retry) instead.
    pub const fn endpoint_gone(self) -> bool {
        matches!(self, Error::PeerClosed | Error::Revoked)
    }

    pub const fn to_raw(self) -> i64 {
        -(self as i64)
    }

    pub const fn from_raw(raw: i64) -> Option<Error> {
        match -raw {
            1 => Some(Error::NoSys),
            2 => Some(Error::Fault),
            3 => Some(Error::Invalid),
            4 => Some(Error::BadHandle),
            5 => Some(Error::Denied),
            6 => Some(Error::Escalation),
            7 => Some(Error::Limit),
            8 => Some(Error::Revoked),
            9 => Some(Error::Timeout),
            10 => Some(Error::TooBig),
            11 => Some(Error::Killed),
            12 => Some(Error::NotFound),
            13 => Some(Error::PeerClosed),
            14 => Some(Error::ServiceGone),
            15 => Some(Error::ReplyLost),
            _ => None,
        }
    }
}

/// Converts the returned `rax` into a `Result`.
pub const fn decode(raw: i64) -> Result<u64, Error> {
    if raw < 0 && raw >= -4095 {
        match Error::from_raw(raw) {
            Some(e) => Err(e),
            None => Err(Error::Invalid),
        }
    } else {
        Ok(raw as u64)
    }
}

/// `true` if `[ptr, ptr+len)` lies entirely within user space.
pub const fn user_range_ok(ptr: u64, len: u64) -> bool {
    match ptr.checked_add(len) {
        Some(end) => end <= USER_SPACE_END,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn kernel_base_signature_is_frozen() {
        let sig = super::KERNEL_BASE_SIGNATURE;
        assert_eq!(sig.len(), 128);
        assert!(sig
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(
            sig,
            concat!(
                "8f404d2264f6e48a55d552bd1139211707c3dbd502cf918ff1338299940d075a",
                "6679eddd5dc069a384607c0cc822ca121d913c6e970a3902edf499e2d30e31ff"
            )
        );
    }

    #[test]
    fn expected_disk_round_trip() {
        use super::driver_arg::*;
        let guid = [0xFFu8; 16];
        let arg = 0x3F | 1 << 15 | expected_disk(&guid);
        assert_eq!(arg & 0x3F, 0x3F, "BAR bits untouched");
        assert_ne!(arg & 1 << 15, 0, "IRQ bit untouched");
        assert_eq!(expected_digest(arg), Some(digest(&guid)));
        assert_eq!(digest(&guid), (1 << 47) - 1);
        let mut other = guid;
        other[0] = 0;
        assert_ne!(expected_digest(arg), Some(digest(&other)));
        assert_eq!(expected_digest(0x3F | 1 << 15), None);
    }

    #[test]
    fn spawn_limits_pack() {
        use super::spawn_limits::*;
        let raw = pack(6, 128);
        assert_eq!(handles(raw), 6);
        assert_eq!(memory_kib(raw), 128);
        assert_eq!(memory_kib(pack(3, 0)), 0);
    }

    #[test]
    fn framebuffer_info_round_trip() {
        use super::sysinfo::*;
        let f = Framebuffer {
            width: 1280,
            height: 800,
            stride: 1280,
            format: FB_FORMAT_BGR,
            size: 1280 * 800 * 4,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
        };
        assert_eq!(Framebuffer::decode(&f.encode()), Some(f));
        assert!(Framebuffer::decode(&[0u8; 10]).is_none());
    }

    #[test]
    fn random_source_round_trip() {
        use super::sysinfo::*;
        let r = RandomSource {
            source: EntropySource::TscInterrupts,
            hardware_samples: 0,
            tsc_samples: 64 + 7,
            interrupt_events: 1234,
        };
        let b = r.encode();
        assert_eq!(b.len(), RandomSource::LEN);
        assert_eq!(b[0], 2, "the source number is the fault code");
        assert!(b[1..8].iter().all(|&x| x == 0), "reserved bytes are zero");
        assert_eq!(RandomSource::decode(&b), Some(r));
        let hw = RandomSource {
            source: EntropySource::Rdrand,
            hardware_samples: 9,
            tsc_samples: 65,
            interrupt_events: u64::MAX,
        };
        assert_eq!(RandomSource::decode(&hw.encode()), Some(hw));
        assert!(hw.source.is_hardware() && !r.source.is_hardware());
        assert_eq!(hw.source.label(), "rdrand");
        assert_eq!(r.source.label(), "TSC + interrupts");
        let mut text = [0u8; RandomSource::SUMMARY_MAX];
        assert_eq!(
            r.summary(&mut text),
            "TSC + interrupts (71 samples, 1234 events)"
        );
        assert_eq!(hw.summary(&mut text), "rdrand");
        let widest = RandomSource {
            tsc_samples: u64::MAX,
            interrupt_events: u64::MAX,
            ..r
        };
        assert!(widest
            .summary(&mut text)
            .ends_with("18446744073709551615 events)"));
        assert!(RandomSource::decode(&b[..31]).is_none());
        for unknown in [0u8, 3, 0xFF] {
            let mut bad = b;
            bad[0] = unknown;
            assert!(RandomSource::decode(&bad).is_none(), "source {unknown}");
        }
    }

    #[test]
    fn process_entry_round_trip() {
        use super::sysinfo::*;
        let e = ProcessEntry::new(7, ProcessState::Exited, 42, "virtio_blk");
        let d = ProcessEntry::decode(&e.encode()).unwrap();
        assert_eq!(d, e);
        assert_eq!(d.name_str(), "virtio_blk");
        let long = ProcessEntry::new(
            1,
            ProcessState::Running,
            0,
            "a-name-far-too-long-for-the-entry",
        );
        assert_eq!(long.name_len as usize, PROCESS_NAME_LEN);
        let mut bad = e.encode();
        bad[4] = 9;
        assert!(ProcessEntry::decode(&bad).is_none());
        assert!(ProcessEntry::decode(&bad[..10]).is_none());
    }

    use super::*;

    #[test]
    fn errors_round_trip_and_decode() {
        assert_eq!(Error::Fault.to_raw(), -2);
        assert_eq!(Error::from_raw(-2), Some(Error::Fault));
        assert_eq!(Error::from_raw(-99), None);
        assert_eq!(Error::PeerClosed.to_raw(), -13);
        assert_eq!(decode(-13), Err(Error::PeerClosed));
        assert!(Error::PeerClosed.endpoint_gone() && Error::Revoked.endpoint_gone());
        assert!(!Error::Timeout.endpoint_gone());
        for e in [Error::ServiceGone, Error::ReplyLost] {
            assert_eq!(Error::from_raw(e.to_raw()), Some(e));
        }
        assert_eq!(decode(-1), Err(Error::NoSys));
        assert_eq!(
            decode(-4096),
            Ok((-4096i64) as u64),
            "outside the error range it is a value"
        );
        assert_eq!(decode(42), Ok(42));
        assert_eq!(
            decode(-77),
            Err(Error::Invalid),
            "unknown code is normalized"
        );
    }

    #[test]
    fn pci_info_round_trip() {
        let v = pci_info::pack(0x1AF4, 0x1042, 0x01, 0x00, 0, 3, 0);
        assert_eq!(pci_info::vendor(v), 0x1AF4);
        assert_eq!(pci_info::device(v), 0x1042);
        assert_eq!((pci_info::class(v), pci_info::subclass(v)), (1, 0));
        assert_eq!(
            (pci_info::bus(v), pci_info::dev(v), pci_info::func(v)),
            (0, 3, 0)
        );
    }

    #[test]
    fn receive_result_packs_len_and_handles() {
        assert_eq!(receive_result(4 | (2 << 32)), (4, 2));
        assert_eq!(receive_result(0), (0, 0));
    }

    #[test]
    fn user_range_checks() {
        assert!(user_range_ok(0x40_0000, 4096));
        assert!(user_range_ok(USER_SPACE_END - 16, 16));
        assert!(!user_range_ok(USER_SPACE_END - 16, 17));
        assert!(!user_range_ok(0xFFFF_FFFF_8000_0000, 1));
        assert!(!user_range_ok(u64::MAX, 1));
    }
}
