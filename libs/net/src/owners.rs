// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Socket tables whose entries belong to a client (ADR 0037). The network
//! service keeps one [`Owned`] table per kind of socket or wait (TCP, UDP,
//! ping, DNS); every entry records the *owner*, the client endpoint the
//! request that created it arrived on, so that an index only answers the
//! owner that holds it (a client cannot send on, receive from or close
//! another one's socket by guessing numbers) and everything an owner holds
//! can be released at once when the kernel reports that its client is gone
//! (incident 0016). Pure and host-tested; the service connects it to IPC.

/// Who holds an entry: 0 is the service's main endpoint, `slot + 1` the
/// client session in `slot`, [`NOBODY`] the service itself.
pub type Owner = usize;

/// An entry no client holds: the service keeps it until the protocol is
/// done with it (a connection its client closed and then died on, waiting
/// out TIME-WAIT). No request can reach it, since no session has this
/// number, and it counts for nobody in the table per owner.
pub const NOBODY: Owner = usize::MAX;

/// Fixed table of `N` slots, each free or held by an owner.
pub struct Owned<T, const N: usize> {
    slots: [Option<(Owner, T)>; N],
}

impl<T, const N: usize> Default for Owned<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Owned<T, N> {
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; N],
        }
    }

    /// Stores `value` for `owner` in the first free slot and returns its
    /// index; `None` when the table is full.
    pub fn insert(&mut self, owner: Owner, value: T) -> Option<usize> {
        let slot = self.free_slot()?;
        self.slots[slot] = Some((owner, value));
        Some(slot)
    }

    /// First free slot, if any.
    pub fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }

    /// The entry at `index`, only for the owner that holds it.
    pub fn get(&self, index: usize, owner: Owner) -> Option<&T> {
        match self.slots.get(index)?.as_ref() {
            Some((o, v)) if *o == owner => Some(v),
            _ => None,
        }
    }

    /// Like [`Owned::get`], mutable.
    pub fn get_mut(&mut self, index: usize, owner: Owner) -> Option<&mut T> {
        match self.slots.get_mut(index)?.as_mut() {
            Some((o, v)) if *o == owner => Some(v),
            _ => None,
        }
    }

    /// Removes the entry at `index` if `owner` holds it (a close by its
    /// client); `None` otherwise.
    pub fn remove(&mut self, index: usize, owner: Owner) -> Option<T> {
        let slot = self.slots.get_mut(index)?;
        if slot.as_ref().is_some_and(|(o, _)| *o == owner) {
            slot.take().map(|(_, v)| v)
        } else {
            None
        }
    }

    /// Who holds `index`, if anybody.
    pub fn owner_of(&self, index: usize) -> Option<Owner> {
        self.slots.get(index)?.as_ref().map(|(o, _)| *o)
    }

    /// The entry at `index` whoever holds it: for the service's own
    /// bookkeeping (demultiplexing a segment, polling a connection), never
    /// for a client's request.
    pub fn at(&self, index: usize) -> Option<&T> {
        self.slots.get(index)?.as_ref().map(|(_, v)| v)
    }

    /// Like [`Owned::at`], mutable.
    pub fn at_mut(&mut self, index: usize) -> Option<&mut T> {
        self.slots.get_mut(index)?.as_mut().map(|(_, v)| v)
    }

    /// Takes the entry at `index` out of the table, whoever holds it, with
    /// its owner: the service's own reaping, or a connection taken out while
    /// its segments are emitted and put back with [`Owned::set`].
    pub fn take(&mut self, index: usize) -> Option<(Owner, T)> {
        self.slots.get_mut(index)?.take()
    }

    /// Puts `value` in `index` for `owner`, replacing whatever was there.
    pub fn set(&mut self, index: usize, owner: Owner, value: T) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = Some((owner, value));
        }
    }

    /// Hands the entry at `index` to [`NOBODY`]: its owner is gone and the
    /// service finishes with it on its own.
    pub fn disown(&mut self, index: usize) {
        if let Some(Some((owner, _))) = self.slots.get_mut(index) {
            *owner = NOBODY;
        }
    }

    /// Every held entry as `(index, owner, value)`.
    pub fn iter(&self) -> impl Iterator<Item = (usize, Owner, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|(o, v)| (i, *o, v)))
    }

    /// Like [`Owned::iter`], mutable.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (usize, Owner, &mut T)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| s.as_mut().map(|(o, v)| (i, *o, v)))
    }

    /// Releases every entry of `owner` (its client is gone), handing each
    /// one to `release`, and returns how many there were.
    pub fn reclaim(&mut self, owner: Owner, mut release: impl FnMut(usize, T)) -> usize {
        let mut n = 0;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|(o, _)| *o == owner) {
                if let Some((_, v)) = slot.take() {
                    release(i, v);
                    n += 1;
                }
            }
        }
        n
    }

    /// Entries `owner` holds.
    pub fn owned_by(&self, owner: Owner) -> usize {
        self.iter().filter(|(_, o, _)| *o == owner).count()
    }

    /// Entries in use, every owner together.
    pub fn used(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub fn is_full(&self) -> bool {
        self.used() == N
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn an_entry_answers_only_its_owner() {
        let mut t = Owned::<&str, 4>::new();
        let i = t.insert(1, "shell").unwrap();
        assert_eq!(t.get(i, 1), Some(&"shell"));
        assert_eq!(t.get(i, 2), None, "another client must not see it");
        assert_eq!(t.get_mut(i, 2), None);
        assert_eq!(t.remove(i, 2), None, "nor close it (BadSock)");
        assert_eq!(t.owner_of(i), Some(1));
        assert_eq!(t.at(i), Some(&"shell"), "the service itself sees it");
        assert_eq!(t.remove(i, 1), Some("shell"));
        assert_eq!(t.get(i, 1), None);
        assert_eq!(t.remove(i, 1), None, "closing twice is BadSock");
        assert_eq!(t.owner_of(i), None);
    }

    #[test]
    fn a_full_table_refuses_and_frees_on_remove() {
        let mut t = Owned::<u8, 4>::new();
        for i in 0..4 {
            assert_eq!(t.insert(0, i), Some(i as usize));
        }
        assert!(t.is_full());
        assert_eq!(t.insert(0, 9), None);
        assert_eq!(t.insert(7, 9), None);
        assert_eq!(t.free_slot(), None);
        assert_eq!(t.remove(2, 0), Some(2));
        assert_eq!(t.free_slot(), Some(2));
        assert_eq!(t.insert(7, 9), Some(2), "the freed slot is reused");
        assert_eq!(t.get(2, 7), Some(&9));
        assert_eq!(
            t.get(2, 0),
            None,
            "the old owner does not see the new entry"
        );
    }

    /// Incident 0016: everything a dead client held goes at once; the other
    /// clients' entries stay where they were.
    #[test]
    fn reclaim_frees_one_owner_only() {
        let mut t = Owned::<u8, 8>::new();
        let shell = t.insert(1, 10).unwrap();
        for i in 0..5 {
            t.insert(2, 20 + i).unwrap();
        }
        let other = t.insert(3, 30).unwrap();
        assert_eq!(t.used(), 7);
        assert_eq!(t.owned_by(2), 5);
        let mut released = Vec::new();
        assert_eq!(
            t.reclaim(2, |i, v| released.push((i, v))),
            5,
            "the count is what the fault event reports"
        );
        assert_eq!(released.len(), 5);
        assert!(released.iter().all(|(_, v)| (20..25).contains(v)));
        assert_eq!(t.reclaim(2, |_, _| {}), 0, "nothing left to reclaim");
        assert_eq!(t.owned_by(2), 0);
        assert_eq!(t.used(), 2);
        assert_eq!(t.get(shell, 1), Some(&10));
        assert_eq!(t.get(other, 3), Some(&30));
        // A new client given the dead one's owner number starts clean.
        assert_eq!(t.get(1, 2), None);
        assert_eq!(t.owned_by(2), 0);
    }

    #[test]
    fn reclaim_makes_room_in_a_full_table() {
        let mut t = Owned::<u8, 4>::new();
        for i in 0..4 {
            t.insert(5, i).unwrap();
        }
        assert_eq!(t.insert(1, 0), None);
        assert_eq!(t.reclaim(5, |_, _| {}), 4);
        assert_eq!(t.insert(1, 0), Some(0));
        assert_eq!(t.used(), 1);
    }

    /// The service takes a connection out to emit its segments and puts it
    /// back under the same owner; the owner never notices.
    #[test]
    fn take_and_set_keep_the_owner() {
        let mut t = Owned::<u8, 2>::new();
        let i = t.insert(4, 1).unwrap();
        let (owner, v) = t.take(i).unwrap();
        assert_eq!((owner, v), (4, 1));
        assert_eq!(t.get(i, 4), None, "gone while taken");
        t.set(i, owner, v + 1);
        assert_eq!(t.get(i, 4), Some(&2));
        assert_eq!(t.take(5), None, "out of range");
        t.set(5, 4, 0);
        assert_eq!(t.used(), 1);
    }

    #[test]
    fn iteration_reports_index_and_owner() {
        let mut t = Owned::<u8, 4>::new();
        t.insert(1, 11).unwrap();
        t.insert(2, 22).unwrap();
        t.remove(0, 1);
        t.insert(3, 33).unwrap();
        let mut seen = Vec::new();
        for (i, o, v) in t.iter() {
            seen.push((i, (o, *v)));
        }
        assert_eq!(seen.as_slice(), &[(0, (3, 33)), (1, (2, 22))]);
        for (_, _, v) in t.iter_mut() {
            *v += 1;
        }
        assert_eq!(t.at(0), Some(&34));
        assert_eq!(t.at_mut(1), Some(&mut 23));
    }

    /// A connection its client closed before dying is nobody's: it stays in
    /// the table until the protocol is done, counts for no owner and is not
    /// reclaimed again.
    #[test]
    fn a_disowned_entry_belongs_to_nobody() {
        let mut t = Owned::<u8, 4>::new();
        let i = t.insert(2, 5).unwrap();
        t.insert(2, 6).unwrap();
        t.disown(i);
        assert_eq!(t.owner_of(i), Some(NOBODY));
        assert_eq!(t.get(i, 2), None);
        assert_eq!(t.owned_by(2), 1);
        assert_eq!(t.owned_by(NOBODY), 1);
        assert_eq!(t.used(), 2);
        assert_eq!(t.reclaim(2, |_, _| {}), 1);
        assert_eq!(t.at(i), Some(&5), "still there for the service");
        t.disown(9);
        assert_eq!(t.used(), 1);
    }

    #[test]
    fn out_of_range_indices_are_bad() {
        let mut t = Owned::<u8, 4>::new();
        assert_eq!(t.get(4, 0), None);
        assert_eq!(t.get(usize::MAX, 0), None);
        assert_eq!(t.remove(4, 0), None);
        assert_eq!(t.owner_of(4), None);
        assert_eq!(t.at(4), None);
    }
}
