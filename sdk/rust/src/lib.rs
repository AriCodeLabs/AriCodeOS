// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! System calls from user space (first piece of the SDK, section 27). No
//! `alloc`, no `std`: just the `syscall` instruction and typed wrappers over
//! `aricode-abi`.

#![no_std]

pub mod block;
pub mod client_buffer;
pub mod fs;
pub mod http;
pub mod input;
pub mod launch;
pub mod net;
pub mod permissions;
pub mod rpc;
pub mod status;
pub mod time;
pub mod virtio_pci;
pub mod window;

pub use aricode_abi::{self as abi, handle, kind, rights, Error, TIMEOUT_INFINITE};
use aricode_abi::{decode, nr, receive_result};

/// User handle (opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(pub u32);

impl Handle {
    pub const SELF_PROCESS: Handle = Handle(handle::SELF_PROCESS);
    pub const MAIN_THREAD: Handle = Handle(handle::MAIN_THREAD);
    pub const CONSOLE: Handle = Handle(handle::CONSOLE);
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn raw_syscall(number: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> i64 {
    let ret: i64;
    // SAFETY: AriCodeOS ABI convention; `rcx` and `r11` are clobbered by the
    // instruction and declared as discarded outputs.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number as i64 => ret,
            in("rdi") a,
            in("rsi") b,
            in("rdx") c,
            in("r10") d,
            in("r8") e,
            in("r9") f,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    ret
}

#[cfg(not(target_arch = "x86_64"))]
pub fn raw_syscall(_n: u64, _a: u64, _b: u64, _c: u64, _d: u64, _e: u64, _f: u64) -> i64 {
    unimplemented!("x86-64 only")
}

fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    decode(raw_syscall(number, a, b, c, d, 0, 0))
}

fn call6(number: u64, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> Result<u64, Error> {
    decode(raw_syscall(number, a, b, c, d, e, f))
}

pub fn process_exit(code: u64) -> ! {
    raw_syscall(nr::PROCESS_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        core::hint::spin_loop();
    }
}

pub fn console_write(console: Handle, bytes: &[u8]) -> Result<u64, Error> {
    call(
        nr::CONSOLE_WRITE,
        console.0 as u64,
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        0,
    )
}

pub fn thread_yield() {
    let _ = call(nr::THREAD_YIELD, 0, 0, 0, 0);
}

pub fn thread_sleep(ms: u64) {
    let _ = call(nr::THREAD_SLEEP, ms, 0, 0, 0);
}

pub fn thread_get_tid() -> u64 {
    call(nr::THREAD_GET_TID, 0, 0, 0, 0).unwrap_or(0)
}

pub fn abi_version() -> u64 {
    call(nr::ABI_VERSION, 0, 0, 0, 0).unwrap_or(0)
}

pub fn handle_close(h: Handle) -> Result<(), Error> {
    call(nr::HANDLE_CLOSE, h.0 as u64, 0, 0, 0).map(|_| ())
}

pub fn handle_duplicate(h: Handle, rights: u32) -> Result<Handle, Error> {
    call(nr::HANDLE_DUPLICATE, h.0 as u64, rights as u64, 0, 0).map(|v| Handle(v as u32))
}

pub fn object_kind(h: Handle) -> Result<u64, Error> {
    call(nr::OBJECT_KIND, h.0 as u64, 0, 0, 0)
}

pub fn object_id(h: Handle) -> Result<u64, Error> {
    call(nr::OBJECT_ID, h.0 as u64, 0, 0, 0)
}

/// Revokes the object for all its handles (requires `MANAGE`).
pub fn object_revoke(h: Handle) -> Result<(), Error> {
    call(nr::OBJECT_REVOKE, h.0 as u64, 0, 0, 0).map(|_| ())
}

pub fn endpoint_create() -> Result<Handle, Error> {
    call(nr::ENDPOINT_CREATE, 0, 0, 0, 0).map(|v| Handle(v as u32))
}

/// A new endpoint as `(receive, send)` handles (ADR 0030): the receive one
/// has `READ|DUPLICATE|TRANSFER|MANAGE`, the send one
/// `WRITE|DUPLICATE|TRANSFER`. Once every send handle is gone, receiving
/// on an empty queue returns `Error::PeerClosed` at once.
pub fn endpoint_create_pair() -> Result<(Handle, Handle), Error> {
    call(nr::ENDPOINT_CREATE_PAIR, 0, 0, 0, 0).map(|v| (Handle(v as u32), Handle((v >> 32) as u32)))
}

/// Closes both sides of an endpoint for every holder (requires `MANAGE`,
/// ADR 0030): its queue is discarded and its clients get
/// `Error::PeerClosed` at once. The handles stay valid.
pub fn endpoint_close(ep: Handle) -> Result<(), Error> {
    call(nr::ENDPOINT_CLOSE, ep.0 as u64, 0, 0, 0).map(|_| ())
}

/// Sends `data` and transfers `handles` (they are removed from this process).
pub fn ipc_send(ep: Handle, data: &[u8], handles: &[Handle], timeout_ms: u64) -> Result<(), Error> {
    call6(
        nr::IPC_SEND,
        ep.0 as u64,
        data.as_ptr() as u64,
        data.len() as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
        timeout_ms,
    )
    .map(|_| ())
}

/// Receives into `buf` and writes the received handles into `handles_out`.
/// Returns (bytes, handles).
pub fn ipc_receive(
    ep: Handle,
    buf: &mut [u8],
    handles_out: &mut [Handle],
    timeout_ms: u64,
) -> Result<(usize, usize), Error> {
    call6(
        nr::IPC_RECEIVE,
        ep.0 as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        handles_out.as_mut_ptr() as u64,
        handles_out.len() as u64,
        timeout_ms,
    )
    .map(receive_result)
}

/// Receives from the first of `endpoints` (up to `IPC_MAX_ANY`, all with
/// `READ`) that has a message; `handles_out` holds `IPC_MAX_HANDLES`. Returns
/// (endpoint index, bytes, handles) (ADR 0020).
pub fn ipc_receive_any(
    endpoints: &[Handle],
    buf: &mut [u8],
    handles_out: &mut [Handle; abi::IPC_MAX_HANDLES],
    timeout_ms: u64,
) -> Result<(usize, usize, usize), Error> {
    let raw = call6(
        nr::IPC_RECEIVE_ANY,
        endpoints.as_ptr() as u64,
        endpoints.len() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        handles_out.as_mut_ptr() as u64,
        timeout_ms,
    )?;
    Ok((
        (raw >> 48) as usize,
        (raw & 0xFFFF_FFFF) as usize,
        ((raw >> 32) & 0xFFFF) as usize,
    ))
}

/// Creates a process from the initrd (requires `MANAGE` on the `SPAWNER`).
pub fn process_spawn(spawner: Handle, name: &str, arg: u64) -> Result<Handle, Error> {
    process_spawn_with(spawner, name, arg, &[])
}

/// Like `process_spawn`, transferring `handles` to the child (they appear in
/// it from `0x0001_0004` on, in the same order; they require `TRANSFER`).
pub fn process_spawn_with(
    spawner: Handle,
    name: &str,
    arg: u64,
    handles: &[Handle],
) -> Result<Handle, Error> {
    process_spawn_limited(spawner, name, arg, handles, 0)
}

/// Like `process_spawn_with`, setting the child's DMA memory quota in KiB
/// (0 = unlimited).
pub fn process_spawn_limited(
    spawner: Handle,
    name: &str,
    arg: u64,
    handles: &[Handle],
    memory_kib: u32,
) -> Result<Handle, Error> {
    call6(
        nr::PROCESS_SPAWN,
        spawner.0 as u64,
        name.as_ptr() as u64,
        name.len() as u64,
        arg,
        handles.as_ptr() as u64,
        abi::spawn_limits::pack(handles.len(), memory_kib),
    )
    .map(|h| Handle(h as u32))
}

/// Creates a process from an ELF held in the caller's DMA buffer `elf`
/// (`elf_len` bytes from `elf_offset`) (ADR 0017); requires `MANAGE` on the
/// `SPAWNER` and `READ` on the buffer. Handles and quota as in
/// `process_spawn_limited`.
#[allow(clippy::too_many_arguments)]
pub fn process_spawn_from(
    spawner: Handle,
    elf: Handle,
    elf_offset: u32,
    elf_len: u32,
    name: &str,
    arg: u64,
    handles: &[Handle],
    memory_kib: u32,
) -> Result<Handle, Error> {
    let req = abi::spawn_from::Request::new(elf.0, elf_offset, elf_len, name)
        .ok_or(Error::Invalid)?
        .encode();
    call6(
        nr::PROCESS_SPAWN_FROM,
        spawner.0 as u64,
        req.as_ptr() as u64,
        arg,
        handles.as_ptr() as u64,
        abi::spawn_limits::pack(handles.len(), memory_kib),
        0,
    )
    .map(|h| Handle(h as u32))
}

/// Copies an initrd file into `buf` (requires `READ` on the `SPAWNER`).
pub fn initrd_read(spawner: Handle, name: &str, buf: &mut [u8]) -> Result<usize, Error> {
    abi::decode(raw_syscall(
        nr::INITRD_READ,
        spawner.0 as u64,
        name.as_ptr() as u64,
        name.len() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ))
    .map(|n| n as usize)
}

/// Terminates another process (requires `MANAGE`); afterwards `process_wait`
/// returns `Err(Killed)`. An already finished process returns `Ok` with no effect.
pub fn process_kill(process: Handle) -> Result<(), Error> {
    call(nr::PROCESS_KILL, process.0 as u64, 0, 0, 0).map(|_| ())
}

/// Waits for a process to exit (requires `WAIT`). `Err(Killed)` if the kernel terminated it.
pub fn process_wait(process: Handle, timeout_ms: u64) -> Result<u64, Error> {
    call(nr::PROCESS_WAIT, process.0 as u64, timeout_ms, 0, 0)
}

/// Waits for interrupts on the line (requires `WAIT`); returns how many arrived.
pub fn irq_wait(irq: Handle, timeout_ms: u64) -> Result<u64, Error> {
    call(nr::IRQ_WAIT, irq.0 as u64, timeout_ms, 0, 0)
}

/// Maps an MMIO or DMA region into the process; returns its virtual address.
pub fn mmio_map(region: Handle) -> Result<u64, Error> {
    call(nr::MMIO_MAP, region.0 as u64, 0, 0, 0)
}

/// Removes a mapping `mmio_map` returned. A DMA buffer stays allocated
/// while it is mapped, even with every handle closed (ADR 0038): a server
/// that keeps a client's buffer mapped past the request releases it here.
pub fn mmio_unmap(virt: u64) -> Result<(), Error> {
    call(nr::MMIO_UNMAP, virt, 0, 0, 0).map(|_| ())
}

pub fn region_len(region: Handle) -> Result<u64, Error> {
    call(nr::REGION_LEN, region.0 as u64, 0, 0, 0)
}

/// Allocates a DMA buffer (requires `MANAGE` on a `HARDWARE` object).
pub fn dma_alloc(hardware: Handle, len: u64) -> Result<Handle, Error> {
    call(nr::DMA_ALLOC, hardware.0 as u64, len, 0, 0).map(|v| Handle(v as u32))
}

pub fn dma_phys(dma: Handle) -> Result<u64, Error> {
    call(nr::DMA_PHYS, dma.0 as u64, 0, 0, 0)
}

pub fn pci_count(root: Handle) -> Result<u64, Error> {
    call(nr::PCI_COUNT, root.0 as u64, 0, 0, 0)
}

/// Packed information about function `index` (see `abi::pci_info`).
pub fn pci_info(root: Handle, index: u64) -> Result<u64, Error> {
    call(nr::PCI_INFO, root.0 as u64, index, 0, 0)
}

/// Extended fields of function `index` (see `abi::pci_info::prog_if`).
pub fn pci_info_ext(root: Handle, index: u64) -> Result<u64, Error> {
    call(nr::PCI_INFO_EXT, root.0 as u64, index, 0, 0)
}

/// Claims function `index`: returns its handles (see `abi::nr::PCI_CLAIM`).
pub fn pci_claim(root: Handle, index: u64) -> Result<[Handle; abi::PCI_CLAIM_HANDLES], Error> {
    let mut raw = [0u32; abi::PCI_CLAIM_HANDLES];
    call(
        nr::PCI_CLAIM,
        root.0 as u64,
        index,
        raw.as_mut_ptr() as u64,
        0,
    )?;
    Ok(raw.map(Handle))
}

/// Reads up to `buf.len()` bytes from the console; blocks until there is at
/// least one or `timeout_ms` expires.
pub fn console_read(console: Handle, buf: &mut [u8], timeout_ms: u64) -> Result<usize, Error> {
    abi::decode(raw_syscall(
        nr::CONSOLE_READ,
        console.0 as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        timeout_ms,
        0,
        0,
    ))
    .map(|n| n as usize)
}

/// Writes text from a user-space keyboard into the console input ring
/// (requires `MANAGE` on the console; ADR 0013). Returns the bytes accepted.
pub fn console_inject(console: Handle, bytes: &[u8]) -> Result<usize, Error> {
    call(
        nr::CONSOLE_INJECT,
        console.0 as u64,
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        0,
    )
    .map(|n| n as usize)
}

/// System query (`abi::sysinfo`); returns the bytes written into `buf`.
/// Records a `Fault` event in the kernel ring for this process (ADR 0028):
/// `code` is the component's reason code, `name` what failed (at most
/// `sysinfo::EVENT_NAME_LEN` bytes). `diag` lists it.
pub fn event_record(code: u64, name: &str) -> Result<(), Error> {
    abi::decode(raw_syscall(
        nr::EVENT_RECORD,
        code,
        name.as_ptr() as u64,
        name.len() as u64,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

/// The kernel command line into `buf` (`sysinfo::CMDLINE`), as text.
pub fn cmdline(system: Handle, buf: &mut [u8]) -> Option<&str> {
    let n = system_info(system, abi::sysinfo::CMDLINE, buf).ok()?;
    core::str::from_utf8(&buf[..n]).ok()
}

pub fn system_info(system: Handle, kind: u64, buf: &mut [u8]) -> Result<usize, Error> {
    abi::decode(raw_syscall(
        nr::SYSTEM_INFO,
        system.0 as u64,
        kind,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
        0,
    ))
    .map(|n| n as usize)
}

/// Random bytes from the kernel (up to `sysinfo::RANDOM_MAX` per call) with a
/// `SYSTEM` handle (`READ`); returns how many were written.
pub fn random(system: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    let n = buf.len().min(abi::sysinfo::RANDOM_MAX as usize);
    system_info(system, abi::sysinfo::RANDOM, &mut buf[..n])
}

/// Powers off or reboots (`abi::power`). Only returns if the kernel rejects the action.
pub fn system_power(system: Handle, action: u64) -> Error {
    match abi::decode(raw_syscall(
        nr::SYSTEM_POWER,
        system.0 as u64,
        action,
        0,
        0,
        0,
        0,
    )) {
        Ok(_) => Error::Invalid,
        Err(e) => e,
    }
}

/// Helper for test programs: writes to the console, ignoring errors.
pub fn print(bytes: &[u8]) {
    let _ = console_write(Handle::CONSOLE, bytes);
}
