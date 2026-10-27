// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Handle table (Phase 12): applications do not hold pointers to kernel
//! objects but opaque handles with rights.
//!
//! A [`Handle`] encodes index and generation: a closed handle stops being
//! valid even if its index is reused, and a made-up value is only valid
//! if it guesses both index and generation. Rights can only be reduced when
//! duplicating, never extended.
//!
//! A closed value is not handed out again before its position has gone
//! through all its other values (ADR 0057): every position is used before
//! one is reused, the one free the longest goes first, and a position
//! moves on to another index when its generations run out. With the
//! kernel's 256 positions that is 255 or 256 * 65535 reuses of the
//! position: between about 1.7 * 10^7 closes in the table (all other
//! positions held) and about 4 * 10^9 (all free).

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
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

/// Opaque handle: index + 1 in the low 16 bits, generation in the high
/// ones. `Handle(0)` is never valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(pub u32);

/// Indexes a handle can carry, `0..INDEXES`: the low half of a handle is
/// at most `0xFFFE`, so no handle is `u32::MAX` (`endpoint_create_pair`
/// packs two handles into a result that must never read as an error;
/// ADR 0030, ADR 0057).
const INDEXES: usize = 0xFFFE;

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

/// A place for one entry. Position `p` of a table of `limit` positions
/// hands out the handles `(p + limit * epoch, generation)`, so its indexes
/// never meet another position's.
struct Position<T> {
    entry: Option<(T, Rights)>,
    /// Which of the position's indexes the next entry gets.
    epoch: u16,
    /// Generation the next entry gets. Never 0: `Handle(0)` and handles
    /// with generation 0 never match a live one.
    generation: u16,
}

/// The value after `(epoch, generation)` of position `position` in a
/// table of `stride` positions: the next generation, then the first
/// generation of the position's next index, and back to its first index
/// only once every index of the position has had every generation.
const fn next_value(position: usize, stride: usize, epoch: u16, generation: u16) -> (u16, u16) {
    if generation < u16::MAX {
        return (epoch, generation + 1);
    }
    let next = epoch as usize + 1;
    if position + stride * next < INDEXES {
        (next as u16, 1)
    } else {
        (0, 1)
    }
}

pub struct HandleTable<T> {
    positions: Vec<Position<T>>,
    /// Positions without an entry, the one free the longest first.
    free: VecDeque<usize>,
    limit: usize,
    live: usize,
}

impl<T> HandleTable<T> {
    /// A table of at most `limit` live handles.
    ///
    /// # Panics
    /// If `limit` is over the indexes a handle can carry (65534).
    pub fn new(limit: usize) -> Self {
        assert!(limit <= INDEXES, "handle table limit over {INDEXES}");
        Self {
            positions: Vec::new(),
            free: VecDeque::new(),
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

    /// Handles that can still be inserted.
    pub fn room(&self) -> usize {
        self.limit - self.live
    }

    pub fn insert(&mut self, object: T, rights: Rights) -> Result<Handle, HandleError> {
        if !rights.is_valid() {
            return Err(HandleError::Escalation);
        }
        if self.live >= self.limit {
            return Err(HandleError::TableFull);
        }
        // Every position is used once before any is reused, and then the
        // one free the longest goes first: a closed value waits as long as
        // the table allows before its position comes round again.
        let position = if self.positions.len() < self.limit {
            self.positions.push(Position {
                entry: None,
                epoch: 0,
                generation: 1,
            });
            self.positions.len() - 1
        } else {
            self.free.pop_front().ok_or(HandleError::TableFull)?
        };
        let p = &mut self.positions[position];
        p.entry = Some((object, rights));
        self.live += 1;
        Ok(Handle::new(
            position + self.limit * p.epoch as usize,
            p.generation,
        ))
    }

    /// Position of a live handle.
    fn position(&self, handle: Handle) -> Result<usize, HandleError> {
        let index = handle.index().ok_or(HandleError::BadHandle)?;
        let position = index
            .checked_rem(self.limit)
            .ok_or(HandleError::BadHandle)?;
        match self.positions.get(position) {
            Some(p)
                if p.entry.is_some()
                    && p.epoch as usize == index / self.limit
                    && p.generation == handle.generation() =>
            {
                Ok(position)
            }
            _ => Err(HandleError::BadHandle),
        }
    }

    fn entry(&self, handle: Handle) -> Result<&(T, Rights), HandleError> {
        let position = self.position(handle)?;
        self.positions[position]
            .entry
            .as_ref()
            .ok_or(HandleError::BadHandle)
    }

    /// Live entries with their rights, in position order.
    pub fn iter(&self) -> impl Iterator<Item = (&T, Rights)> {
        self.positions
            .iter()
            .filter_map(|p| p.entry.as_ref())
            .map(|(object, rights)| (object, *rights))
    }

    /// Object and rights of a valid handle.
    pub fn get(&self, handle: Handle) -> Result<(&T, Rights), HandleError> {
        let (object, rights) = self.entry(handle)?;
        Ok((object, *rights))
    }

    /// Object of a handle that also holds `required`.
    pub fn get_checked(&self, handle: Handle, required: Rights) -> Result<&T, HandleError> {
        let (object, rights) = self.entry(handle)?;
        if rights.contains(required) {
            Ok(object)
        } else {
            Err(HandleError::Denied)
        }
    }

    /// Closes the handle and returns the object. Its position moves on to
    /// its next value: this handle value is not handed out again before
    /// the position has gone through all of its others.
    pub fn remove(&mut self, handle: Handle) -> Result<T, HandleError> {
        let position = self.position(handle)?;
        let stride = self.limit;
        let p = &mut self.positions[position];
        let (object, _) = p.entry.take().ok_or(HandleError::BadHandle)?;
        (p.epoch, p.generation) = next_value(position, stride, p.epoch, p.generation);
        self.free.push_back(position);
        self.live -= 1;
        Ok(object)
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
    /// the source's; the source needs `DUPLICATE`. `make` builds the entry of
    /// the new handle from the source's entry and the new rights (the kernel
    /// accounts endpoint sides per capability, ADR 0030); it only runs once
    /// the rights are checked.
    pub fn duplicate(
        &mut self,
        handle: Handle,
        rights: Rights,
        make: impl FnOnce(&T, Rights) -> T,
    ) -> Result<Handle, HandleError> {
        let (object, have) = self.get(handle)?;
        if !have.contains(Rights::DUPLICATE) {
            return Err(HandleError::Denied);
        }
        if !rights.is_valid() || !have.contains(rights) {
            return Err(HandleError::Escalation);
        }
        let object = make(object, rights);
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
        // A table of one position reuses the index with another
        // generation: the old handle stays dead.
        let mut one: HandleTable<u32> = HandleTable::new(1);
        let h = one.insert(7, Rights::READ).unwrap();
        one.remove(h).unwrap();
        let h2 = one.insert(8, Rights::READ).unwrap();
        assert_eq!(h2.index(), h.index());
        assert_ne!(h2, h);
        assert_eq!(one.get(h), Err(HandleError::BadHandle));
        assert_eq!(one.get(h2).unwrap().0, &8);
    }

    #[test]
    fn a_stale_handle_never_resolves_again() {
        // A program opens and closes one handle in a loop while another
        // part of it still holds an old value (incident 0047). Before, the
        // same index came back at once and its 16-bit generation wrapped
        // after 65535 reuses: the old value named the new object.
        for limit in [1, 4, 256] {
            let mut t: HandleTable<u32> = HandleTable::new(limit);
            let stale = t.insert(0, Rights::READ).unwrap();
            t.remove(stale).unwrap();
            for n in 1..=3 * 0x1_0000u32 {
                let h = t.insert(n, Rights::READ).unwrap();
                assert_ne!(
                    h, stale,
                    "limit {limit}: the closed value came back after {n} reuses"
                );
                assert_eq!(t.get(stale), Err(HandleError::BadHandle));
                t.remove(h).unwrap();
            }
        }
    }

    #[test]
    fn every_position_is_used_before_one_is_reused() {
        let mut t: HandleTable<u32> = HandleTable::new(4);
        let held = t.insert(0, Rights::READ).unwrap();
        let mut seen = Vec::new();
        for n in 0..9 {
            let h = t.insert(n, Rights::READ).unwrap();
            seen.push(h.index().unwrap() % 4);
            t.remove(h).unwrap();
        }
        // Position 0 stays held; the others take turns, oldest free first.
        assert_eq!(seen, [1, 2, 3, 1, 2, 3, 1, 2, 3]);
        assert_eq!(t.get(held).unwrap().0, &0);
    }

    #[test]
    fn a_position_goes_through_all_its_values_before_repeating() {
        // Position 1 of a table of 0x5000 positions has the indexes 0x0001,
        // 0x5001, 0xA001 and 0xF001: 4 * 65535 values, each given once, no
        // index 0xFFFE or above (the low half never reads 0xFFFF).
        let (position, stride) = (1, 0x5000);
        let mut seen = alloc::collections::BTreeSet::new();
        let (mut epoch, mut generation) = (0u16, 1u16);
        loop {
            let index = position + stride * epoch as usize;
            assert!(index < INDEXES);
            assert!(seen.insert(Handle::new(index, generation).0));
            (epoch, generation) = next_value(position, stride, epoch, generation);
            if (epoch, generation) == (0, 1) {
                break;
            }
        }
        assert_eq!(seen.len(), 4 * 0xFFFF);
        assert!(!seen.contains(&u32::MAX));
    }

    #[test]
    fn handle_values_of_a_fresh_table_are_unchanged() {
        // The ABI's fixed handles (process, thread, console) and the ones a
        // spawn passes after them keep their values.
        let mut t: HandleTable<u32> = HandleTable::new(256);
        let values: Vec<u32> = (0..5)
            .map(|n| t.insert(n, Rights::READ).unwrap().0)
            .collect();
        assert_eq!(values, [0x10001, 0x10002, 0x10003, 0x10004, 0x10005]);
    }

    #[test]
    #[should_panic(expected = "handle table limit")]
    fn a_limit_beyond_the_indexes_is_refused() {
        let _ = HandleTable::<u32>::new(INDEXES + 1);
    }

    #[test]
    fn iter_lists_live_entries_only() {
        let mut t: HandleTable<u32> = HandleTable::new(8);
        let a = t.insert(1, Rights::READ).unwrap();
        t.insert(2, Rights::WRITE).unwrap();
        t.insert(3, Rights::READ).unwrap();
        t.insert(4, Rights::MANAGE).unwrap();
        t.remove(a).unwrap();
        let live: Vec<(u32, Rights)> = t.iter().map(|(o, r)| (*o, r)).collect();
        assert_eq!(
            live,
            [(2, Rights::WRITE), (3, Rights::READ), (4, Rights::MANAGE)]
        );
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
        let copy = |o: &u32, _: Rights| *o;
        let ro = t.duplicate(h, Rights::READ, copy).unwrap();
        assert_eq!(t.get(ro).unwrap().1, Rights::READ);
        assert_eq!(
            t.duplicate(h, Rights::MANAGE, copy),
            Err(HandleError::Escalation)
        );
        assert_eq!(
            t.duplicate(ro, Rights::READ, copy),
            Err(HandleError::Denied),
            "without DUPLICATE"
        );
        assert_eq!(
            t.duplicate(h, Rights(0x8000_0000), copy),
            Err(HandleError::Escalation),
            "invalid bits"
        );
        assert_eq!(t.insert(2, Rights(0x100)), Err(HandleError::Escalation));
    }

    #[test]
    fn duplicate_builds_the_entry_from_the_new_rights() {
        let mut t: HandleTable<(u32, Rights)> = HandleTable::new(8);
        let all = Rights::READ.union(Rights::WRITE).union(Rights::DUPLICATE);
        let h = t.insert((5, all), all).unwrap();
        let w = t.duplicate(h, Rights::WRITE, |o, r| (o.0, r)).unwrap();
        assert_eq!(t.get(w).unwrap(), (&(5, Rights::WRITE), Rights::WRITE));
        let mut called = false;
        let denied = t.duplicate(h, Rights::MANAGE, |o, r| {
            called = true;
            (o.0, r)
        });
        assert_eq!(denied, Err(HandleError::Escalation));
        assert!(!called, "no entry is built for a refused duplicate");
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
