//! Tabla de handles (Fase 12): las aplicaciones no manejan punteros a objetos
//! del kernel sino handles opacos con derechos.
//!
//! Un [`Handle`] codifica índice y generación: un handle cerrado deja de ser
//! válido aunque su índice se reutilice, y un valor inventado solo es válido
//! si acierta índice y generación. Los derechos solo pueden reducirse al
//! duplicar, nunca ampliarse.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Derechos sobre un objeto. Conjunto pequeño y explícito (sección 25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    pub const READ: Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);
    pub const DUPLICATE: Rights = Rights(1 << 2);
    pub const TRANSFER: Rights = Rights(1 << 3);
    pub const WAIT: Rights = Rights(1 << 4);
    pub const SIGNAL: Rights = Rights(1 << 5);
    pub const MANAGE: Rights = Rights(1 << 6);
    pub const ALL: Rights = Rights(0x7F);

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }
    /// `true` si todos los bits están definidos.
    pub const fn is_valid(self) -> bool {
        self.0 & !Self::ALL.0 == 0
    }
}

/// Handle opaco: índice en los 16 bits bajos, generación en los altos.
/// `Handle(0)` nunca es válido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(pub u32);

impl Handle {
    pub const INVALID: Handle = Handle(0);
    const fn new(index: usize, generation: u16) -> Handle {
        Handle(((generation as u32) << 16) | (index as u32 + 1))
    }
    const fn index(self) -> Option<usize> {
        match (self.0 & 0xFFFF) as usize {
            0 => None,
            i => Some(i - 1),
        }
    }
    const fn generation(self) -> u16 {
        (self.0 >> 16) as u16
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// No existe, está cerrado o es inventado.
    BadHandle,
    /// El handle no tiene los derechos requeridos.
    Denied,
    /// Se intentó obtener derechos que el origen no tiene, o bits inválidos.
    Escalation,
    /// Límite de handles del proceso alcanzado (sección 52).
    TableFull,
}

struct Slot<T> {
    object: T,
    rights: Rights,
    generation: u16,
}

pub struct HandleTable<T> {
    slots: Vec<Option<Slot<T>>>,
    /// Generación que recibirá la próxima inserción en cada índice.
    generations: Vec<u16>,
    free: Vec<usize>,
    limit: usize,
    live: usize,
}

impl<T> HandleTable<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            limit,
            live: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn insert(&mut self, object: T, rights: Rights) -> Result<Handle, HandleError> {
        if !rights.is_valid() {
            return Err(HandleError::Escalation);
        }
        if self.live >= self.limit {
            return Err(HandleError::TableFull);
        }
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(None);
                // La generación 0 se salta: así `Handle(0)` y los handles con
                // generación 0 nunca coinciden con uno vivo.
                self.generations.push(1);
                self.slots.len() - 1
            }
        };
        let generation = self.generations[index];
        self.slots[index] = Some(Slot {
            object,
            rights,
            generation,
        });
        self.live += 1;
        Ok(Handle::new(index, generation))
    }

    fn slot(&self, handle: Handle) -> Result<&Slot<T>, HandleError> {
        let index = handle.index().ok_or(HandleError::BadHandle)?;
        match self.slots.get(index) {
            Some(Some(slot)) if slot.generation == handle.generation() => Ok(slot),
            _ => Err(HandleError::BadHandle),
        }
    }

    /// Objeto y derechos de un handle válido.
    pub fn get(&self, handle: Handle) -> Result<(&T, Rights), HandleError> {
        let slot = self.slot(handle)?;
        Ok((&slot.object, slot.rights))
    }

    /// Objeto de un handle que además posee `required`.
    pub fn get_checked(&self, handle: Handle, required: Rights) -> Result<&T, HandleError> {
        let slot = self.slot(handle)?;
        if slot.rights.contains(required) {
            Ok(&slot.object)
        } else {
            Err(HandleError::Denied)
        }
    }

    /// Cierra el handle y devuelve el objeto. El índice queda invalidado
    /// para ese valor de handle para siempre (nueva generación).
    pub fn remove(&mut self, handle: Handle) -> Result<T, HandleError> {
        let index = self.slot(handle).map(|_| handle.index().unwrap_or(0))?;
        let slot = self.slots[index].take().expect("slot comprobado");
        self.generations[index] = self.generations[index].wrapping_add(1).max(1);
        self.free.push(index);
        self.live -= 1;
        Ok(slot.object)
    }

    /// Nuevo handle al mismo objeto con `rights`, que debe ser subconjunto de
    /// los del origen; el origen necesita `DUPLICATE`.
    pub fn duplicate(&mut self, handle: Handle, rights: Rights) -> Result<Handle, HandleError>
    where
        T: Clone,
    {
        let (object, have) = self.get(handle)?;
        if !have.contains(Rights::DUPLICATE) {
            return Err(HandleError::Denied);
        }
        if !rights.is_valid() || !have.contains(rights) {
            return Err(HandleError::Escalation);
        }
        let object = object.clone();
        self.insert(object, rights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_and_rights() {
        let mut t: HandleTable<&str> = HandleTable::new(8);
        let h = t
            .insert("consola", Rights::WRITE.union(Rights::DUPLICATE))
            .unwrap();
        assert_ne!(h, Handle::INVALID);
        assert_eq!(t.get(h).unwrap(), (&"consola", Rights(0b110)));
        assert_eq!(t.get_checked(h, Rights::WRITE).unwrap(), &"consola");
        assert_eq!(t.get_checked(h, Rights::READ), Err(HandleError::Denied));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn forged_and_stale_handles_are_rejected() {
        let mut t: HandleTable<u32> = HandleTable::new(8);
        let h = t.insert(7, Rights::READ).unwrap();
        assert_eq!(t.get(Handle::INVALID), Err(HandleError::BadHandle));
        assert_eq!(t.get(Handle(0x1234)), Err(HandleError::BadHandle));
        assert_eq!(
            t.get(Handle(h.0 ^ 0x10000)),
            Err(HandleError::BadHandle),
            "generacion incorrecta"
        );
        assert_eq!(t.remove(h), Ok(7));
        assert_eq!(t.get(h), Err(HandleError::BadHandle), "cerrado");
        assert_eq!(t.remove(h), Err(HandleError::BadHandle));
        // El indice se reutiliza con otra generacion: el handle viejo sigue muerto.
        let h2 = t.insert(8, Rights::READ).unwrap();
        assert_eq!(h2.index(), h.index());
        assert_ne!(h2, h);
        assert_eq!(t.get(h), Err(HandleError::BadHandle));
        assert_eq!(t.get(h2).unwrap().0, &8);
    }

    #[test]
    fn duplicate_only_reduces_rights() {
        let mut t: HandleTable<u32> = HandleTable::new(8);
        let h = t
            .insert(
                1,
                Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE),
            )
            .unwrap();
        let ro = t.duplicate(h, Rights::READ).unwrap();
        assert_eq!(t.get(ro).unwrap().1, Rights::READ);
        assert_eq!(t.duplicate(h, Rights::MANAGE), Err(HandleError::Escalation));
        assert_eq!(
            t.duplicate(ro, Rights::READ),
            Err(HandleError::Denied),
            "sin DUPLICATE"
        );
        assert_eq!(
            t.duplicate(h, Rights(0x8000_0000)),
            Err(HandleError::Escalation),
            "bits invalidos"
        );
        assert_eq!(t.insert(2, Rights(0x100)), Err(HandleError::Escalation));
    }

    #[test]
    fn table_limit() {
        let mut t: HandleTable<u8> = HandleTable::new(2);
        t.insert(1, Rights::NONE).unwrap();
        let h = t.insert(2, Rights::NONE).unwrap();
        assert_eq!(t.insert(3, Rights::NONE), Err(HandleError::TableFull));
        t.remove(h).unwrap();
        assert!(t.insert(3, Rights::NONE).is_ok());
    }
}
