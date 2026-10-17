// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Handle table (Phase 12): applications do not hold pointers to kernel
//! objects but opaque handles with rights.
//!
//! A [`Handle`] encodes index and generation: a closed handle stops being
//! valid even if its index is reused, and a made-up value is only valid
//! if it guesses both index and generation. Rights can only be reduced when
//! duplicating, never extended.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Rights over an object. Small, explicit set (section 25).
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
    /// `true` if every bit is defined.
    pub const fn is_valid(self) -> bool {
        self.0 & !Self::ALL.0 == 0
    }
}

/// Opaque handle: index in the low 16 bits, generation in the high ones.
/// `Handle(0)` is never valid.
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
    /// Does not exist, is closed or was made up.
    BadHandle,
    /// The handle lacks the required rights.
    Denied,
    /// Attempt to obtain rights the source does not have, or invalid bits.
    Escalation,
    /// Per-process handle limit reached (section 52).
    TableFull,
}

struct Slot<T> {
    object: T,
    rights: Rights,
    generation: u16,
}

pub struct HandleTable<T> {
    slots: Vec<Option<Slot<T>>>,
    /// Generation the next insertion at each index will receive.
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
                // Generation 0 is skipped: this way `Handle(0)` and handles with
                // generation 0 never match a live one.
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

    /// Object and rights of a valid handle.
    pub fn get(&self, handle: Handle) -> Result<(&T, Rights), HandleError> {
        let slot = self.slot(handle)?;
        Ok((&slot.object, slot.rights))
    }

    /// Object of a handle that also holds `required`.
    pub fn get_checked(&self, handle: Handle, required: Rights) -> Result<&T, HandleError> {
        let slot = self.slot(handle)?;
        if slot.rights.contains(required) {
            Ok(&slot.object)
        } else {
            Err(HandleError::Denied)
        }
    }

    /// Closes the handle and returns the object. The index is invalidated
    /// for that handle value forever (new generation).
    pub fn remove(&mut self, handle: Handle) -> Result<T, HandleError> {
        let index = self.slot(handle).map(|_| handle.index().unwrap_or(0))?;
        let slot = self.slots[index].take().expect("slot checked");
        self.generations[index] = self.generations[index].wrapping_add(1).max(1);
        self.free.push(index);
        self.live -= 1;
        Ok(slot.object)
    }

    /// Removes the handle to transfer it to another table: requires `TRANSFER`.
    /// Returns the object and its rights (kept as they are: without `TRANSFER` the
    /// receiver could not forward it unless explicitly granted).
    pub fn take(&mut self, handle: Handle) -> Result<(T, Rights), HandleError> {
        let rights = self.get(handle)?.1;
        if !rights.contains(Rights::TRANSFER) {
            return Err(HandleError::Denied);
        }
        let object = self.remove(handle)?;
        Ok((object, rights))
    }

    /// New handle to the same object with `rights`, which must be a subset of
    /// the source's; the source needs `DUPLICATE`.
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
            .insert("console", Rights::WRITE.union(Rights::DUPLICATE))
            .unwrap();
        assert_ne!(h, Handle::INVALID);
        assert_eq!(t.get(h).unwrap(), (&"console", Rights(0b110)));
        assert_eq!(t.get_checked(h, Rights::WRITE).unwrap(), &"console");
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
            "wrong generation"
        );
        assert_eq!(t.remove(h), Ok(7));
        assert_eq!(t.get(h), Err(HandleError::BadHandle), "closed");
        assert_eq!(t.remove(h), Err(HandleError::BadHandle));
        // The index is reused with another generation: the old handle stays dead.
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
            "without DUPLICATE"
        );
        assert_eq!(
            t.duplicate(h, Rights(0x8000_0000)),
            Err(HandleError::Escalation),
            "invalid bits"
        );
        assert_eq!(t.insert(2, Rights(0x100)), Err(HandleError::Escalation));
    }

    #[test]
    fn take_requires_transfer_and_moves_the_handle() {
        let mut a: HandleTable<u32> = HandleTable::new(4);
        let mut b: HandleTable<u32> = HandleTable::new(4);
        let fixed = a.insert(1, Rights::READ).unwrap();
        assert_eq!(a.take(fixed), Err(HandleError::Denied));
        let movable = a.insert(2, Rights::READ.union(Rights::TRANSFER)).unwrap();
        let (obj, rights) = a.take(movable).unwrap();
        assert_eq!(
            a.get(movable),
            Err(HandleError::BadHandle),
            "no longer in the source"
        );
        let hb = b.insert(obj, rights).unwrap();
        assert_eq!(
            b.get(hb).unwrap(),
            (&2, Rights::READ.union(Rights::TRANSFER))
        );
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
