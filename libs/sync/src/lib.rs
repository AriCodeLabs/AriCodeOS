// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel mutual exclusion primitives.
//!
//! [`IrqLock`] is a spinlock that also applies an [`IrqPolicy`] when taking
//! and releasing it: the kernel implements it with `cli`/restoring `RFLAGS.IF`,
//! so a critical section is never interrupted and therefore never
//! re-entered by a handler that wants the same lock. Host tests use
//! the null policy [`NoIrq`].
//!
//! While waiting, interrupts stay as they were at the call: this way
//! a CPU spinning on a lock held by another CPU keeps servicing IPIs
//! (Phase 32) and no handler gets stuck behind someone else's lock. A lock
//! held for a huge number of spins can only be re-entry (one CPU) or a
//! deadlock between CPUs: `lock` panics instead of hanging.

#![no_std]

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// What to do with interrupts around a critical section.
pub trait IrqPolicy {
    type State: Copy;
    /// Disables interrupts and returns the previous state.
    fn save_and_disable() -> Self::State;
    /// Restores the state returned by `save_and_disable`.
    fn restore(state: Self::State);
}

/// Null policy: a plain spinlock.
pub struct NoIrq;

impl IrqPolicy for NoIrq {
    type State = ();
    fn save_and_disable() {}
    fn restore((): ()) {}
}

/// Spins before declaring a deadlock.
const DEADLOCK_SPINS: u32 = 1 << 26;

pub struct IrqLock<T, P: IrqPolicy = NoIrq> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
    _policy: PhantomData<P>,
}

// SAFETY: access to `data` is serialised by `locked`.
unsafe impl<T: Send, P: IrqPolicy> Sync for IrqLock<T, P> {}
unsafe impl<T: Send, P: IrqPolicy> Send for IrqLock<T, P> {}

pub type SpinLock<T> = IrqLock<T, NoIrq>;

impl<T, P: IrqPolicy> IrqLock<T, P> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
            _policy: PhantomData,
        }
    }

    fn try_acquire(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Takes the lock, busy-waiting if needed. Between attempts the
    /// policy is undone (interrupts return to their state) so that
    /// waiting does not lock out this CPU's handlers.
    ///
    /// # Panics
    /// If it is still held after many spins: re-entry or deadlock.
    pub fn lock(&self) -> IrqGuard<'_, T, P> {
        let mut spins = 0u32;
        loop {
            let state = P::save_and_disable();
            if self.try_acquire() {
                return IrqGuard { lock: self, state };
            }
            P::restore(state);
            spins += 1;
            if spins == DEADLOCK_SPINS {
                panic!("IrqLock: deadlock (re-entry) at {:p}", self);
            }
            core::hint::spin_loop();
        }
    }

    /// Releases the lock without going through its guard or touching the policy. For the
    /// context switch protocol: the switching CPU leaves the lock
    /// held and the resumed thread, which has no guard, releases it.
    ///
    /// # Safety
    ///
    /// The lock must be held and its guard forgotten (`mem::forget`), and the
    /// caller must restore by itself the state the policy saved.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }

    /// Takes the lock only if it is free. Leaves interrupts as they were
    /// if it fails.
    pub fn try_lock(&self) -> Option<IrqGuard<'_, T, P>> {
        let state = P::save_and_disable();
        if self.try_acquire() {
            Some(IrqGuard { lock: self, state })
        } else {
            P::restore(state);
            None
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// Lock-free access when holding `&mut` to the lock itself.
    pub fn get_mut(&mut self) -> &mut T {
        self.data.get_mut()
    }
}

pub struct IrqGuard<'a, T, P: IrqPolicy> {
    lock: &'a IrqLock<T, P>,
    state: P::State,
}

impl<T, P: IrqPolicy> Deref for IrqGuard<'_, T, P> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard owns the lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T, P: IrqPolicy> DerefMut for IrqGuard<'_, T, P> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard owns the lock exclusively.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T, P: IrqPolicy> Drop for IrqGuard<'_, T, P> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        P::restore(self.state);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

    static DISABLED: AtomicUsize = AtomicUsize::new(0);
    static RESTORED: AtomicUsize = AtomicUsize::new(0);

    struct Counting;
    impl IrqPolicy for Counting {
        type State = u32;
        fn save_and_disable() -> u32 {
            DISABLED.fetch_add(1, Ordering::SeqCst);
            42
        }
        fn restore(state: u32) {
            assert_eq!(state, 42);
            RESTORED.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn lock_guard_and_try_lock() {
        let l: SpinLock<u32> = SpinLock::new(1);
        {
            let mut g = l.lock();
            *g += 1;
            assert!(l.is_locked());
            assert!(l.try_lock().is_none(), "held: try_lock fails");
        }
        assert!(!l.is_locked());
        assert_eq!(*l.lock(), 2);
        assert_eq!(*l.try_lock().unwrap(), 2);
    }

    #[test]
    fn policy_is_applied_symmetrically_even_on_failed_try_lock() {
        let l: IrqLock<u8, Counting> = IrqLock::new(0);
        let g = l.lock();
        let _ = l.try_lock();
        drop(g);
        let d = DISABLED.load(Ordering::SeqCst);
        let r = RESTORED.load(Ordering::SeqCst);
        assert_eq!(d, r, "every save_and_disable has its restore");
        assert!(d >= 2);
    }

    #[test]
    fn mutual_exclusion_across_threads() {
        let l = Arc::new(SpinLock::new(0u64));
        let handles: std::vec::Vec<_> = (0..4)
            .map(|_| {
                let l = Arc::clone(&l);
                thread::spawn(move || {
                    for _ in 0..10_000 {
                        *l.lock() += 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*l.lock(), 40_000);
    }

    #[test]
    fn forgotten_guard_is_released_by_force_unlock() {
        // Own counters: `Counting` shares them with another parallel test.
        static D: AtomicUsize = AtomicUsize::new(0);
        static R: AtomicUsize = AtomicUsize::new(0);
        struct Own;
        impl IrqPolicy for Own {
            type State = ();
            fn save_and_disable() {
                D.fetch_add(1, Ordering::SeqCst);
            }
            fn restore((): ()) {
                R.fetch_add(1, Ordering::SeqCst);
            }
        }
        let l: IrqLock<u8, Own> = IrqLock::new(0);
        let (d0, r0) = (0, 0);
        let g = l.lock();
        core::mem::forget(g);
        assert!(l.is_locked());
        assert!(l.try_lock().is_none());
        // SAFETY: the guard was forgotten above.
        unsafe { l.force_unlock() };
        assert!(!l.is_locked());
        assert_eq!(*l.lock(), 0);
        // The forgotten lock disabled once without restoring; try_lock and the
        // final lock do restore theirs.
        assert_eq!(D.load(Ordering::SeqCst) - d0, 3);
        assert_eq!(R.load(Ordering::SeqCst) - r0, 2);
    }

    #[test]
    fn spinning_restores_policy_between_attempts() {
        // A lock held by another thread: the waiter must restore and
        // disable again on every spin (not stay "disabled").
        static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
        struct Spy;
        impl IrqPolicy for Spy {
            type State = ();
            fn save_and_disable() {
                ATTEMPTS.fetch_add(1, Ordering::SeqCst);
            }
            fn restore((): ()) {}
        }
        let l = Arc::new(IrqLock::<u8, Spy>::new(0));
        let g = l.lock();
        let l2 = Arc::clone(&l);
        let waiter = thread::spawn(move || {
            *l2.lock() = 5;
        });
        while ATTEMPTS.load(Ordering::SeqCst) < 50 {
            thread::yield_now();
        }
        drop(g);
        waiter.join().unwrap();
        assert_eq!(*l.lock(), 5);
        assert!(
            ATTEMPTS.load(Ordering::SeqCst) >= 50,
            "one attempt per spin"
        );
    }

    #[test]
    fn get_mut_bypasses_lock() {
        let mut l: SpinLock<u32> = SpinLock::new(5);
        *l.get_mut() = 6;
        assert_eq!(*l.lock(), 6);
    }
}
