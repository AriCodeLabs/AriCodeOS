//! Allocator de frames físicos de 4 KiB por bitmap (Fase 5).
//!
//! Un bit por frame, 1 = ocupado. El almacenamiento del bitmap lo aporta el
//! llamador (en el kernel, una región libre del mapa UEFI), así que la crate
//! no necesita `alloc` y se prueba en host con un `Vec<u64>`.
//!
//! Política: todo empieza ocupado; el kernel libera explícitamente las
//! regiones `Free` del mapa y vuelve a reservar lo que no debe entregarse
//! (frame 0, memoria baja, el propio bitmap).

#![no_std]

pub const FRAME_SIZE: u64 = 4096;

/// Un frame físico, identificado por su dirección base (múltiplo de 4 KiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(u64);

impl Frame {
    /// # Panics
    /// Si `addr` no está alineado a frame.
    pub const fn from_addr(addr: u64) -> Self {
        assert!(
            addr.is_multiple_of(FRAME_SIZE),
            "direccion no alineada a frame"
        );
        Frame(addr)
    }
    pub const fn containing(addr: u64) -> Self {
        Frame(addr & !(FRAME_SIZE - 1))
    }
    pub const fn from_number(n: u64) -> Self {
        Frame(n * FRAME_SIZE)
    }
    pub const fn addr(self) -> u64 {
        self.0
    }
    pub const fn number(self) -> u64 {
        self.0 / FRAME_SIZE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmmError {
    /// El frame no pertenece al rango administrado.
    OutOfRange,
    /// El frame ya estaba libre.
    DoubleFree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub total_frames: usize,
    pub free_frames: usize,
}

impl Stats {
    pub const fn used_frames(&self) -> usize {
        self.total_frames - self.free_frames
    }
    pub const fn free_bytes(&self) -> u64 {
        self.free_frames as u64 * FRAME_SIZE
    }
    pub const fn total_bytes(&self) -> u64 {
        self.total_frames as u64 * FRAME_SIZE
    }
}

pub struct BitmapAllocator<'a> {
    /// 1 = ocupado. Los bits más allá de `frames` están siempre a 1.
    bits: &'a mut [u64],
    frames: usize,
    free: usize,
    /// Primer word que puede contener un bit libre (heurística, nunca mayor
    /// que el verdadero primero).
    hint: usize,
}

impl<'a> BitmapAllocator<'a> {
    /// Palabras de 64 bits necesarias para `frames` frames.
    pub const fn words_for(frames: usize) -> usize {
        frames.div_ceil(64)
    }

    /// Bytes necesarios para el bitmap de `frames` frames.
    pub const fn bytes_for(frames: usize) -> usize {
        Self::words_for(frames) * 8
    }

    /// Administra los frames `0..frames`, todos marcados como ocupados.
    ///
    /// # Panics
    /// Si `bits` es demasiado pequeño.
    pub fn new(bits: &'a mut [u64], frames: usize) -> Self {
        assert!(
            bits.len() >= Self::words_for(frames),
            "bitmap demasiado pequeno"
        );
        bits.fill(u64::MAX);
        Self {
            bits,
            frames,
            free: 0,
            hint: 0,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn stats(&self) -> Stats {
        Stats {
            total_frames: self.frames,
            free_frames: self.free,
        }
    }

    fn in_range(&self, f: Frame) -> bool {
        (f.number() as usize) < self.frames
    }

    fn get(&self, n: usize) -> bool {
        self.bits[n / 64] & (1 << (n % 64)) != 0
    }

    fn set(&mut self, n: usize, used: bool) {
        let (w, b) = (n / 64, 1u64 << (n % 64));
        let was = self.bits[w] & b != 0;
        if used {
            self.bits[w] |= b;
        } else {
            self.bits[w] &= !b;
            if w < self.hint {
                self.hint = w;
            }
        }
        match (was, used) {
            (true, false) => self.free += 1,
            (false, true) => self.free -= 1,
            _ => {}
        }
    }

    /// Marca como libres los frames contenidos por completo en `[start, end)`.
    /// Los frames parciales de los bordes quedan ocupados. Fuera de rango se ignora.
    pub fn free_range(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let first = start.div_ceil(FRAME_SIZE) as usize;
        let last = (end / FRAME_SIZE) as usize; // exclusivo
        for n in first..last.min(self.frames) {
            self.set(n, false);
        }
    }

    /// Marca como ocupados todos los frames que tocan `[start, end)`.
    pub fn reserve_range(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let first = (start / FRAME_SIZE) as usize;
        let last = end.div_ceil(FRAME_SIZE) as usize;
        for n in first..last.min(self.frames) {
            self.set(n, true);
        }
    }

    pub fn is_free(&self, f: Frame) -> bool {
        self.in_range(f) && !self.get(f.number() as usize)
    }

    /// Entrega el frame libre más bajo.
    pub fn allocate(&mut self) -> Option<Frame> {
        let words = self.bits.len();
        for w in self.hint..words {
            let word = self.bits[w];
            if word != u64::MAX {
                let bit = (!word).trailing_zeros() as usize;
                let n = w * 64 + bit;
                if n >= self.frames {
                    return None;
                }
                self.hint = w;
                self.set(n, true);
                return Some(Frame::from_number(n as u64));
            }
        }
        None
    }

    /// `count` frames consecutivos cuyo primero está alineado a `align_frames`
    /// (potencia de dos, >= 1). Búsqueda lineal: para reservas grandes y raras
    /// (DMA, tablas iniciales), no para la ruta rápida.
    pub fn allocate_contiguous(&mut self, count: usize, align_frames: usize) -> Option<Frame> {
        assert!(count > 0 && align_frames.is_power_of_two());
        let mut start = self.hint * 64;
        start = start.next_multiple_of(align_frames);
        while start + count <= self.frames {
            match (start..start + count).find(|&n| self.get(n)) {
                None => {
                    for n in start..start + count {
                        self.set(n, true);
                    }
                    return Some(Frame::from_number(start as u64));
                }
                Some(busy) => start = (busy + 1).next_multiple_of(align_frames),
            }
        }
        None
    }

    pub fn free(&mut self, f: Frame) -> Result<(), PmmError> {
        if !self.in_range(f) {
            return Err(PmmError::OutOfRange);
        }
        let n = f.number() as usize;
        if !self.get(n) {
            return Err(PmmError::DoubleFree);
        }
        self.set(n, false);
        Ok(())
    }

    /// Libera `count` frames consecutivos desde `first`.
    pub fn free_contiguous(&mut self, first: Frame, count: usize) -> Result<(), PmmError> {
        for i in 0..count as u64 {
            self.free(Frame::from_number(first.number() + i))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn pmm(frames: usize) -> (Vec<u64>, usize) {
        (vec![0u64; BitmapAllocator::words_for(frames)], frames)
    }

    #[test]
    fn starts_fully_reserved_then_frees_whole_frames_only() {
        let (mut bits, n) = pmm(100);
        let mut p = BitmapAllocator::new(&mut bits, n);
        assert_eq!(p.stats().free_frames, 0);
        assert!(p.allocate().is_none());
        // [0x1800, 0x8800): frames completos 2..=7 (6 frames); bordes parciales fuera.
        p.free_range(0x1800, 0x8800);
        assert_eq!(p.stats().free_frames, 6);
        assert!(!p.is_free(Frame::from_number(1)));
        assert!(p.is_free(Frame::from_number(2)));
        assert!(p.is_free(Frame::from_number(7)));
        assert!(!p.is_free(Frame::from_number(8)));
    }

    #[test]
    fn allocate_returns_lowest_and_unique_frames() {
        let (mut bits, n) = pmm(64 * 3);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, n as u64 * FRAME_SIZE);
        let a = p.allocate().unwrap();
        let b = p.allocate().unwrap();
        assert_eq!(a, Frame::from_number(0));
        assert_eq!(b, Frame::from_number(1));
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(a);
        seen.insert(b);
        while let Some(f) = p.allocate() {
            assert!(seen.insert(f), "frame repetido {f:?}");
        }
        assert_eq!(seen.len(), n);
        assert_eq!(p.stats().free_frames, 0);
    }

    #[test]
    fn free_and_double_free() {
        let (mut bits, n) = pmm(10);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 10 * FRAME_SIZE);
        let f = p.allocate().unwrap();
        assert!(p.free(f).is_ok());
        assert_eq!(p.free(f), Err(PmmError::DoubleFree));
        assert_eq!(p.free(Frame::from_number(99)), Err(PmmError::OutOfRange));
        // Tras liberar, el mismo frame vuelve a ser el primero.
        assert_eq!(p.allocate(), Some(f));
    }

    #[test]
    fn reserve_range_rounds_outward_and_hint_moves_back_on_free() {
        let (mut bits, n) = pmm(200);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 200 * FRAME_SIZE);
        p.reserve_range(0x1001, 0x2FFF); // toca frames 1 y 2
        assert!(p.is_free(Frame::from_number(0)));
        assert!(!p.is_free(Frame::from_number(1)));
        assert!(!p.is_free(Frame::from_number(2)));
        assert!(p.is_free(Frame::from_number(3)));
        // Agotar los primeros 130 frames mueve el hint; liberar uno bajo lo devuelve.
        let first = p.allocate().unwrap();
        for _ in 0..129 {
            p.allocate().unwrap();
        }
        p.free(first).unwrap();
        assert_eq!(p.allocate(), Some(first));
    }

    #[test]
    fn contiguous_allocation_respects_alignment_and_gaps() {
        let (mut bits, n) = pmm(64);
        let mut p = BitmapAllocator::new(&mut bits, n);
        p.free_range(0, 64 * FRAME_SIZE);
        p.reserve_range(3 * FRAME_SIZE, 4 * FRAME_SIZE); // hueco en el frame 3
        let f = p.allocate_contiguous(4, 4).unwrap();
        assert_eq!(f, Frame::from_number(4), "0..4 tiene el frame 3 ocupado");
        let g = p.allocate_contiguous(2, 1).unwrap();
        assert_eq!(g, Frame::from_number(0));
        assert_eq!(p.stats().used_frames(), 7);
        assert!(p.allocate_contiguous(100, 1).is_none());
        p.free_contiguous(f, 4).unwrap();
        assert!(p.is_free(Frame::from_number(7)));
    }

    #[test]
    fn sizes_and_frame_helpers() {
        assert_eq!(BitmapAllocator::words_for(1), 1);
        assert_eq!(BitmapAllocator::words_for(64), 1);
        assert_eq!(BitmapAllocator::words_for(65), 2);
        assert_eq!(BitmapAllocator::bytes_for(1 << 20), 128 * 1024); // 4 GiB -> 128 KiB
        assert_eq!(Frame::containing(0x1FFF), Frame::from_number(1));
        assert_eq!(Frame::from_number(5).addr(), 5 * FRAME_SIZE);
    }

    #[test]
    #[should_panic(expected = "demasiado pequeno")]
    fn rejects_small_bitmap() {
        let mut bits = [0u64; 1];
        BitmapAllocator::new(&mut bits, 65);
    }
}
