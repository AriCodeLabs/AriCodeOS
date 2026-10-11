//! Portable app (Phase 29 and stage 4): compiled to `wasm32-unknown-unknown`,
//! it only knows the Capability API that the host (`wasm_runner`) imports
//! into it under the `aricode` module, and only what its manifest
//! (`demo.manifest`) grants: no POSIX and no syscalls.
#![no_std]

#[link(wasm_import_module = "aricode")]
extern "C" {
    /// Writes `len` bytes of module memory to the console.
    fn print(ptr: *const u8, len: u32);
    /// Milliseconds since system boot.
    fn uptime_ms() -> i64;
    /// Creates or fully replaces a file in the runner's private directory;
    /// returns the bytes written or -1.
    fn file_write(name: *const u8, name_len: u32, data: *const u8, len: u32) -> i32;
    /// Reads a file from the private directory into `buf`; bytes read or -1.
    fn file_read(name: *const u8, name_len: u32, buf: *mut u8, len: u32) -> i32;
}

fn say(s: &[u8]) {
    // SAFETY: `s` is in the module linear memory; the host validates the range.
    unsafe { print(s.as_ptr(), s.len() as u32) };
}

fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &buf[i..]
}

fn fib(n: u32) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        let t = a + b;
        a = b;
        b = t;
    }
    a
}

/// Entry point called by the host: `arg` is the launch argument; the result
/// is the exit code.
#[no_mangle]
pub extern "C" fn main(arg: i64) -> i32 {
    let mut buf = [0u8; 20];
    say(b"wasm: hello from WebAssembly (arg ");
    say(fmt_u64(arg as u64, &mut buf));
    say(b")\n");
    say(b"wasm: fib(24) = ");
    say(fmt_u64(fib(24), &mut buf));
    say(b"\n");
    // SAFETY: import with no arguments.
    let ms = unsafe { uptime_ms() };
    say(b"wasm: uptime ");
    say(fmt_u64(ms as u64 / 1000, &mut buf));
    say(b" s\n");
    // A table in memory: the module data reaches linear memory.
    let table = [3u32, 1, 4, 1, 5, 9, 2, 6];
    let mut sum = 0u32;
    for (i, v) in table.iter().enumerate() {
        sum = sum.wrapping_add(v * (i as u32 + 1));
    }
    say(b"wasm: weighted sum ");
    say(fmt_u64(sum as u64, &mut buf));
    say(b"\n");
    // A file in the runner's private directory: write and read back.
    let name = b"notas";
    let text = b"escrito desde wasm";
    let mut back = [0u8; 32];
    // SAFETY: pointers into the module linear memory; the host validates them.
    let (written, read) = unsafe {
        (
            file_write(
                name.as_ptr(),
                name.len() as u32,
                text.as_ptr(),
                text.len() as u32,
            ),
            file_read(
                name.as_ptr(),
                name.len() as u32,
                back.as_mut_ptr(),
                back.len() as u32,
            ),
        )
    };
    if written as usize != text.len() || read as usize != text.len() || &back[..text.len()] != text
    {
        say(b"wasm: file did not come back the same\n");
        return 3;
    }
    say(b"wasm: file 'notas' written and read (");
    say(fmt_u64(text.len() as u64, &mut buf));
    say(b" bytes)\n");
    42
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
