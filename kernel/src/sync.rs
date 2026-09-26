//! Primitivas de sincronización del kernel.
//!
//! Hasta la Fase 9 (threads) hay una sola CPU y las interrupciones externas
//! están deshabilitadas: `SingleCpu` expresa ese régimen. Se sustituirá por
//! un spinlock consciente de interrupciones cuando deje de ser cierto.

use core::cell::UnsafeCell;

/// Celda global mutable para un régimen de una sola CPU sin concurrencia.
pub struct SingleCpu<T>(UnsafeCell<T>);

// SAFETY: el acceso es estrictamente secuencial mientras solo exista una CPU y
// no haya interrupciones habilitadas; los llamadores de `get` no deben crear
// dos `&mut` vivos a la vez (documentado en cada uso).
unsafe impl<T> Sync for SingleCpu<T> {}

impl<T> SingleCpu<T> {
    pub const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// Puntero al contenido. El llamador garantiza exclusividad.
    pub fn get(&self) -> *mut T {
        self.0.get()
    }
}
