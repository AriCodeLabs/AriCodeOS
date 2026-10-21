// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Client DMA buffers mapped into a server (ADR 0038). A mapping keeps the
//! buffer allocated even after every handle to it is closed, so a server
//! must not keep mappings it no longer needs: [`ClientBuffers`] keeps at
//! most `N` and unmaps the oldest to make room. The kernel reuses the
//! addresses of an unmapped range, so a cache that misses often costs a
//! map and an unmap per request, not page tables.
//!
//! A driver that hands a client buffer to a device and cannot retire the
//! request (a timeout with the descriptors still in the ring, a transfer
//! whose slot was not released) keeps the buffer's handle in
//! [`DeviceOwned`] until the device can no longer reach it (incident 0017).

use crate::{self as sys, Handle};

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
        let entry = self.entry(dma)?;
        // SAFETY: client buffer mapped RW in this process until `insert`
        // evicts it, which needs `&mut self` (so not while this lives).
        Some(unsafe { core::slice::from_raw_parts_mut(entry.virt as *mut u8, entry.len as usize) })
    }

    /// Physical address and length of `dma`, mapped into this server (and
    /// so into its devices' IOMMU domain): for drivers that hand the buffer
    /// to a device and never touch it themselves.
    pub fn device_view(&mut self, dma: Handle) -> Option<(u64, u64)> {
        self.entry(dma).map(|e| (e.phys, e.len))
    }

    fn entry(&mut self, dma: Handle) -> Option<Entry> {
        let phys = sys::dma_phys(dma).ok()?;
        let len = sys::region_len(dma).ok()?;
        if let Some(e) = self.find(phys) {
            return Some(e);
        }
        let virt = sys::mmio_map(dma).ok()?;
        let e = Entry { phys, virt, len };
        self.insert(e, |old| {
            let _ = sys::mmio_unmap(old.virt);
        });
        Some(e)
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

/// What [`DeviceOwned::keep`] did with a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// Held until [`DeviceOwned::release`] or [`DeviceOwned::abandon`].
    Held,
    /// Every slot was taken: kept for good at once (its handle is never
    /// closed), as after [`DeviceOwned::abandon`].
    ForGood,
}

/// Client buffers a device may still own (incident 0017): requests the
/// driver could not retire from the device. Without an IOMMU the device
/// may write into such a buffer at any time, so its handle stays open:
/// a handle keeps the DMA buffer allocated whatever the client does and
/// whatever the mapping cache evicts (ADR 0038), and its IOMMU entry, if
/// any, lives as long as the buffer. The driver releases the buffers once
/// the device cannot reach them any more (the device or its queue was
/// reset, the slot was confirmed released) or keeps them for good when it
/// never can know. At most `N` are held; more are kept for good at once.
pub struct DeviceOwned<const N: usize> {
    held: [Option<Handle>; N],
    /// Buffers kept for good since the driver started (never closed).
    for_good: u32,
}

impl<const N: usize> Default for DeviceOwned<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> DeviceOwned<N> {
    pub const fn new() -> Self {
        Self {
            held: [None; N],
            for_good: 0,
        }
    }

    /// Takes over `dma`, the handle of a buffer the device may own: the
    /// caller must not close it any more.
    pub fn keep(&mut self, dma: Handle) -> Kept {
        match self.held.iter_mut().find(|h| h.is_none()) {
            Some(slot) => {
                *slot = Some(dma);
                Kept::Held
            }
            None => {
                self.keep_for_good(dma);
                Kept::ForGood
            }
        }
    }

    /// Takes over `dma` for good: the device may own it and nothing will
    /// ever say otherwise, so its handle is never closed.
    pub fn keep_for_good(&mut self, dma: Handle) {
        let _ = dma;
        self.for_good = self.for_good.saturating_add(1);
    }

    /// The device can no longer reach any held buffer: closes their
    /// handles. Returns how many were released.
    pub fn release(&mut self) -> usize {
        self.release_with(|h| {
            let _ = sys::handle_close(h);
        })
    }

    fn release_with(&mut self, mut close: impl FnMut(Handle)) -> usize {
        let mut n = 0;
        for h in self.held.iter_mut().filter_map(Option::take) {
            close(h);
            n += 1;
        }
        n
    }

    /// The device may keep the held buffers for good (nothing will tell
    /// the driver otherwise): they are forgotten without closing their
    /// handles, so their memory is never reused. Returns how many.
    pub fn abandon(&mut self) -> usize {
        let n = self.held().count();
        self.held = [None; N];
        self.for_good = self.for_good.saturating_add(n as u32);
        n
    }

    /// The handles held now.
    pub fn held(&self) -> impl Iterator<Item = Handle> + '_ {
        self.held.iter().flatten().copied()
    }

    /// Buffers kept for good since the driver started.
    pub fn kept_for_good(&self) -> u32 {
        self.for_good
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_owned_holds_until_released() {
        let mut o: DeviceOwned<2> = DeviceOwned::new();
        assert_eq!(o.keep(Handle(7)), Kept::Held);
        assert_eq!(o.keep(Handle(9)), Kept::Held);
        assert_eq!(o.held().count(), 2);
        let mut closed = [Handle(0); 2];
        let mut n = 0;
        assert_eq!(
            o.release_with(|h| {
                closed[n] = h;
                n += 1;
            }),
            2
        );
        assert_eq!(closed, [Handle(7), Handle(9)]);
        assert_eq!(o.held().count(), 0);
        assert_eq!(o.release_with(|_| panic!("nothing is held")), 0);
        assert_eq!(o.kept_for_good(), 0);
    }

    #[test]
    fn device_owned_abandons_without_closing_and_overflows_for_good() {
        let mut o: DeviceOwned<1> = DeviceOwned::new();
        assert_eq!(o.keep(Handle(3)), Kept::Held);
        // No room: the second is kept for good at once, never closed.
        assert_eq!(o.keep(Handle(4)), Kept::ForGood);
        assert_eq!(o.kept_for_good(), 1);
        assert_eq!(o.abandon(), 1);
        assert_eq!(o.kept_for_good(), 2);
        assert_eq!(o.held().count(), 0);
        // An abandoned handle is never handed to `close` later.
        assert_eq!(o.release_with(|_| panic!("abandoned handles stay open")), 0);
        assert_eq!(o.keep(Handle(5)), Kept::Held, "the slot is free again");
        o.keep_for_good(Handle(6));
        assert_eq!(o.kept_for_good(), 3);
        let mut held = o.held();
        assert_eq!(held.next(), Some(Handle(5)), "held ones stay held");
        assert_eq!(held.next(), None);
    }

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
