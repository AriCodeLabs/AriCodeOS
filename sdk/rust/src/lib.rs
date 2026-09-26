//! Llamadas al sistema desde espacio de usuario (primer trozo del SDK,
//! sección 27). Sin `alloc`, sin `std`: solo la instrucción `syscall` y
//! wrappers tipados sobre `aricode-abi`.

#![no_std]

pub use aricode_abi::{self as abi, handle, kind, rights, Error};
use aricode_abi::{decode, nr};

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
pub fn raw_syscall(number: u64, a: u64, b: u64, c: u64, d: u64) -> i64 {
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
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    ret
}

#[cfg(not(target_arch = "x86_64"))]
pub fn raw_syscall(_number: u64, _a: u64, _b: u64, _c: u64, _d: u64) -> i64 {
    unimplemented!("solo x86-64")
}

fn call(number: u64, a: u64, b: u64, c: u64, d: u64) -> Result<u64, Error> {
    decode(raw_syscall(number, a, b, c, d))
}

pub fn process_exit(code: u64) -> ! {
    raw_syscall(nr::PROCESS_EXIT, code, 0, 0, 0);
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

/// Ayuda para programas de prueba: escribe en la consola ignorando errores.
pub fn print(bytes: &[u8]) {
    let _ = console_write(Handle::CONSOLE, bytes);
}
