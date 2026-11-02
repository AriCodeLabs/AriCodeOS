// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-FileCopyrightText: The compiler-builtins contributors (rust-lang/compiler-builtins)
// SPDX-License-Identifier: Apache-2.0
//! `memcpy`, `memmove`, `memset`, `memcmp` and `bcmp`, which the compiler
//! emits calls to. User programs are built for `x86_64-unknown-linux-gnu`
//! without its C library (ADR 0065), and that target's `compiler_builtins`
//! leaves these to libc.
//!
//! Adapted from compiler-builtins (`src/mem/mod.rs` and `src/mem/x86_64.rs`,
//! MIT OR Apache-2.0): the routines `x86_64-unknown-none` programs ran
//! before, so copies cost what they did. Changed: no unstable intrinsics,
//! no `strlen` (nothing calls it), and only the non-`ermsb` paths, since
//! the baseline target does not assume Enhanced REP MOVSB.
//!
//! The copies and fills are `rep` instructions in assembly, so the compiler
//! cannot turn them back into calls to themselves.

use core::arch::asm;
use core::ffi::{c_int, c_void};

/// Bytes before an 8-byte aligned destination, 8-byte words, bytes after.
fn rep_param(dest: *mut u8, mut count: usize) -> (usize, usize, usize) {
    // Unaligned writes are still slow on modern processors, so align the
    // destination address.
    let pre_byte_count = ((8 - (dest as usize & 0b111)) & 0b111).min(count);
    count -= pre_byte_count;
    (pre_byte_count, count >> 3, count & 0b111)
}

/// # Safety
///
/// `src` readable and `dest` writable for `count` bytes.
#[inline(always)]
unsafe fn copy_forward(mut dest: *mut u8, mut src: *const u8, count: usize) {
    let (pre_byte_count, qword_count, byte_count) = rep_param(dest, count);
    // Separate blocks give the compiler more freedom to reorder.
    asm!(
        "rep movsb",
        inout("ecx") pre_byte_count => _,
        inout("rdi") dest => dest,
        inout("rsi") src => src,
        options(nostack, preserves_flags)
    );
    asm!(
        "rep movsq",
        inout("rcx") qword_count => _,
        inout("rdi") dest => dest,
        inout("rsi") src => src,
        options(nostack, preserves_flags)
    );
    asm!(
        "rep movsb",
        inout("ecx") byte_count => _,
        inout("rdi") dest => _,
        inout("rsi") src => _,
        options(nostack, preserves_flags)
    );
}

/// # Safety
///
/// As `copy_forward`, with `count > 0`.
#[inline(always)]
unsafe fn copy_backward(dest: *mut u8, src: *const u8, count: usize) {
    let (pre_byte_count, qword_count, byte_count) = rep_param(dest, count);
    // One block: the direction flag must be cleared again before anything
    // else runs.
    asm!(
        "std",
        "rep movsb",
        "sub rsi, 7",
        "sub rdi, 7",
        "mov rcx, {qword_count:r}",
        "rep movsq",
        "add rsi, 7",
        "add rdi, 7",
        "mov ecx, {pre_byte_count:e}",
        "rep movsb",
        "cld",
        pre_byte_count = in(reg) pre_byte_count,
        qword_count = in(reg) qword_count,
        inout("ecx") byte_count => _,
        inout("rdi") dest.add(count - 1) => _,
        inout("rsi") src.add(count - 1) => _,
        options(nostack)
    );
}

/// # Safety
///
/// `dest` writable for `count` bytes.
#[inline(always)]
unsafe fn set_bytes(mut dest: *mut u8, c: u8, count: usize) {
    let c = c as u64 * 0x0101_0101_0101_0101;
    let (pre_byte_count, qword_count, byte_count) = rep_param(dest, count);
    asm!(
        "rep stosb",
        inout("ecx") pre_byte_count => _,
        inout("rdi") dest => dest,
        in("rax") c,
        options(nostack, preserves_flags)
    );
    asm!(
        "rep stosq",
        inout("rcx") qword_count => _,
        inout("rdi") dest => dest,
        in("rax") c,
        options(nostack, preserves_flags)
    );
    asm!(
        "rep stosb",
        inout("ecx") byte_count => _,
        inout("rdi") dest => _,
        in("rax") c,
        options(nostack, preserves_flags)
    );
}

/// Compares in 16-, 8-, 4-, 2-byte and single-byte steps; the first
/// differing byte decides.
///
/// # Safety
///
/// `a` and `b` readable for `n` bytes.
#[inline(always)]
unsafe fn compare_bytes(a: *const u8, b: *const u8, n: usize) -> i32 {
    #[inline(always)]
    unsafe fn cmp<T: Copy + Eq, U, F>(mut a: *const T, mut b: *const T, n: usize, f: F) -> i32
    where
        F: FnOnce(*const U, *const U, usize) -> i32,
    {
        let size = core::mem::size_of::<T>();
        let end = a.add(n / size);
        while a != end {
            if a.read_unaligned() != b.read_unaligned() {
                return f(a.cast(), b.cast(), size);
            }
            a = a.add(1);
            b = b.add(1);
        }
        f(a.cast(), b.cast(), n % size)
    }
    let c1 = |mut a: *const u8, mut b: *const u8, n| {
        for _ in 0..n {
            if a.read() != b.read() {
                return i32::from(a.read()) - i32::from(b.read());
            }
            a = a.add(1);
            b = b.add(1);
        }
        0
    };
    let c2 = |a: *const u16, b, n| cmp(a, b, n, c1);
    let c4 = |a: *const u32, b, n| cmp(a, b, n, c2);
    let c8 = |a: *const u64, b, n| cmp(a, b, n, c4);
    let c16 = |a: *const u128, b, n| cmp(a, b, n, c8);
    c16(a.cast(), b.cast(), n)
}

/// # Safety
///
/// C semantics: `src` readable and `dest` writable for `n` bytes, not
/// overlapping.
#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    copy_forward(dest.cast(), src.cast(), n);
    dest
}

/// # Safety
///
/// C semantics: `src` readable and `dest` writable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let delta = (dest as usize).wrapping_sub(src as usize);
    if delta >= n {
        // Forwards: `dest` is far enough ahead of `src`, or behind it
        // (and `delta` wrapped).
        copy_forward(dest.cast(), src.cast(), n);
    } else {
        copy_backward(dest.cast(), src.cast(), n);
    }
    dest
}

/// # Safety
///
/// C semantics: `s` writable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memset(s: *mut c_void, c: c_int, n: usize) -> *mut c_void {
    set_bytes(s.cast(), c as u8, n);
    s
}

/// # Safety
///
/// C semantics: `s1` and `s2` readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const c_void, s2: *const c_void, n: usize) -> c_int {
    compare_bytes(s1.cast(), s2.cast(), n)
}

/// # Safety
///
/// C semantics: `s1` and `s2` readable for `n` bytes.
#[no_mangle]
pub unsafe extern "C" fn bcmp(s1: *const c_void, s2: *const c_void, n: usize) -> c_int {
    memcmp(s1, s2, n)
}
