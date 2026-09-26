//! Entrada de consola (Fase 21): recepción por COM1 con interrupción. El
//! handler guarda los bytes en un anillo y despierta a quien espera en
//! `console_read`. La consola es del kernel: los procesos solo la ven a
//! través de un handle `CONSOLE` con `READ`.

use crate::arch::x86_64::ioapic;
use crate::arch::x86_64::port::{inb, outb};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use log::{info, warn};

const COM1: u16 = 0x3F8;
/// IRQ ISA de COM1.
const COM1_ISA_IRQ: u8 = 4;
/// Vector propio, fuera del rango de dispositivos de usuario (`hw`).
pub const SERIAL_VECTOR: u8 = 0x40;
const CAPACITY: usize = 1024;

struct Ring {
    buf: [u8; CAPACITY],
    head: usize,
    len: usize,
    dropped: u64,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; CAPACITY],
            head: 0,
            len: 0,
            dropped: 0,
        }
    }
    fn push(&mut self, b: u8) {
        if self.len == CAPACITY {
            self.dropped += 1;
            return;
        }
        self.buf[(self.head + self.len) % CAPACITY] = b;
        self.len += 1;
    }
    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        Some(b)
    }
}

static RING: IrqLock<Ring> = IrqLock::new(Ring::new());
static WAITERS: WaitQueue = WaitQueue::new();

/// Enruta la IRQ de COM1 al vector propio y habilita la interrupción de
/// recepción del UART. Requiere I/O APIC e interrupciones activas.
pub fn init() {
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(COM1_ISA_IRQ));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    let lapic_id = crate::acpi::with(|a| a.madt.lapic_ids.first().copied()).unwrap_or(0);
    if let Err(e) = ioapic::route(gsi, SERIAL_VECTOR, lapic_id, level, low) {
        warn!("console: sin entrada por serie ({e})");
        return;
    }
    // SAFETY: IER de COM1; solo habilita la interrupción "dato recibido".
    unsafe { outb(COM1 + 1, 0x01) };
    info!("console: entrada por COM1 (GSI {gsi}, vector {SERIAL_VECTOR:#x})");
}

/// Handler del vector serie: vacía el FIFO de recepción en el anillo.
pub fn on_irq() {
    let mut ring = RING.lock();
    // SAFETY: LSR bit 0 = dato disponible; RBR devuelve el byte recibido.
    unsafe {
        while inb(COM1 + 5) & 0x01 != 0 {
            ring.push(inb(COM1));
        }
    }
    drop(ring);
    WAITERS.wake_all();
}

/// Copia hasta `buf.len()` bytes disponibles; espera al primero hasta
/// `deadline` (ticks). Devuelve 0 solo si venció el plazo.
pub fn read(buf: &mut [u8], deadline: Option<u64>) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let timed_out = WAITERS.wait_until_deadline(|| RING.lock().len > 0, deadline);
    if timed_out {
        return 0;
    }
    let mut ring = RING.lock();
    let mut n = 0;
    while n < buf.len() {
        match ring.pop() {
            Some(b) => {
                buf[n] = b;
                n += 1;
            }
            None => break,
        }
    }
    n
}

/// Bytes descartados por anillo lleno (diagnóstico).
pub fn dropped() -> u64 {
    RING.lock().dropped
}

/// Autoprueba (`test=console_input`): un plazo vence sin entrada, luego se
/// lee una línea que el runner envía al ver `CONSOLE_INPUT_READY`.
pub fn selftest() {
    let mut buf = [0u8; 64];
    let deadline = Some(crate::time::ticks() + 10);
    assert_eq!(
        read(&mut buf, deadline),
        0,
        "sin entrada debe vencer el plazo"
    );
    crate::println!("CONSOLE_INPUT_READY");
    let mut line = alloc::vec::Vec::new();
    loop {
        let n = read(&mut buf, None);
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                let text = core::str::from_utf8(&line).unwrap_or("?");
                crate::println!(
                    "console: linea '{text}' ({} bytes, {} descartados)",
                    line.len(),
                    dropped()
                );
                return;
            }
            line.push(b);
        }
    }
}
