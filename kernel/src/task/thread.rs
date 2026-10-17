// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Kernel thread control block.

use super::context::{initial_stack_frame, ThreadEntry};
use super::stack::KernelStack;
use alloc::string::String;
use aricode_sched::Tid;

pub struct Thread {
    pub tid: Tid,
    pub name: String,
    /// `None` for the boot context (uses the kernel stack).
    pub stack: Option<KernelStack>,
    /// `rsp` saved by `switch_context` while the thread is not running.
    pub saved_rsp: u64,
    /// User process it belongs to (`None` = kernel thread).
    pub process: Option<u32>,
    /// PML4 to load into CR3 when switching to this thread.
    pub cr3: u64,
}

impl Thread {
    /// Thread already running on a foreign stack (the boot thread).
    pub fn adopt_current(tid: Tid, name: &str, cr3: u64) -> Self {
        Self {
            tid,
            name: String::from(name),
            stack: None,
            saved_rsp: 0,
            process: None,
            cr3,
        }
    }

    /// Thread already running on `stack` without having gone through
    /// `switch_context`: an AP's idle thread, which starts directly on it.
    pub fn adopt_on_stack(tid: Tid, name: &str, stack: KernelStack, cr3: u64) -> Self {
        Self {
            tid,
            name: String::from(name),
            stack: Some(stack),
            saved_rsp: 0,
            process: None,
            cr3,
        }
    }

    /// New thread with its own stack, ready for its first context switch.
    pub fn new(
        tid: Tid,
        name: &str,
        entry: ThreadEntry,
        arg: usize,
        process: Option<u32>,
        cr3: u64,
    ) -> Result<Self, &'static str> {
        let stack = KernelStack::allocate(tid.0)?;
        // SAFETY: `stack.top()` is the end of a freshly mapped RW stack.
        let saved_rsp = unsafe { initial_stack_frame(stack.top(), entry, arg) };
        Ok(Self {
            tid,
            name: String::from(name),
            stack: Some(stack),
            saved_rsp,
            process,
            cr3,
        })
    }

    /// Top of this thread's kernel stack (TSS.rsp0 and `syscall` entry).
    pub fn kernel_stack_top(&self) -> u64 {
        match &self.stack {
            Some(s) => s.top(),
            None => crate::memory::vmm::KERNEL_STACK_TOP,
        }
    }
}
