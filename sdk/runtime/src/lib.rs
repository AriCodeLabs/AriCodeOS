// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Runtime for user programs (Phase 27): `_start`, heap, panic and
//! formatting helpers without `alloc`. Every AriCodeOS program (services,
//! applications and test programs) depends on this crate and on the
//! syscall and service SDK (`aricode-sys`, re-exported as `sys`).
//!
//! A minimal program:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! use aricode_rt::sys;
//! fn main(arg: u64) -> u64 { sys::print(b"hello\n"); 0 }
//! aricode_rt::entry!(main);
//! ```

#![no_std]

extern crate alloc;

pub use aricode_sys as sys;

/// User program heap: 256 KiB of static memory served by
/// `aricode-heap` (the same allocator as the kernel) behind a spinlock. No
/// growth: services and drivers use little dynamic memory.
mod user_heap {
    use aricode_heap::Heap;
    use aricode_sync::SpinLock;
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;

    const SIZE: usize = 256 * 1024;

    #[repr(align(4096))]
    struct Arena(UnsafeCell<[u8; SIZE]>);
    // SAFETY: only accessed under the heap lock.
    unsafe impl Sync for Arena {}
    static ARENA: Arena = Arena(UnsafeCell::new([0; SIZE]));

    struct UserHeap {
        heap: SpinLock<Option<Heap>>,
    }

    #[global_allocator]
    static HEAP: UserHeap = UserHeap {
        heap: SpinLock::new(None),
    };

    impl UserHeap {
        fn with<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> R {
            let mut guard = self.heap.lock();
            let heap = guard.get_or_insert_with(|| {
                let mut h = Heap::empty();
                // SAFETY: static page-aligned region, exclusive to the heap.
                unsafe { h.add_region(ARENA.0.get() as usize, SIZE) };
                h
            });
            f(heap)
        }
    }

    // SAFETY: access serialized by the spinlock; pointers come from `alloc`.
    unsafe impl GlobalAlloc for UserHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            self.with(|h| {
                h.allocate(layout)
                    .map_or(core::ptr::null_mut(), |p| p.as_ptr())
            })
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if let Some(p) = core::ptr::NonNull::new(ptr) {
                self.with(|h| h.deallocate(p, layout));
            }
        }
    }
}

/// Exit code of the panic handler.
pub const PANIC_EXIT_CODE: u64 = 255;

/// Prints `prefix` followed by the ABI error (its numeric code).
pub fn print_error(prefix: &[u8], e: sys::Error) {
    let mut buf = [0u8; 20];
    sys::print(prefix);
    sys::print(b" error ");
    sys::print(fmt_u64(-(e.to_raw()) as u64, &mut buf));
    sys::print(b"\n");
}

/// Formats an integer in decimal without `alloc` (for console messages).
pub fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
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

/// Formats an integer in hexadecimal with `width` digits (zero-padded).
pub fn fmt_hex(v: u64, width: usize, buf: &mut [u8; 16]) -> &[u8] {
    let width = width.clamp(1, 16);
    for (i, slot) in buf.iter_mut().enumerate().take(width) {
        let nibble = ((v >> (4 * (width - 1 - i))) & 0xF) as u8;
        *slot = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
    }
    &buf[..width]
}

/// Defines `_start` for a `fn main(arg: u64) -> u64`: `arg` is the value
/// passed by the process creator and the result is the exit code.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub extern "C" fn _start(arg: u64) -> ! {
            let code: u64 = $main(arg);
            $crate::sys::process_exit(code)
        }

        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            $crate::sys::print(b"panic in user program\n");
            $crate::sys::process_exit($crate::PANIC_EXIT_CODE)
        }
    };
}

/// Binary whose entry point is in assembly (global `_start` in the .s file).
#[macro_export]
macro_rules! asm_program {
    ($file:literal) => {
        core::arch::global_asm!(include_str!($file));

        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            $crate::sys::process_exit($crate::PANIC_EXIT_CODE)
        }
    };
}
