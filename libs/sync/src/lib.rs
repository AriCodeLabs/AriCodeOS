//! Primitivas de exclusión mutua del kernel.
//!
//! [`IrqLock`] es un spinlock que, además, aplica una [`IrqPolicy`] al tomar
//! y soltar: el kernel la implementa con `cli`/restauración de `RFLAGS.IF`,
//! de modo que una sección crítica nunca es interrumpida ni, por tanto,
//! reentrada por un handler que quiera el mismo lock. Los tests de host usan
//! la política nula [`NoIrq`].
//!
//! Con una sola CPU y las interrupciones deshabilitadas dentro del lock, la
//! única forma de encontrarlo tomado es la reentrada: por eso `lock` entra en
//! pánico tras un número alto de giros en vez de colgar en silencio.

#![no_std]

use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Qué hacer con las interrupciones alrededor de una sección crítica.
pub trait IrqPolicy {
    type State: Copy;
    /// Deshabilita interrupciones y devuelve el estado previo.
    fn save_and_disable() -> Self::State;
    /// Restaura el estado devuelto por `save_and_disable`.
    fn restore(state: Self::State);
}

/// Política nula: un spinlock corriente.
pub struct NoIrq;

impl IrqPolicy for NoIrq {
    type State = ();
    fn save_and_disable() {}
    fn restore((): ()) {}
}

/// Giros antes de declarar un bloqueo mutuo.
const DEADLOCK_SPINS: u32 = 1 << 26;

pub struct IrqLock<T, P: IrqPolicy = NoIrq> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
    _policy: PhantomData<P>,
}

// SAFETY: el acceso a `data` está serializado por `locked`.
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

    /// Toma el lock, esperando activamente si hace falta.
    ///
    /// # Panics
    /// Si tras muchos giros sigue tomado: en una CPU eso es reentrada.
    pub fn lock(&self) -> IrqGuard<'_, T, P> {
        let state = P::save_and_disable();
        let mut spins = 0u32;
        while !self.try_acquire() {
            spins += 1;
            if spins == DEADLOCK_SPINS {
                P::restore(state);
                panic!("IrqLock: bloqueo mutuo (reentrada) en {:p}", self);
            }
            core::hint::spin_loop();
        }
        IrqGuard { lock: self, state }
    }

    /// Toma el lock solo si está libre. Deja las interrupciones como estaban
    /// si no lo consigue.
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

    /// Acceso sin lock para cuando se tiene `&mut` al propio lock.
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
        // SAFETY: el guard posee el lock.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T, P: IrqPolicy> DerefMut for IrqGuard<'_, T, P> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: el guard posee el lock en exclusiva.
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
            assert!(l.try_lock().is_none(), "tomado: try_lock falla");
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
        assert_eq!(d, r, "cada save_and_disable tiene su restore");
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
    fn get_mut_bypasses_lock() {
        let mut l: SpinLock<u32> = SpinLock::new(5);
        *l.get_mut() = 6;
        assert_eq!(*l.lock(), 6);
    }
}
