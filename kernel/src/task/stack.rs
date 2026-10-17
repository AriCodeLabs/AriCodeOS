// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Thread stacks: a dedicated virtual region, frames from the PMM and an
//! unmapped guard page below each stack.

use crate::memory::{pmm, vmm};
use aricode_pmm::Frame;
use aricode_x86_paging::{GLOBAL, NO_EXECUTE, PAGE_SIZE, WRITABLE};

/// Thread stack region (PML4 entry 448).
pub const STACKS_BASE: u64 = 0xFFFF_E000_0000_0000;
pub const STACK_SIZE: u64 = 32 * 1024;
const SLOT_SIZE: u64 = STACK_SIZE + PAGE_SIZE; // guard + stack

pub struct KernelStack {
    slot: u32,
}

impl KernelStack {
    /// Maps the stack of slot `slot` (one per thread, never reused within the
    /// same boot session: the slot is the tid).
    pub fn allocate(slot: u32) -> Result<Self, &'static str> {
        let s = Self { slot };
        let mut done = 0;
        for virt in (s.bottom()..s.top()).step_by(PAGE_SIZE as usize) {
            let Some(frame) = pmm::with(|p| p.allocate()) else {
                s.unmap(done);
                return Err("stack: out of frames");
            };
            if vmm::with(|m| m.map_4k(virt, frame.addr(), WRITABLE | NO_EXECUTE | GLOBAL)).is_err()
            {
                pmm::with(|p| p.free(frame)).expect("freshly allocated frame");
                s.unmap(done);
                return Err("stack: map");
            }
            done += PAGE_SIZE;
        }
        Ok(s)
    }

    pub fn guard(&self) -> u64 {
        STACKS_BASE + self.slot as u64 * SLOT_SIZE
    }
    pub fn bottom(&self) -> u64 {
        self.guard() + PAGE_SIZE
    }
    pub fn top(&self) -> u64 {
        self.bottom() + STACK_SIZE
    }
    pub fn in_guard(&self, addr: u64) -> bool {
        (self.guard()..self.bottom()).contains(&addr)
    }

    fn unmap(&self, len: u64) {
        for virt in (self.bottom()..self.bottom() + len).step_by(PAGE_SIZE as usize) {
            let phys = vmm::with(|m| m.unmap_4k(virt)).expect("mapped stack page");
            // SAFETY: invalidation of an entry we just removed.
            unsafe {
                core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags))
            };
            pmm::with(|p| p.free(Frame::from_addr(phys))).expect("stack frame");
        }
    }
}

impl Drop for KernelStack {
    /// Returns the pages to the PMM. The running thread's stack must never be
    /// dropped: the scheduler only frees finished threads from another one.
    fn drop(&mut self) {
        self.unmap(STACK_SIZE);
    }
}
