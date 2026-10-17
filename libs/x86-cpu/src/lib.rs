// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! x86-64 architecture structures that can be built and tested on the
//! host: segment descriptors, TSS, IDT entries and the exception
//! table. The privileged instructions (`lgdt`, `lidt`, `ltr`,
//! `iretq`) live in `kernel/src/arch/x86_64`.

#![no_std]

/// Pointer for `lgdt`/`lidt`.
#[repr(C, packed(2))]
#[derive(Debug, Clone, Copy)]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

/// Segment selector (index << 3 | TI << 2 | RPL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selector(pub u16);

impl Selector {
    pub const NULL: Selector = Selector(0);
    pub const fn new(index: u16, rpl: u16) -> Self {
        Selector((index << 3) | (rpl & 3))
    }
    pub const fn index(self) -> u16 {
        self.0 >> 3
    }
}

/// 8-byte segment descriptor (long mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDescriptor(pub u64);

impl SegmentDescriptor {
    const LIMIT_LOW: u64 = 0xFFFF;
    const LIMIT_HIGH: u64 = 0xF << 48;
    const PRESENT: u64 = 1 << 47;
    const USER_SEGMENT: u64 = 1 << 44; // S = code/data
    const EXECUTABLE: u64 = 1 << 43;
    const READ_WRITE: u64 = 1 << 41; // read (code) / write (data)
    const LONG_MODE: u64 = 1 << 53;
    const DEFAULT_32: u64 = 1 << 54;
    const GRANULARITY_4K: u64 = 1 << 55;
    const DPL_SHIFT: u32 = 45;

    pub const NULL: SegmentDescriptor = SegmentDescriptor(0);

    /// 64-bit code, executable and readable, base 0, full limit.
    pub const fn code64(dpl: u8) -> Self {
        SegmentDescriptor(
            Self::LIMIT_LOW
                | Self::LIMIT_HIGH
                | Self::PRESENT
                | Self::USER_SEGMENT
                | Self::EXECUTABLE
                | Self::READ_WRITE
                | Self::LONG_MODE
                | Self::GRANULARITY_4K
                | ((dpl as u64 & 3) << Self::DPL_SHIFT),
        )
    }

    /// Readable and writable data, base 0, full limit.
    pub const fn data(dpl: u8) -> Self {
        SegmentDescriptor(
            Self::LIMIT_LOW
                | Self::LIMIT_HIGH
                | Self::PRESENT
                | Self::USER_SEGMENT
                | Self::READ_WRITE
                | Self::DEFAULT_32
                | Self::GRANULARITY_4K
                | ((dpl as u64 & 3) << Self::DPL_SHIFT),
        )
    }

    pub const fn dpl(self) -> u8 {
        ((self.0 >> Self::DPL_SHIFT) & 3) as u8
    }
}

/// 64-bit Task State Segment (§8.7 of the Intel manual, vol. 3A).
#[repr(C, packed(4))]
#[derive(Debug, Clone, Copy)]
pub struct Tss {
    _reserved0: u32,
    /// Stacks for privilege changes to ring 0..2.
    pub rsp: [u64; 3],
    _reserved1: u64,
    /// Interrupt Stack Table: `ist[0]` is IST1.
    pub ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    pub iomap_base: u16,
}

impl Tss {
    pub const SIZE: usize = 104;

    pub const fn new() -> Self {
        Tss {
            _reserved0: 0,
            rsp: [0; 3],
            _reserved1: 0,
            ist: [0; 7],
            _reserved2: 0,
            _reserved3: 0,
            // No I/O map: base beyond the limit.
            iomap_base: Self::SIZE as u16,
        }
    }
}

impl Default for Tss {
    fn default() -> Self {
        Self::new()
    }
}

/// 16-byte system descriptor for an available TSS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TssDescriptor {
    pub low: u64,
    pub high: u64,
}

impl TssDescriptor {
    /// `base` is the virtual address of the `Tss`; the limit is `Tss::SIZE - 1`.
    pub const fn new(base: u64) -> Self {
        const TYPE_TSS_AVAILABLE: u64 = 0x9;
        let limit = (Tss::SIZE - 1) as u64;
        let low = (limit & 0xFFFF)
            | ((base & 0xFF_FFFF) << 16)
            | (TYPE_TSS_AVAILABLE << 40)
            | (1 << 47) // present
            | (((limit >> 16) & 0xF) << 48)
            | (((base >> 24) & 0xFF) << 56);
        let high = base >> 32;
        TssDescriptor { low, high }
    }
}

/// GDT layout required by `syscall`/`sysret`: `STAR[47:32]` = kernel
/// code (and data = +8); `STAR[63:48]` = base such that user data = +8 and
/// user code = +16.
///
/// ```text
/// 0x00 null
/// 0x08 kernel code (DPL 0)
/// 0x10 kernel data (DPL 0)
/// 0x18 user data   (DPL 3)   <- STAR[63:48] = 0x10 => SS = 0x18 | 3
/// 0x20 user code   (DPL 3)   <-                        CS = 0x20 | 3
/// 0x28 TSS (16 bytes)
/// ```
pub mod layout {
    use super::Selector;
    pub const KERNEL_CODE: Selector = Selector(0x08);
    pub const KERNEL_DATA: Selector = Selector(0x10);
    pub const USER_DATA: Selector = Selector(0x18 | 3);
    pub const USER_CODE: Selector = Selector(0x20 | 3);
    pub const TSS: Selector = Selector(0x28);
    /// Value of `IA32_STAR` for this layout.
    pub const STAR: u64 = ((0x10u64) << 48) | ((0x08u64) << 32);
}

/// Global descriptor table with fixed capacity.
#[repr(C, align(8))]
#[derive(Debug, Clone, Copy)]
pub struct Gdt<const N: usize> {
    entries: [u64; N],
    len: usize,
}

impl<const N: usize> Gdt<N> {
    /// GDT with the null entry already in place.
    pub const fn new() -> Self {
        Gdt {
            entries: [0; N],
            len: 1,
        }
    }

    pub fn push(&mut self, d: SegmentDescriptor) -> Selector {
        assert!(self.len < N, "GDT full");
        let idx = self.len;
        self.entries[idx] = d.0;
        self.len += 1;
        Selector::new(idx as u16, d.dpl() as u16)
    }

    pub fn push_tss(&mut self, d: TssDescriptor) -> Selector {
        assert!(self.len + 1 < N, "GDT full");
        let idx = self.len;
        self.entries[idx] = d.low;
        self.entries[idx + 1] = d.high;
        self.len += 2;
        Selector::new(idx as u16, 0)
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn entry(&self, i: usize) -> u64 {
        self.entries[i]
    }

    pub fn pointer(&self) -> DescriptorTablePointer {
        DescriptorTablePointer {
            limit: (self.len * 8 - 1) as u16,
            base: self.entries.as_ptr() as u64,
        }
    }
}

impl<const N: usize> Default for Gdt<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// 16-byte IDT entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdtEntry {
    offset_low: u16,
    selector: u16,
    /// bits 0..2: IST index (0 = none).
    ist: u8,
    /// P | DPL | 0 | type (0xE = 64-bit interrupt gate).
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl IdtEntry {
    const TYPE_INTERRUPT_GATE: u8 = 0xE;

    pub const fn missing() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    /// Interrupt gate (IF is cleared on entry). `ist` in 0..=7.
    pub const fn interrupt_gate(handler: u64, cs: Selector, ist: u8, dpl: u8) -> Self {
        IdtEntry {
            offset_low: handler as u16,
            selector: cs.0,
            ist: ist & 7,
            type_attr: (1 << 7) | ((dpl & 3) << 5) | Self::TYPE_INTERRUPT_GATE,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved: 0,
        }
    }

    pub const fn handler(&self) -> u64 {
        self.offset_low as u64 | (self.offset_mid as u64) << 16 | (self.offset_high as u64) << 32
    }
    pub const fn ist(&self) -> u8 {
        self.ist & 7
    }
    pub const fn present(&self) -> bool {
        self.type_attr & 0x80 != 0
    }
}

/// Full interrupt descriptor table.
#[repr(C, align(16))]
pub struct Idt {
    entries: [IdtEntry; 256],
}

impl Idt {
    pub const fn new() -> Self {
        Idt {
            entries: [IdtEntry::missing(); 256],
        }
    }
    pub fn set(&mut self, vector: u8, entry: IdtEntry) {
        self.entries[vector as usize] = entry;
    }
    pub fn get(&self, vector: u8) -> IdtEntry {
        self.entries[vector as usize]
    }
    pub fn pointer(&self) -> DescriptorTablePointer {
        DescriptorTablePointer {
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
            base: self.entries.as_ptr() as u64,
        }
    }
}

impl Default for Idt {
    fn default() -> Self {
        Self::new()
    }
}

/// Architecture exceptions (vectors 0..=31).
pub mod exception {
    pub const DIVIDE_ERROR: u8 = 0;
    pub const DEBUG: u8 = 1;
    pub const NMI: u8 = 2;
    pub const BREAKPOINT: u8 = 3;
    pub const OVERFLOW: u8 = 4;
    pub const BOUND_RANGE: u8 = 5;
    pub const INVALID_OPCODE: u8 = 6;
    pub const DEVICE_NOT_AVAILABLE: u8 = 7;
    pub const DOUBLE_FAULT: u8 = 8;
    pub const INVALID_TSS: u8 = 10;
    pub const SEGMENT_NOT_PRESENT: u8 = 11;
    pub const STACK_SEGMENT: u8 = 12;
    pub const GENERAL_PROTECTION: u8 = 13;
    pub const PAGE_FAULT: u8 = 14;
    pub const X87_FP: u8 = 16;
    pub const ALIGNMENT_CHECK: u8 = 17;
    pub const MACHINE_CHECK: u8 = 18;
    pub const SIMD_FP: u8 = 19;
    pub const VIRTUALIZATION: u8 = 20;
    pub const CONTROL_PROTECTION: u8 = 21;

    /// Vectors for which the CPU pushes an error code.
    pub const fn has_error_code(vector: u8) -> bool {
        matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
    }

    /// Mnemonic and name. Vectors >= 32 are external interrupts.
    pub const fn name(vector: u8) -> (&'static str, &'static str) {
        match vector {
            0 => ("#DE", "Divide Error"),
            1 => ("#DB", "Debug"),
            2 => ("NMI", "Non-Maskable Interrupt"),
            3 => ("#BP", "Breakpoint"),
            4 => ("#OF", "Overflow"),
            5 => ("#BR", "BOUND Range Exceeded"),
            6 => ("#UD", "Invalid Opcode"),
            7 => ("#NM", "Device Not Available"),
            8 => ("#DF", "Double Fault"),
            9 => ("---", "Coprocessor Segment Overrun"),
            10 => ("#TS", "Invalid TSS"),
            11 => ("#NP", "Segment Not Present"),
            12 => ("#SS", "Stack-Segment Fault"),
            13 => ("#GP", "General Protection"),
            14 => ("#PF", "Page Fault"),
            16 => ("#MF", "x87 Floating-Point"),
            17 => ("#AC", "Alignment Check"),
            18 => ("#MC", "Machine Check"),
            19 => ("#XM", "SIMD Floating-Point"),
            20 => ("#VE", "Virtualization"),
            21 => ("#CP", "Control Protection"),
            28 => ("#HV", "Hypervisor Injection"),
            29 => ("#VC", "VMM Communication"),
            30 => ("#SX", "Security"),
            15 | 22..=27 | 31 => ("#??", "Reserved"),
            _ => ("IRQ", "External Interrupt"),
        }
    }

    /// #PF error code bits.
    pub const PF_PRESENT: u64 = 1 << 0;
    pub const PF_WRITE: u64 = 1 << 1;
    pub const PF_USER: u64 = 1 << 2;
    pub const PF_RESERVED: u64 = 1 << 3;
    pub const PF_INSTRUCTION: u64 = 1 << 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_descriptors_match_known_encodings() {
        // Classic values of a 64-bit kernel (base 0, limit 4G, G=1).
        assert_eq!(SegmentDescriptor::code64(0).0, 0x00AF_9A00_0000_FFFF);
        assert_eq!(SegmentDescriptor::data(0).0, 0x00CF_9200_0000_FFFF);
        assert_eq!(SegmentDescriptor::code64(3).0, 0x00AF_FA00_0000_FFFF);
        assert_eq!(SegmentDescriptor::data(3).0, 0x00CF_F200_0000_FFFF);
        assert_eq!(SegmentDescriptor::code64(3).dpl(), 3);
    }

    #[test]
    fn tss_layout_and_descriptor() {
        assert_eq!(core::mem::size_of::<Tss>(), Tss::SIZE);
        let base = 0xFFFF_FFFF_8012_3456u64;
        let d = TssDescriptor::new(base);
        assert_eq!(d.low & 0xFFFF, (Tss::SIZE - 1) as u64);
        assert_eq!((d.low >> 16) & 0xFF_FFFF, base & 0xFF_FFFF);
        assert_eq!((d.low >> 40) & 0xF, 0x9, "available TSS type");
        assert!(d.low & (1 << 47) != 0, "present");
        assert_eq!((d.low >> 56) & 0xFF, (base >> 24) & 0xFF);
        assert_eq!(d.high, base >> 32);
        let t = Tss::new();
        assert_eq!(t.iomap_base, Tss::SIZE as u16);
    }

    #[test]
    fn gdt_selectors_and_pointer() {
        let mut g: Gdt<8> = Gdt::new();
        let cs = g.push(SegmentDescriptor::code64(0));
        let ds = g.push(SegmentDescriptor::data(0));
        let uds = g.push(SegmentDescriptor::data(3));
        let ucs = g.push(SegmentDescriptor::code64(3));
        let tss = g.push_tss(TssDescriptor::new(0x1000));
        assert_eq!(cs, layout::KERNEL_CODE);
        assert_eq!(ds, layout::KERNEL_DATA);
        assert_eq!(uds, layout::USER_DATA, "RPL 3 included");
        assert_eq!(ucs, layout::USER_CODE);
        assert_eq!(tss, layout::TSS);
        assert_eq!(g.len(), 7);
        // sysret: CS = STAR[63:48] + 16, SS = STAR[63:48] + 8; syscall: CS = STAR[47:32].
        let base = (layout::STAR >> 48) as u16;
        assert_eq!(base + 16 | 3, layout::USER_CODE.0);
        assert_eq!(base + 8 | 3, layout::USER_DATA.0);
        assert_eq!(
            ((layout::STAR >> 32) & 0xFFFF) as u16,
            layout::KERNEL_CODE.0
        );
        assert_eq!(g.entry(0), 0, "null entry");
        let p = g.pointer();
        assert_eq!({ p.limit }, 7 * 8 - 1);
        assert_eq!({ p.base }, g.entries.as_ptr() as u64);
        assert_eq!(Selector::new(3, 3).0, 0x1B);
        assert_eq!(Selector(0x1B).index(), 3);
    }

    #[test]
    #[should_panic(expected = "GDT full")]
    fn gdt_overflow_panics() {
        let mut g: Gdt<2> = Gdt::new();
        g.push(SegmentDescriptor::data(0));
        g.push(SegmentDescriptor::data(0));
    }

    #[test]
    fn idt_entry_splits_handler_address() {
        let h = 0xFFFF_FFFF_8000_1234u64;
        let e = IdtEntry::interrupt_gate(h, Selector(0x08), 1, 0);
        assert_eq!(e.handler(), h);
        assert_eq!(e.ist(), 1);
        assert!(e.present());
        assert_eq!(e.type_attr, 0x8E);
        assert_eq!(e.selector, 0x08);
        assert_eq!(core::mem::size_of::<IdtEntry>(), 16);
        assert!(!IdtEntry::missing().present());
        let user = IdtEntry::interrupt_gate(h, Selector(0x08), 0, 3);
        assert_eq!(user.type_attr, 0xEE);
    }

    #[test]
    fn idt_table_pointer() {
        let mut idt = Idt::new();
        idt.set(14, IdtEntry::interrupt_gate(0x1000, Selector(0x08), 0, 0));
        assert_eq!(idt.get(14).handler(), 0x1000);
        assert!(!idt.get(13).present());
        let p = idt.pointer();
        assert_eq!({ p.limit }, 256 * 16 - 1);
        assert_eq!(core::mem::size_of::<Idt>(), 4096);
    }

    #[test]
    fn exception_table() {
        use exception::*;
        assert!(
            has_error_code(DOUBLE_FAULT)
                && has_error_code(PAGE_FAULT)
                && has_error_code(GENERAL_PROTECTION)
        );
        assert!(
            !has_error_code(BREAKPOINT)
                && !has_error_code(INVALID_OPCODE)
                && !has_error_code(DIVIDE_ERROR)
        );
        assert_eq!(name(PAGE_FAULT).0, "#PF");
        assert_eq!(name(3).1, "Breakpoint");
        assert_eq!(name(15).0, "#??");
        assert_eq!(name(33).0, "IRQ");
    }
}
