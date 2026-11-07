// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Futexes (ADR 0082): a thread sleeps while a word of its process's memory holds
//! a value, and another thread of the process wakes it. Wait and wake only: no
//! priority inheritance and no requeue, the paths where Linux's futex failed
//! (CVE-2014-3153), and no futex shared between processes.
//!
//! No wake is lost. The word is read and the thread queued in one hold of the
//! table, interrupts off: a waker changes the word and then takes the table, so
//! either the read sees the change or the waker finds the thread queued. A wake
//! that comes after the thread is queued but before it blocks stays pending in
//! the scheduler, which turns the block into a yield.

use super::process;
use crate::sync::{IrqLock, KernelIrq};
use crate::task::scheduler;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use aricode_abi::Error;
use aricode_sched::Tid;
use aricode_sync::IrqPolicy;

/// The threads waiting on each word, by process and address, oldest first. An
/// entry goes with its last waiter, so the table holds no more entries than there
/// are threads waiting.
static WAITING: IrqLock<BTreeMap<(u32, u64), VecDeque<Tid>>> = IrqLock::new(BTreeMap::new());

/// The key of the word at `addr` of the current process, checked: aligned and the
/// process's own memory.
fn key(addr: u64) -> Result<(u32, u64), Error> {
    if !addr.is_multiple_of(4) {
        return Err(Error::Invalid);
    }
    process::check_user_readable(addr, 4)?;
    let pid = process::current_pid().ok_or(Error::Invalid)?;
    Ok((pid.0, addr))
}

/// `futex_wait`: sleeps while the word at `addr` holds `expected`.
pub fn wait(addr: u64, expected: u32, deadline: Option<u64>) -> Result<u64, Error> {
    let key = key(addr)?;
    if process::current_kill_requested() {
        return Ok(0);
    }
    let me = scheduler::current_tid();
    let irq = KernelIrq::save_and_disable();
    {
        let mut waiting = WAITING.lock();
        let mut word = [0u8; 4];
        // SAFETY: `word` holds 4 bytes; `addr` was checked as the process's, and a
        // page gone since makes the read fail rather than fault the kernel.
        let read =
            unsafe { crate::arch::x86_64::usercopy::copy(word.as_mut_ptr(), addr as *const u8, 4) };
        let early = if !read {
            Some(Err(Error::Fault))
        } else if u32::from_le_bytes(word) != expected {
            Some(Ok(0))
        } else if deadline.is_some_and(|d| crate::time::ticks() >= d) {
            Some(Err(Error::Timeout))
        } else {
            None
        };
        if let Some(r) = early {
            drop(waiting);
            KernelIrq::restore(irq);
            return r;
        }
        waiting.entry(key).or_default().push_back(me);
    }
    match deadline {
        Some(d) => scheduler::sleep_until(d),
        None => scheduler::block_current(),
    }
    KernelIrq::restore(irq);
    // Still queued means nobody woke it: the deadline, a kill or a spurious wake.
    let still = {
        let mut waiting = WAITING.lock();
        let still = waiting.get_mut(&key).is_some_and(|q| {
            let at = q.iter().position(|t| *t == me);
            at.is_some_and(|i| q.remove(i).is_some())
        });
        if waiting.get(&key).is_some_and(|q| q.is_empty()) {
            waiting.remove(&key);
        }
        still
    };
    match deadline {
        Some(d) if still && crate::time::ticks() >= d => Err(Error::Timeout),
        _ => Ok(0),
    }
}

/// `futex_wake`: wakes up to `count` threads waiting on the word at `addr`.
pub fn wake(addr: u64, count: u64) -> Result<u64, Error> {
    let key = key(addr)?;
    let woken: Vec<Tid> = {
        let mut waiting = WAITING.lock();
        let Some(q) = waiting.get_mut(&key) else {
            return Ok(0);
        };
        let n = (count as usize).min(q.len());
        let woken = q.drain(..n).collect();
        if q.is_empty() {
            waiting.remove(&key);
        }
        woken
    };
    for &t in &woken {
        let _ = scheduler::wake(t);
    }
    Ok(woken.len() as u64)
}
