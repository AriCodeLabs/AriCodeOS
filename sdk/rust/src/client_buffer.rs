// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Client DMA buffers mapped into a server (ADR 0038). A mapping keeps the
//! buffer allocated even after every handle to it is closed, so a server
//! must not keep mappings it no longer needs: either one request at a time
//! ([`MappedBuffer`], unmapped on drop) or a bounded cache
//! ([`ClientBuffers`], the oldest mapping is unmapped to make room).

use crate::{self as sys, Handle};

/// A client buffer mapped for one request; unmapped when dropped. Do not
/// map the same buffer through a [`ClientBuffers`] in the same process:
/// the kernel returns the same mapping and this would remove it.
pub struct MappedBuffer {
    pub phys: u64,
    pub len: u64,
    virt: u64,
}

impl MappedBuffer {
    /// Maps `dma` into this server (and its devices' IOMMU domain).
    pub fn map(dma: Handle) -> Option<Self> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        let virt = sys::mmio_map(dma).ok()?;
        Some(Self { phys, len, virt })
    }
}

impl Drop for MappedBuffer {
    fn drop(&mut self) {
        let _ = sys::mmio_unmap(self.virt);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    phys: u64,
    virt: u64,
    len: u64,
}

/// Client buffer mappings reused across requests, by physical address (a
/// mapped buffer cannot be freed, so the same address is the same buffer).
/// At most `N`: mapping one more unmaps the oldest, so at most `N` buffers
/// of clients that are gone stay allocated because of this server.
pub struct ClientBuffers<const N: usize> {
    entries: [Option<Entry>; N],
    /// Slot the next mapping replaces when every slot is taken.
    oldest: usize,
}

impl<const N: usize> Default for ClientBuffers<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ClientBuffers<N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            oldest: 0,
        }
    }

    /// The bytes of `dma`, mapped now or by an earlier request.
    pub fn map(&mut self, dma: Handle) -> Option<&mut [u8]> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        let entry = match self.find(phys) {
            Some(e) => e,
            None => {
                let virt = sys::mmio_map(dma).ok()?;
                let e = Entry { phys, virt, len };
                self.insert(e, |old| {
                    let _ = sys::mmio_unmap(old.virt);
                });
                e
            }
        };
        // SAFETY: client buffer mapped RW in this process until `insert`
        // evicts it, which needs `&mut self` (so not while this lives).
        Some(unsafe { core::slice::from_raw_parts_mut(entry.virt as *mut u8, entry.len as usize) })
    }

    fn find(&self, phys: u64) -> Option<Entry> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.phys == phys)
            .copied()
    }

    /// Stores `e` in a free slot or in place of the oldest entry, which is
    /// handed to `evict` first.
    fn insert(&mut self, e: Entry, evict: impl FnOnce(Entry)) {
        if N == 0 {
            return evict(e);
        }
        let slot = match self.entries.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                let victim = self.oldest;
                self.oldest = (victim + 1) % N;
                if let Some(old) = self.entries[victim].take() {
                    evict(old);
                }
                victim
            }
        };
        self.entries[slot] = Some(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u64) -> Entry {
        Entry {
            phys: n * 0x1000,
            virt: 0x1000_0000 + n * 0x1000,
            len: 512,
        }
    }

    /// Evicted entries, in order (no `alloc` in this crate).
    #[derive(Default)]
    struct Evicted {
        list: [Option<Entry>; 4],
        count: usize,
    }

    impl Evicted {
        fn push(&mut self, e: Entry) {
            self.list[self.count] = Some(e);
            self.count += 1;
        }
        fn get(&self) -> &[Option<Entry>] {
            &self.list[..self.count]
        }
    }

    #[test]
    fn cache_is_bounded_and_evicts_the_oldest() {
        let mut c: ClientBuffers<2> = ClientBuffers::new();
        let mut ev = Evicted::default();
        c.insert(entry(1), |o| ev.push(o));
        c.insert(entry(2), |o| ev.push(o));
        assert!(ev.get().is_empty());
        assert_eq!(c.find(0x1000), Some(entry(1)));
        c.insert(entry(3), |o| ev.push(o));
        c.insert(entry(4), |o| ev.push(o));
        assert_eq!(ev.get(), [Some(entry(1)), Some(entry(2))]);
        assert_eq!(c.find(0x1000), None);
        assert_eq!(c.find(0x3000), Some(entry(3)));
        assert_eq!(c.find(0x4000), Some(entry(4)));
        c.insert(entry(5), |o| ev.push(o));
        assert_eq!(ev.get().last(), Some(&Some(entry(3))));
    }

    #[test]
    fn an_empty_cache_unmaps_at_once() {
        let mut c: ClientBuffers<0> = ClientBuffers::new();
        let mut ev = Evicted::default();
        c.insert(entry(1), |o| ev.push(o));
        assert_eq!(ev.get(), [Some(entry(1))]);
    }
}
