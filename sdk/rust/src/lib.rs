//! Llamadas al sistema desde espacio de usuario (primer trozo del SDK,
//! sección 27). Sin `alloc`, sin `std`: solo la instrucción `syscall` y
//! wrappers tipados sobre `aricode-abi`.

#![no_std]

pub mod block;
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

/// Handle de usuario (opaco).
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
    // SAFETY: convención del ABI de AriCodeOS; `rcx` y `r11` los destruye la
    // instrucción y se declaran como salidas descartadas.
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
    unimplemented!("solo x86-64")
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

/// Revoca el objeto para todos sus handles (exige `MANAGE`).
pub fn object_revoke(h: Handle) -> Result<(), Error> {
    call(nr::OBJECT_REVOKE, h.0 as u64, 0, 0, 0).map(|_| ())
}

pub fn endpoint_create() -> Result<Handle, Error> {
    call(nr::ENDPOINT_CREATE, 0, 0, 0, 0).map(|v| Handle(v as u32))
}

/// Envía `data` y transfiere `handles` (se retiran de este proceso).
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

/// Recibe en `buf` y escribe los handles recibidos en `handles_out`.
/// Devuelve (bytes, handles).
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

/// Crea un proceso del initrd (exige `MANAGE` sobre el `SPAWNER`).
pub fn process_spawn(spawner: Handle, name: &str, arg: u64) -> Result<Handle, Error> {
    process_spawn_with(spawner, name, arg, &[])
}

/// Como `process_spawn`, transfiriendo `handles` al hijo (aparecen en él a
/// partir de `0x0001_0004` en el mismo orden; exigen `TRANSFER`).
pub fn process_spawn_with(
    spawner: Handle,
    name: &str,
    arg: u64,
    handles: &[Handle],
) -> Result<Handle, Error> {
    process_spawn_limited(spawner, name, arg, handles, 0)
}

/// Como `process_spawn_with`, fijando la cuota de memoria DMA del hijo en
/// KiB (0 = sin límite).
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

/// Copia un fichero del initrd en `buf` (exige `READ` sobre el `SPAWNER`).
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

/// Espera la salida de un proceso (exige `WAIT`). `Err(Killed)` si lo terminó el kernel.
pub fn process_wait(process: Handle, timeout_ms: u64) -> Result<u64, Error> {
    call(nr::PROCESS_WAIT, process.0 as u64, timeout_ms, 0, 0)
}

/// Espera interrupciones de la línea (exige `WAIT`); devuelve cuántas llegaron.
pub fn irq_wait(irq: Handle, timeout_ms: u64) -> Result<u64, Error> {
    call(nr::IRQ_WAIT, irq.0 as u64, timeout_ms, 0, 0)
}

/// Mapea una región MMIO o DMA en el proceso; devuelve su dirección virtual.
pub fn mmio_map(region: Handle) -> Result<u64, Error> {
    call(nr::MMIO_MAP, region.0 as u64, 0, 0, 0)
}

pub fn region_len(region: Handle) -> Result<u64, Error> {
    call(nr::REGION_LEN, region.0 as u64, 0, 0, 0)
}

/// Reserva un buffer DMA (exige `MANAGE` sobre un objeto `HARDWARE`).
pub fn dma_alloc(hardware: Handle, len: u64) -> Result<Handle, Error> {
    call(nr::DMA_ALLOC, hardware.0 as u64, len, 0, 0).map(|v| Handle(v as u32))
}

pub fn dma_phys(dma: Handle) -> Result<u64, Error> {
    call(nr::DMA_PHYS, dma.0 as u64, 0, 0, 0)
}

pub fn pci_count(root: Handle) -> Result<u64, Error> {
    call(nr::PCI_COUNT, root.0 as u64, 0, 0, 0)
}

/// Información empaquetada de la función `index` (ver `abi::pci_info`).
pub fn pci_info(root: Handle, index: u64) -> Result<u64, Error> {
    call(nr::PCI_INFO, root.0 as u64, index, 0, 0)
}

/// Campos extendidos de la función `index` (ver `abi::pci_info::prog_if`).
pub fn pci_info_ext(root: Handle, index: u64) -> Result<u64, Error> {
    call(nr::PCI_INFO_EXT, root.0 as u64, index, 0, 0)
}

/// Reclama la función `index`: devuelve sus handles (ver `abi::nr::PCI_CLAIM`).
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

/// Lee de la consola hasta `buf.len()` bytes; bloquea hasta que haya al
/// menos uno o venza `timeout_ms`.
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

/// Escribe texto de un teclado de usuario en el anillo de entrada de consola
/// (exige `MANAGE` sobre la consola; ADR 0013). Devuelve los bytes aceptados.
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

/// Consulta del sistema (`abi::sysinfo`); devuelve los bytes escritos en `buf`.
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

/// Bytes aleatorios del kernel (hasta `sysinfo::RANDOM_MAX` por llamada) con
/// un handle `SYSTEM` (`READ`); devuelve cuántos escribió.
pub fn random(system: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    let n = buf.len().min(abi::sysinfo::RANDOM_MAX as usize);
    system_info(system, abi::sysinfo::RANDOM, &mut buf[..n])
}

/// Apaga o reinicia (`abi::power`). Solo retorna si el kernel rechaza la acción.
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

/// Ayuda para programas de prueba: escribe en la consola ignorando errores.
pub fn print(bytes: &[u8]) {
    let _ = console_write(Handle::CONSOLE, bytes);
}
