// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Open-descriptor table of the file service (ADR 0036). A descriptor is
//! `(file index, owner)`: the owner is the client endpoint the request
//! arrived on, so a descriptor only answers the endpoint that opened it
//! (an application cannot read, write or close another one's files by
//! guessing numbers) and every descriptor of an owner can be released at
//! once when the kernel reports that its client is gone (incident 0011).

/// Open descriptors at once, for every client together.
pub const MAX_OPEN: usize = 16;

/// Descriptor table: slot = descriptor number.
pub struct Descriptors {
    slots: [Option<(usize, usize)>; MAX_OPEN],
}

impl Default for Descriptors {
    fn default() -> Self {
        Self::new()
    }
}

impl Descriptors {
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_OPEN],
        }
    }

    /// Opens `index` for `owner`; `None` when the table is full.
    pub fn open(&mut self, index: usize, owner: usize) -> Option<u32> {
        let slot = self.slots.iter().position(Option::is_none)?;
        self.slots[slot] = Some((index, owner));
        Some(slot as u32)
    }

    /// The file behind `fd`, only for the owner that opened it.
    pub fn get(&self, fd: u32, owner: usize) -> Option<usize> {
        match self.slots.get(fd as usize).copied().flatten() {
            Some((index, o)) if o == owner => Some(index),
            _ => None,
        }
    }

    /// Closes `fd` if `owner` opened it; `false` otherwise (`BadFd`).
    pub fn close(&mut self, fd: u32, owner: usize) -> bool {
        match self.slots.get_mut(fd as usize) {
            Some(s) if s.is_some_and(|(_, o)| o == owner) => {
                *s = None;
                true
            }
            _ => false,
        }
    }

    /// Releases every descriptor of `owner` (its client is gone) and
    /// returns how many there were.
    pub fn reclaim(&mut self, owner: usize) -> usize {
        let mut n = 0;
        for s in self.slots.iter_mut() {
            if s.is_some_and(|(_, o)| o == owner) {
                *s = None;
                n += 1;
            }
        }
        n
    }

    /// Descriptors in use, every owner together.
    pub fn used(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    pub fn is_full(&self) -> bool {
        self.used() == MAX_OPEN
    }

    /// Descriptors `owner` holds.
    pub fn owned_by(&self, owner: usize) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|(_, o)| *o == owner)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_descriptor_answers_only_its_owner() {
        let mut t = Descriptors::new();
        let fd = t.open(3, 1).unwrap();
        assert_eq!(t.get(fd, 1), Some(3));
        assert_eq!(t.get(fd, 2), None, "another client must not see it");
        assert!(!t.close(fd, 2), "nor close it");
        assert_eq!(t.get(fd, 1), Some(3));
        assert!(t.close(fd, 1));
        assert_eq!(t.get(fd, 1), None);
        assert!(!t.close(fd, 1), "closing twice is BadFd");
    }

    #[test]
    fn a_full_table_refuses_and_frees_on_close() {
        let mut t = Descriptors::new();
        for i in 0..MAX_OPEN {
            assert!(t.open(i, 0).is_some());
        }
        assert!(t.is_full());
        assert_eq!(t.open(0, 0), None);
        assert_eq!(t.open(0, 7), None);
        assert!(t.close(5, 0));
        assert_eq!(t.open(9, 7), Some(5), "the freed slot is reused");
    }

    /// Incident 0011: the descriptors of a dead client go at once, those of
    /// the others stay where they were.
    #[test]
    fn reclaim_frees_one_owner_only() {
        let mut t = Descriptors::new();
        let shell = t.open(0, 1).unwrap();
        for i in 0..5 {
            t.open(i, 2).unwrap();
        }
        let other = t.open(4, 3).unwrap();
        assert_eq!(t.used(), 7);
        assert_eq!(t.owned_by(2), 5);
        assert_eq!(t.reclaim(2), 5);
        assert_eq!(t.reclaim(2), 0, "nothing left to reclaim");
        assert_eq!(t.owned_by(2), 0);
        assert_eq!(t.used(), 2);
        assert_eq!(t.get(shell, 1), Some(0));
        assert_eq!(t.get(other, 3), Some(4));
        // A new client given the dead one's owner number starts clean.
        assert_eq!(t.get(1, 2), None);
    }

    #[test]
    fn reclaim_makes_room_in_a_full_table() {
        let mut t = Descriptors::new();
        for i in 0..MAX_OPEN {
            t.open(i, 5).unwrap();
        }
        assert_eq!(t.open(0, 1), None);
        assert_eq!(t.reclaim(5), MAX_OPEN);
        assert_eq!(t.open(0, 1), Some(0));
        assert_eq!(t.used(), 1);
    }

    #[test]
    fn out_of_range_descriptors_are_bad() {
        let mut t = Descriptors::new();
        assert_eq!(t.get(MAX_OPEN as u32, 0), None);
        assert_eq!(t.get(u32::MAX, 0), None);
        assert!(!t.close(MAX_OPEN as u32, 0));
    }
}
