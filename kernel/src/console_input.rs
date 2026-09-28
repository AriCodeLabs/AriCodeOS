//! Entrada de consola (Fases 21 y 34): recepción por COM1 y por el teclado
//! PS/2 del i8042, ambas con interrupción. Los handlers dejan los bytes en
//! un anillo común y despiertan a quien espera en `console_read`. La consola
//! es del kernel: los procesos solo la ven a través de un handle `CONSOLE`
//! con `READ`. El teclado PS/2 es la consola de un portátil sin puerto serie
//! (Fase 34); los teclados de verdad (virtio, USB) siguen en ring 3.

use crate::arch::x86_64::ioapic;
use crate::arch::x86_64::port::{inb, io_wait, outb};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use aricode_input::{Keyboard, ScancodeSet1};
use log::{info, warn};

const COM1: u16 = 0x3F8;
/// IRQ ISA de COM1.
const COM1_ISA_IRQ: u8 = 4;
/// Vector propio, fuera del rango de dispositivos de usuario (`hw`).
pub const SERIAL_VECTOR: u8 = 0x40;
/// Vector del teclado PS/2.
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

/// Enruta la IRQ de COM1 al vector propio y habilita la interrupción de
/// recepción del UART. Requiere I/O APIC e interrupciones activas.
pub fn init() {
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(COM1_ISA_IRQ));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    let lapic_id = crate::arch::x86_64::apic::bsp_id();
    if let Err(e) = ioapic::route(gsi, SERIAL_VECTOR, lapic_id, level, low) {
        warn!("console: sin entrada por serie ({e})");
        return;
    }
    // SAFETY: IER de COM1; solo habilita la interrupción "dato recibido".
    unsafe { outb(COM1 + 1, 0x01) };
    info!("console: entrada por COM1 (GSI {gsi}, vector {SERIAL_VECTOR:#x})");
    init_ps2();
}

/// Espera acotada a que el controlador acepte un byte (buffer de entrada
/// vacío). Sin controlador el registro de estado lee 0xFF y nunca se cumple.
fn ps2_can_write() -> bool {
    for _ in 0..50_000 {
        // SAFETY: lectura del registro de estado del i8042.
        if unsafe { inb(PS2_STATUS) } & 0x02 == 0 {
            return true;
        }
        io_wait();
    }
    false
}

fn ps2_read() -> Option<u8> {
    for _ in 0..50_000 {
        // SAFETY: estado y dato del i8042.
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
    // SAFETY: orden al controlador i8042.
    unsafe { outb(PS2_COMMAND, cmd) };
    true
}

fn ps2_write_data(v: u8) -> bool {
    if !ps2_can_write() {
        return false;
    }
    // SAFETY: byte de datos al i8042 (controlador o teclado).
    unsafe { outb(PS2_DATA, v) };
    true
}

fn ps2_flush() {
    for _ in 0..32 {
        // SAFETY: vaciar el buffer de salida del i8042.
        unsafe {
            if inb(PS2_STATUS) & 0x01 == 0 {
                break;
            }
            inb(PS2_DATA);
        }
    }
}

/// Inicializa el controlador i8042 y su teclado si existen: puertos
/// deshabilitados, configuración con IRQ1 y traducción al conjunto 1,
/// autotest, puerto 1 habilitado, IRQ1 enrutada y teclado explorando.
fn init_ps2() {
    ps2_command(0xAD);
    ps2_command(0xA7);
    ps2_flush();
    if !ps2_command(0x20) {
        info!("console: sin controlador PS/2");
        return;
    }
    let Some(cfg) = ps2_read() else {
        info!("console: sin controlador PS/2");
        return;
    };
    // IRQ1 sí, IRQ12 no, reloj del puerto 1 activo, traducción al conjunto 1.
    let cfg = (cfg | 0x01 | 0x40) & !0x02 & !0x10;
    ps2_command(0x60);
    ps2_write_data(cfg);
    ps2_command(0xAA);
    match ps2_read() {
        Some(0x55) => {}
        other => {
            info!("console: el controlador PS/2 no supera el autotest ({other:?})");
            return;
        }
    }
    // Algunos controladores pierden la configuración con el autotest.
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
        warn!("console: teclado PS/2 sin interrupcion ({e})");
        return;
    }
    // Teclado: explorar (0xF4); el ACK llega por la interrupción o se descarta.
    ps2_write_data(0xF4);
    let _ = ps2_read();
    ps2_flush();
    info!("console: teclado PS/2 (GSI {gsi}, vector {KEYBOARD_VECTOR:#x})");
}

/// Handler del vector del teclado: scancodes del conjunto 1 a texto.
pub fn on_keyboard_irq() {
    let mut pushed = false;
    {
        let mut ps2 = PS2.lock();
        let mut ring = RING.lock();
        loop {
            // SAFETY: estado y dato del i8042; bit 5 = dato del ratón.
            let status = unsafe { inb(PS2_STATUS) };
            if status & 0x01 == 0 {
                break;
            }
            // SAFETY: ídem.
            let byte = unsafe { inb(PS2_DATA) };
            ps2.bytes += 1;
            if status & 0x20 != 0 {
                continue;
            }
            if let Some(ev) = ps2.decoder.feed(byte) {
                if let Some(c) = ps2.keyboard.feed(ev) {
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

/// Texto de un teclado de usuario (syscall `console_inject`, ADR 0013) al
/// mismo anillo que COM1 y el PS/2. Devuelve los bytes aceptados; los que
/// no caben se cuentan como descartados.
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

/// Bytes descartados por anillo lleno (diagnóstico).
pub fn dropped() -> u64 {
    RING.lock().dropped
}

/// Autoprueba (`test=console_ps2`): el host teclea una línea en el teclado
/// PS/2 (QMP) al ver `CONSOLE_PS2_READY` y el kernel la recibe por IRQ1.
pub fn ps2_selftest() {
    crate::println!("CONSOLE_PS2_READY");
    let deadline = crate::time::ticks() + 15 * crate::time::TICK_HZ;
    let mut line = alloc::vec::Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let n = read(&mut buf, Some(deadline));
        assert!(n > 0, "nadie tecleo por PS/2 en 15 s");
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                let text = core::str::from_utf8(&line).unwrap_or("?");
                crate::println!(
                    "console: linea PS/2 '{text}' ({} bytes, {} scancodes)",
                    line.len(),
                    PS2.lock().bytes
                );
                return;
            }
            line.push(b);
        }
    }
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
