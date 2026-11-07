// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Exhaust every allocation point, including a partially built mip chain or mesh.
use aricode_render3d::{Mesh, Texture};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static REMAINING: Cell<Option<usize>> = const { Cell::new(None) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

struct Failing;
#[global_allocator]
static ALLOCATOR: Failing = Failing;

// SAFETY: successful allocations and all deallocations delegate to System with
// the caller's original layout. Failure injection is local to the test thread.
unsafe impl GlobalAlloc for Failing {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let allowed = REMAINING.with(|r| match r.get() {
            None => true,
            Some(0) => false,
            Some(n) => {
                r.set(Some(n - 1));
                true
            }
        });
        if !allowed {
            return std::ptr::null_mut();
        }
        // SAFETY: this is the allocator's incoming layout.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.with(|live| live.set(live.get() + 1));
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.with(|live| live.set(live.get() - 1));
        // SAFETY: every successful allocation above came from System.
        unsafe { System.dealloc(ptr, layout) };
    }
}

fn exhaust<T>(make: impl Fn() -> Option<T>) {
    let mut failures = 0;
    for available in 0..32 {
        let before = LIVE.with(Cell::get);
        REMAINING.with(|r| r.set(Some(available)));
        let result = make();
        REMAINING.with(|r| r.set(None));
        let succeeded = result.is_some();
        drop(result);
        assert_eq!(LIVE.with(Cell::get), before, "partial construction leaked");
        if succeeded {
            assert!(failures > 0);
            return;
        }
        failures += 1;
    }
    panic!("constructor never succeeded");
}

#[test]
fn allocation_failure_is_recoverable_and_releases_partial_assets() {
    exhaust(|| Texture::try_new(128, |x, y| (x ^ y) as u32));
    exhaust(Mesh::try_cube);
    exhaust(|| Mesh::try_torus(1.3, 0.5, 64, 32));
    exhaust(|| Mesh::try_sphere(1.6, 64, 128));
    exhaust(|| Mesh::try_terrain(120.0, 96, 4.0));
}
