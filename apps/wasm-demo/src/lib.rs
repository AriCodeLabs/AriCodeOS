// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
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

/// Launch argument that runs the benchmark instead of the demo (ADR 0071).
const BENCH: i64 = 99;

/// Recursive on purpose: calls are what it measures.
fn fib_rec(n: i32) -> i32 {
    if n < 2 {
        n
    } else {
        fib_rec(n - 1) + fib_rec(n - 2)
    }
}

static mut SIEVE: [u8; 100_000] = [0; 100_000];

/// Primes below `n`: memory and loops.
fn sieve(n: usize) -> u64 {
    // SAFETY: the module is single-threaded and nothing else holds it.
    let s = unsafe { &mut *core::ptr::addr_of_mut!(SIEVE) };
    let n = n.min(s.len());
    s[..n].fill(1);
    let mut count = 0;
    for i in 2..n {
        if s[i] == 1 {
            count += 1;
            let mut j = i * i;
            while j < n {
                s[j] = 0;
                j += i;
            }
        }
    }
    count
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

static mut BLOCKS: [u8; 16384] = [0; 16384];

/// SHA-256 compression over 16 KiB, `rounds` times: 32-bit arithmetic.
fn sha(rounds: u32) -> u64 {
    // SAFETY: as for `SIEVE`.
    let buf = unsafe { &mut *core::ptr::addr_of_mut!(BLOCKS) };
    for (i, b) in buf.iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    for _ in 0..rounds {
        for block in buf.as_chunks::<64>().0 {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    block[4 * i],
                    block[4 * i + 1],
                    block[4 * i + 2],
                    block[4 * i + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(s0.wrapping_add(maj));
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
                *x = x.wrapping_add(y);
            }
        }
    }
    h[0] as u64
}

const N: usize = 64;
static mut MATRICES: [[i32; N * N]; 3] = [[0; N * N]; 3];

/// A 64×64 integer matrix product: loads, stores and multiplications.
fn matmul() -> u64 {
    // SAFETY: as for `SIEVE`.
    let [a, b, c] = unsafe { &mut *core::ptr::addr_of_mut!(MATRICES) };
    for i in 0..N * N {
        a[i] = (i % 17) as i32 - 8;
        b[i] = (i % 13) as i32 - 6;
    }
    for i in 0..N {
        for j in 0..N {
            let mut s = 0i32;
            for k in 0..N {
                s = s.wrapping_add(a[i * N + k].wrapping_mul(b[k * N + j]));
            }
            c[i * N + j] = s;
        }
    }
    c.iter().fold(0i32, |s, x| s.wrapping_add(*x)) as u32 as u64
}

/// A small stack machine dispatching through `match` (a `br_table`).
fn vm(steps: u32) -> u64 {
    const PROG: [u8; 12] = [0, 1, 2, 3, 0, 4, 2, 5, 1, 3, 6, 7];
    let (mut acc, mut x, mut y) = (1i32, 3i32, 7i32);
    let mut pc = 0;
    for _ in 0..steps {
        match PROG[pc] {
            0 => acc = acc.wrapping_add(x),
            1 => x = x.wrapping_mul(3) ^ y,
            2 => y = y.wrapping_add(acc >> 3),
            3 => acc ^= y << 1,
            4 => x = x.rotate_left(5),
            5 => y = y.wrapping_sub(x),
            6 => acc = acc.wrapping_mul(7),
            _ => x ^= acc,
        }
        pc = (pc + 1) % PROG.len();
    }
    (acc ^ x ^ y) as u32 as u64
}

/// Prints a workload's result and the milliseconds it took.
fn report(name: &[u8], result: u64, ms: i64) {
    let mut buf = [0u8; 20];
    say(b"wasm bench: ");
    say(name);
    say(b" = ");
    say(fmt_u64(result, &mut buf));
    say(b" in ");
    say(fmt_u64(ms as u64, &mut buf));
    say(b" ms\n");
}

/// Times `$work` with `uptime_ms` and reports it. Direct calls, no
/// function pointers: the module stays within what every engine compared
/// in ADR 0071 accepts.
macro_rules! timed {
    ($name:expr, $work:expr) => {{
        // SAFETY: import with no arguments.
        let t0 = unsafe { uptime_ms() };
        let result = $work;
        let t1 = unsafe { uptime_ms() };
        report($name, result, t1 - t0);
    }};
}

/// Runs each workload.
fn bench() -> i32 {
    timed!(b"fib(22)", fib_rec(22) as u64);
    timed!(b"sieve(100000)", sieve(100_000));
    timed!(b"sha256 16 KiB", sha(1));
    timed!(b"matmul 64", matmul());
    timed!(b"vm 500000", vm(500_000));
    0
}

/// Entry point called by the host: `arg` is the launch argument; the result
/// is the exit code.
#[no_mangle]
pub extern "C" fn main(arg: i64) -> i32 {
    if arg == BENCH {
        return bench();
    }
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
