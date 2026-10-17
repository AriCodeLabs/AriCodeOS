// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Console input (Phases 21 and 34): reception on COM1 and on the i8042 PS/2
//! keyboard, both interrupt-driven. The handlers put the bytes in a shared
//! ring and wake whoever waits in `console_read`. The console belongs to the
//! kernel: processes only see it through a `CONSOLE` handle with `READ`.
//! The PS/2 keyboard is the console of a laptop without a serial port
//! (Phase 34); real keyboard drivers (virtio, USB) stay in ring 3.

use crate::arch::x86_64::ioapic;
use crate::arch::x86_64::port::{inb, io_wait, outb};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use aricode_input::{Keyboard, Layout, ScancodeSet1};
use log::{info, warn};

const COM1: u16 = 0x3F8;
/// ISA IRQ of COM1.
const COM1_ISA_IRQ: u8 = 4;
/// Dedicated vector, outside the user device range (`hw`).
pub const SERIAL_VECTOR: u8 = 0x40;
/// PS/2 keyboard vector.
pub const KEYBOARD_VECTOR: u8 = 0x41;
const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_COMMAND: u16 = 0x64;
const PS2_ISA_IRQ: u8 = 1;
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

struct Ps2 {
    decoder: ScancodeSet1,
    keyboard: Keyboard,
    bytes: u64,
}

static PS2: IrqLock<Ps2> = IrqLock::new(Ps2 {
    decoder: ScancodeSet1::new(),
    keyboard: Keyboard::new(),
    bytes: 0,
});

/// Routes the COM1 IRQ to its dedicated vector and enables the UART receive
/// interrupt. Requires the I/O APIC and interrupts enabled.
pub fn init(layout: Layout) {
    PS2.lock().keyboard.set_layout(layout);
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(COM1_ISA_IRQ));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    let lapic_id = crate::arch::x86_64::apic::bsp_id();
    if let Err(e) = ioapic::route(gsi, SERIAL_VECTOR, lapic_id, level, low) {
        warn!("console: no serial input ({e})");
        return;
    }
    // SAFETY: COM1 IER; only enables the "data received" interrupt.
    unsafe { outb(COM1 + 1, 0x01) };
    info!("console: input on COM1 (GSI {gsi}, vector {SERIAL_VECTOR:#x})");
    init_ps2();
}

/// Bounded wait for the controller to accept a byte (input buffer empty).
/// Without a controller the status register reads 0xFF and it never holds.
fn ps2_can_write() -> bool {
    for _ in 0..50_000 {
        // SAFETY: read of the i8042 status register.
        if unsafe { inb(PS2_STATUS) } & 0x02 == 0 {
            return true;
        }
        io_wait();
    }
    false
}

fn ps2_read() -> Option<u8> {
    for _ in 0..50_000 {
        // SAFETY: i8042 status and data.
        unsafe {
            if inb(PS2_STATUS) & 0x01 != 0 {
                return Some(inb(PS2_DATA));
            }
        }
        io_wait();
    }
    None
}

fn ps2_command(cmd: u8) -> bool {
    if !ps2_can_write() {
        return false;
    }
    // SAFETY: command to the i8042 controller.
    unsafe { outb(PS2_COMMAND, cmd) };
    true
}

fn ps2_write_data(v: u8) -> bool {
    if !ps2_can_write() {
        return false;
    }
    // SAFETY: data byte to the i8042 (controller or keyboard).
    unsafe { outb(PS2_DATA, v) };
    true
}

fn ps2_flush() {
    for _ in 0..32 {
        // SAFETY: drain the i8042 output buffer.
        unsafe {
            if inb(PS2_STATUS) & 0x01 == 0 {
                break;
            }
            inb(PS2_DATA);
        }
    }
}

/// Initializes the i8042 controller and its keyboard if present: ports
/// disabled, configuration with IRQ1 and translation to set 1, self-test,
/// port 1 enabled, IRQ1 routed and keyboard scanning.
fn init_ps2() {
    ps2_command(0xAD);
    ps2_command(0xA7);
    ps2_flush();
    if !ps2_command(0x20) {
        info!("console: no PS/2 controller");
        return;
    }
    let Some(cfg) = ps2_read() else {
        info!("console: no PS/2 controller");
        return;
    };
    // IRQ1 on, IRQ12 off, port 1 clock enabled, translation to set 1.
    let cfg = (cfg | 0x01 | 0x40) & !0x02 & !0x10;
    ps2_command(0x60);
    ps2_write_data(cfg);
    ps2_command(0xAA);
    match ps2_read() {
        Some(0x55) => {}
        other => {
            info!("console: the PS/2 controller fails its self-test ({other:?})");
            return;
        }
    }
    // Some controllers lose their configuration on self-test.
    ps2_command(0x60);
    ps2_write_data(cfg);
    ps2_command(0xAE);
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(PS2_ISA_IRQ));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    if let Err(e) = ioapic::route(
        gsi,
        KEYBOARD_VECTOR,
        crate::arch::x86_64::apic::bsp_id(),
        level,
        low,
    ) {
        warn!("console: PS/2 keyboard without interrupt ({e})");
        return;
    }
    // Keyboard: enable scanning (0xF4); the ACK arrives via the interrupt or is discarded.
    ps2_write_data(0xF4);
    let _ = ps2_read();
    ps2_flush();
    info!(
        "console: PS/2 keyboard (GSI {gsi}, vector {KEYBOARD_VECTOR:#x}, layout {})",
        match PS2.lock().keyboard.layout() {
            Layout::Us => "us",
            Layout::Es => "es",
        }
    );
}

/// Keyboard vector handler: set 1 scancodes to text.
pub fn on_keyboard_irq() {
    crate::random::stir();
    let mut pushed = false;
    {
        let mut ps2 = PS2.lock();
        let mut ring = RING.lock();
        loop {
            // SAFETY: i8042 status and data; bit 5 = mouse data.
            let status = unsafe { inb(PS2_STATUS) };
            if status & 0x01 == 0 {
                break;
            }
            // SAFETY: same as above.
            let byte = unsafe { inb(PS2_DATA) };
            ps2.bytes += 1;
            if status & 0x20 != 0 {
                continue;
            }
            if let Some(ev) = ps2.decoder.feed(byte) {
                for &c in ps2.keyboard.feed(ev).as_slice() {
                    ring.push(c);
                    pushed = true;
                }
            }
        }
    }
    if pushed {
        WAITERS.wake_all();
    }
}

/// Serial vector handler: drains the receive FIFO into the ring.
pub fn on_irq() {
    crate::random::stir();
    let mut ring = RING.lock();
    // SAFETY: LSR bit 0 = data available; RBR returns the received byte.
    unsafe {
        while inb(COM1 + 5) & 0x01 != 0 {
            ring.push(inb(COM1));
        }
    }
    drop(ring);
    WAITERS.wake_all();
}

/// Copies up to `buf.len()` available bytes; waits for the first one until
/// `deadline` (ticks). Returns 0 only if the deadline expired.
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

/// Text from a user-space keyboard (syscall `console_inject`, ADR 0013) into
/// the same ring as COM1 and PS/2. Returns the bytes accepted; those that
/// do not fit are counted as dropped.
pub fn inject(bytes: &[u8]) -> usize {
    let mut ring = RING.lock();
    let mut n = 0;
    while n < bytes.len() && ring.len < CAPACITY {
        ring.push(bytes[n]);
        n += 1;
    }
    ring.dropped += (bytes.len() - n) as u64;
    drop(ring);
    if n > 0 {
        WAITERS.wake_all();
    }
    n
}

/// Bytes dropped because the ring was full (diagnostics).
pub fn dropped() -> u64 {
    RING.lock().dropped
}

/// Self-test (`test=console_ps2`): the host types a line on the PS/2
/// keyboard (QMP) on seeing `CONSOLE_PS2_READY` and the kernel receives it
/// through IRQ1.
pub fn ps2_selftest() {
    crate::println!("CONSOLE_PS2_READY");
    let deadline = crate::time::ticks() + 15 * crate::time::TICK_HZ;
    let mut line = alloc::vec::Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let n = read(&mut buf, Some(deadline));
        assert!(n > 0, "nobody typed on PS/2 within 15 s");
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                let text = core::str::from_utf8(&line).unwrap_or("?");
                crate::println!(
                    "console: PS/2 line '{text}' ({} bytes, {} scancodes)",
                    line.len(),
                    PS2.lock().bytes
                );
                return;
            }
            line.push(b);
        }
    }
}

/// Self-test (`test=console_input`): a deadline expires without input, then
/// a line sent by the runner on seeing `CONSOLE_INPUT_READY` is read.
pub fn selftest() {
    let mut buf = [0u8; 64];
    let deadline = Some(crate::time::ticks() + 10);
    assert_eq!(
        read(&mut buf, deadline),
        0,
        "without input the deadline must expire"
    );
    crate::println!("CONSOLE_INPUT_READY");
    let mut line = alloc::vec::Vec::new();
    loop {
        let n = read(&mut buf, None);
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                let text = core::str::from_utf8(&line).unwrap_or("?");
                crate::println!(
                    "console: line '{text}' ({} bytes, {} dropped)",
                    line.len(),
                    dropped()
                );
                return;
            }
            line.push(b);
        }
    }
}
