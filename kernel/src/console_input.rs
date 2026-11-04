// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Console input (Phases 21 and 34): reception on COM1 and on the i8042 PS/2
//! keyboard, both interrupt-driven. The handlers put the bytes in a shared
//! ring and wake whoever waits in `console_read`. The console belongs to the
//! kernel: processes only see it through a `CONSOLE` handle with `READ`.
//! The PS/2 keyboard is the console of a laptop without a serial port
//! (Phase 34); real keyboard drivers (virtio, USB) stay in ring 3.
//!
//! While a process holds the screen the i8042's bytes, the keyboard's and
//! the aux port's (touchpad, mouse), go untouched into the PS/2 stream
//! instead, for the ring-3 `ps2-input` to decode (ADR 0075).

use crate::arch::x86_64::ioapic;
use crate::arch::x86_64::port::{inb, io_wait, outb};
use crate::sync::IrqLock;
use crate::task::wait::WaitQueue;
use aricode_abi::ps2;
use aricode_input::{EscapeChord, Keyboard, Layout, Ps2Queue, ScancodeSet1};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use log::{info, warn};

const COM1: u16 = 0x3F8;
/// ISA IRQ of COM1.
const COM1_ISA_IRQ: u8 = 4;
/// Dedicated vector, outside the user device range (`hw`).
pub const SERIAL_VECTOR: u8 = 0x40;
/// PS/2 keyboard vector.
pub const KEYBOARD_VECTOR: u8 = 0x41;
/// PS/2 aux port (touchpad, mouse) vector (ADR 0075).
pub const AUX_VECTOR: u8 = 0x42;
const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_COMMAND: u16 = 0x64;
const PS2_ISA_IRQ: u8 = 1;
const PS2_AUX_ISA_IRQ: u8 = 12;
/// Device acknowledgement of a command.
const PS2_ACK: u8 = 0xFA;
/// Device request to send the last byte again.
const PS2_RESEND: u8 = 0xFE;
const CAPACITY: usize = 1024;

struct Ring {
    buf: [u8; CAPACITY],
    head: usize,
    len: usize,
    dropped: u64,
    /// Process that read the console last: what is queued is typed for it.
    reader: Option<u32>,
    /// That reader died (it is named here until a new one reads): what is
    /// queued meanwhile was typed for it too (incident 0047).
    dead_reader: Option<u32>,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; CAPACITY],
            head: 0,
            len: 0,
            dropped: 0,
            reader: None,
            dead_reader: None,
        }
    }
    /// Empties the ring; returns how many bytes it held.
    fn discard(&mut self) -> usize {
        let n = self.len;
        self.head = 0;
        self.len = 0;
        n
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
    /// Bytes read from the i8042 (the keyboard's acknowledgements included).
    bytes: u64,
    /// `bytes` when the last line ended (the byte that produced the newline
    /// included), for `ps2_selftest`.
    bytes_at_newline: u64,
    /// Characters typed while a process held the screen, never given to
    /// the console (ADR 0074 phase B).
    withheld: u64,
    /// Ctrl+Alt+Backspace: while a process holds the screen, the owner's
    /// way back to the console (ADR 0074 phase B).
    escape: EscapeChord,
}

static PS2: IrqLock<Ps2> = IrqLock::new(Ps2 {
    decoder: ScancodeSet1::new(),
    keyboard: Keyboard::new(),
    bytes: 0,
    bytes_at_newline: 0,
    withheld: 0,
    escape: EscapeChord::new(),
});

/// The PS/2 stream (ADR 0075): what the i8042 delivered while a process
/// held the screen, as (tag, byte) entries for `ps2-input`. The console
/// never sees these bytes; the stream never sees the console's.
static STREAM: IrqLock<Ps2Queue> = IrqLock::new(Ps2Queue::new());
static STREAM_WAITERS: WaitQueue = WaitQueue::new();

/// Whether the bytes typed while the screen is held go to the stream
/// (`ps2=console` keeps them from it: they are withheld, as before ADR
/// 0075). Set once at boot.
static PUMP: AtomicBool = AtomicBool::new(false);

/// What the controller has (`ps2::KIND_*`), plus `PRESENT` once it passed
/// its self-test. Set once at boot.
static FOUND: AtomicU64 = AtomicU64::new(0);
const PRESENT: u64 = 1 << 63;

/// `Some(ps2::KIND_* bits)` if the machine has an i8042 that passed its
/// self-test and its stream is for the desktop (not `ps2=console`): init
/// then gets the stream (`INIT_PS2`).
pub fn ps2_stream_kinds() -> Option<u64> {
    let found = FOUND.load(Ordering::Acquire);
    (found & PRESENT != 0 && PUMP.load(Ordering::Acquire)).then_some(found & !PRESENT)
}

/// The stream is cut: a process took the screen, or the last one gave it
/// back. What it held was for the session that ended, so it is emptied,
/// and the reader starts over from a reset mark. Under `LEASE`; takes only
/// the stream's lock and wakes nobody (the reader sees the mark with the
/// next byte, or at its next look).
pub fn stream_cut() {
    STREAM.lock().cut();
}

/// Whether the stream has entries or a reset mark for its reader.
pub fn stream_ready() -> bool {
    STREAM.lock().pending() > 0
}

/// The entries waiting (a reset mark counts as one), for `receive_any`.
pub fn stream_pending() -> u32 {
    STREAM.lock().pending() as u32
}

/// The queue woken when bytes enter the stream (`receive_any`).
pub fn stream_waiters() -> &'static WaitQueue {
    &STREAM_WAITERS
}

/// Copies whole entries into `out` for process `pid` (`ps2_read`): a reset
/// mark first if the stream was cut, then the oldest entries. Returns the
/// bytes written; `None` if another live process drains the stream.
pub fn stream_read(pid: u32, out: &mut [u8]) -> Option<usize> {
    let (n, dropped) = {
        let mut stream = STREAM.lock();
        (stream.read(pid, out), stream.take_dropped())
    };
    if dropped > 0 {
        warn!("console: {dropped} PS/2 byte(s) dropped: the stream was full (its reader is told to start over)");
    }
    n
}

/// Entries `stream_read` took could not be delivered (the reader's buffer
/// went away meanwhile): the reader starts over from a reset mark.
pub fn stream_lost() {
    STREAM.lock().lost();
}

/// Characters the PS/2 keyboard produced while a process held the screen,
/// since the last call: they were typed at that process's screen, never at
/// the console, so the console never got them (ADR 0074 phase B).
pub fn take_withheld() -> u64 {
    core::mem::take(&mut PS2.lock().withheld)
}

/// Routes the COM1 IRQ to its dedicated vector and enables the UART receive
/// interrupt. Requires the I/O APIC and interrupts enabled.
pub fn init(layout: Layout, pump: bool) {
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
    init_ps2(pump);
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

/// Sends `byte` to the keyboard (port 1), or with `aux` to the aux port,
/// and waits for the device's acknowledgement. Only before the ports'
/// interrupts are on (`init_ps2`): the reply is polled.
fn ps2_device_command(aux: bool, byte: u8) -> bool {
    // A device that answers "resend" (0xFE) gets the byte again, as Linux's
    // i8042 driver does, a few times at most.
    for _ in 0..3 {
        if aux && !ps2_command(0xD4) {
            return false;
        }
        if !ps2_write_data(byte) {
            return false;
        }
        // A device may queue a byte of its own before the acknowledgement.
        for _ in 0..4 {
            match ps2_read() {
                Some(PS2_ACK) => return true,
                Some(PS2_RESEND) => break,
                Some(_) => {}
                None => return false,
            }
        }
    }
    false
}

/// Routes ISA `irq` to `vector` on the boot CPU.
fn ps2_route(irq: u8, vector: u8) -> Result<u32, &'static str> {
    let (gsi, ov) = crate::acpi::with(|a| a.madt.isa_irq_to_gsi(irq));
    let (level, low) = ov
        .map(|o| (o.level_triggered(), o.active_low()))
        .unwrap_or((false, false));
    ioapic::route(gsi, vector, crate::arch::x86_64::apic::bsp_id(), level, low).map(|()| gsi)
}

/// Brings up the aux port's device (ADR 0075): its defaults, the
/// IntelliMouse wheel if it has one (sample rates 200, 100, 80, then its ID
/// reads 3), and reporting. Returns its `ps2::KIND_*` bits (0 if nothing
/// acknowledged). The port is enabled and the interrupts still off.
fn init_aux() -> u64 {
    if !ps2_device_command(true, 0xF6) {
        return 0;
    }
    let mut kinds = ps2::KIND_POINTER;
    let rates = [200, 100, 80];
    if rates
        .iter()
        .all(|&r| ps2_device_command(true, 0xF3) && ps2_device_command(true, r))
        && ps2_device_command(true, 0xF2)
        && ps2_read() == Some(3)
    {
        kinds |= ps2::KIND_WHEEL;
    }
    // The wheel sequence left the rate at 80: back to the default.
    let _ = ps2_device_command(true, 0xF3) && ps2_device_command(true, 100);
    if !ps2_device_command(true, 0xF4) {
        return 0;
    }
    kinds
}

/// Initializes the i8042 controller and its devices: both ports disabled
/// and their interrupts off while the devices are set up by polling (so no
/// handler takes an acknowledgement), translation to set 1, self-test,
/// keyboard scanning on port 1, the aux port's pointer if the controller
/// has one (ADR 0075), then the interrupts on and routed. `pump`: the
/// bytes typed while the screen is held go to the stream.
fn init_ps2(pump: bool) {
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
    // Interrupts off, port 1 clock enabled, translation to set 1.
    let setup = (cfg | 0x40) & !(0x01 | 0x02 | 0x10);
    ps2_command(0x60);
    ps2_write_data(setup);
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
    ps2_write_data(setup);
    // A second port exists if enabling it clears its clock-disabled bit.
    // Its interface test answers 0, but many chipsets answer 0xFA or 0xFF
    // on a working port, which Linux accepts too; the device's own
    // acknowledgement (`init_aux`) is what proves a pointer is there.
    ps2_command(0xA8);
    let aux_port = ps2_command(0x20)
        && ps2_read().is_some_and(|c| c & 0x20 == 0)
        && ps2_command(0xA7)
        && ps2_command(0xA9)
        && matches!(ps2_read(), Some(0x00 | 0xFA | 0xFF));
    ps2_command(0xA7);
    ps2_flush();
    // Each device is set up with the other port disabled, so a key pressed
    // or a touchpad touched meanwhile cannot land where a reply is
    // expected (the wheel's ID above all: a wrong one would decode 4-byte
    // packets as 3-byte ones the whole boot).
    let mut found = 0;
    if aux_port {
        ps2_command(0xA8);
        found |= init_aux();
        ps2_command(0xA7);
    }
    ps2_command(0xAE);
    if ps2_device_command(false, 0xF4) {
        found |= ps2::KIND_KEYBOARD;
    }
    let pointer = found & ps2::KIND_POINTER != 0;
    // The I/O APIC entries are routed before the controller may raise
    // either line: an edge on a still masked entry would be lost, and with
    // its byte left in the shared output buffer no other byte, and so no
    // other edge, would ever come (audit of ADR 0075, H1).
    let gsi = match ps2_route(PS2_ISA_IRQ, KEYBOARD_VECTOR) {
        Ok(gsi) => gsi,
        Err(e) => {
            warn!("console: PS/2 keyboard without interrupt ({e})");
            return;
        }
    };
    let aux_gsi = if pointer {
        match ps2_route(PS2_AUX_ISA_IRQ, AUX_VECTOR) {
            Ok(gsi) => Some(gsi),
            Err(e) => {
                warn!("console: PS/2 pointer without interrupt ({e})");
                found &= !(ps2::KIND_POINTER | ps2::KIND_WHEEL);
                None
            }
        }
    } else {
        None
    };
    let pointer = found & ps2::KIND_POINTER != 0;
    let mut cfg = setup | 0x01;
    if pointer {
        ps2_command(0xA8);
        cfg = (cfg | 0x02) & !0x20;
    } else {
        cfg |= 0x20;
    }
    FOUND.store(found | PRESENT, Ordering::Release);
    PUMP.store(pump, Ordering::Release);
    ps2_command(0x60);
    ps2_write_data(cfg);
    // Whatever came in before the interrupts were on is taken now, through
    // the handler, so the buffer is empty for the next edge.
    on_ps2_irq();
    info!(
        "console: PS/2 keyboard{} (GSI {gsi}, vector {KEYBOARD_VECTOR:#x}, layout {})",
        if found & ps2::KIND_KEYBOARD != 0 {
            ""
        } else {
            " port (no keyboard acknowledged)"
        },
        match PS2.lock().keyboard.layout() {
            Layout::Us => "us",
            Layout::Es => "es",
        }
    );
    if let Some(gsi) = aux_gsi {
        info!(
            "console: PS/2 pointer{} (GSI {gsi}, vector {AUX_VECTOR:#x})",
            if found & ps2::KIND_WHEEL != 0 {
                " with a wheel"
            } else {
                ""
            }
        );
    }
    info!(
        "console: {}",
        if pump {
            "while a process holds the screen, the PS/2 bytes go to its input service"
        } else {
            "ps2=console: the PS/2 keyboard serves only the console"
        }
    );
}

/// Keyboard and aux vector handler: the i8042 has one output buffer for
/// both ports, so either interrupt drains it all. Keyboard bytes become
/// text for the console (set 1 scancodes); while a process holds the screen
/// they and the aux bytes go to the stream instead (ADR 0075), and without
/// one the aux bytes are dropped.
pub fn on_ps2_irq() {
    crate::random::stir();
    let mut pushed = false;
    let mut streamed = false;
    {
        let mut ps2 = PS2.lock();
        let mut ring = RING.lock();
        let pump = PUMP.load(Ordering::Relaxed);
        loop {
            // SAFETY: i8042 status and data; bit 5 = aux data.
            let status = unsafe { inb(PS2_STATUS) };
            if status & 0x01 == 0 {
                break;
            }
            // SAFETY: same as above.
            let byte = unsafe { inb(PS2_DATA) };
            ps2.bytes += 1;
            let held = crate::console::screen_held();
            if status & 0x20 != 0 {
                if held && pump {
                    streamed |= STREAM.lock().push(ps2::TAG_AUX, byte);
                }
                continue;
            }
            if held && pump {
                streamed |= STREAM.lock().push(ps2::TAG_KEYBOARD, byte);
            }
            if let Some(ev) = ps2.decoder.feed(byte) {
                // Decoded either way, so a modifier held across the change
                // is not left stuck.
                let chord = ps2.escape.feed(ev);
                let typed = ps2.keyboard.feed(ev);
                // The person typing looks at the screen, and while a process
                // (the compositor) holds it the keys are not typed at the
                // console: a shell must never read what was meant for a
                // window (ADR 0074 phase B). Ctrl+Alt+Backspace asks for the
                // screen back, the only key the kernel acts on then.
                if held {
                    if chord {
                        crate::console::screen_wanted_back();
                    } else {
                        ps2.withheld += typed.as_slice().len() as u64;
                    }
                    continue;
                }
                for &c in typed.as_slice() {
                    if c == b'\n' || c == b'\r' {
                        ps2.bytes_at_newline = ps2.bytes;
                    }
                    ring.push(c);
                    pushed = true;
                }
            }
        }
    }
    if pushed {
        WAITERS.wake_all();
    }
    if streamed {
        STREAM_WAITERS.wake_all();
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

/// Process `pid` is about to read the console (`console_read`). If the
/// reader before it died, what is still queued was typed for that reader
/// (a shell that crashed with a line half read): it is discarded instead
/// of reaching this one, where it could run as a command (incident 0047).
/// Input typed before anyone has read, at boot, stays for the first
/// reader.
pub fn take(pid: u32) {
    let (discarded, dead) = {
        let mut ring = RING.lock();
        if ring.reader == Some(pid) {
            return;
        }
        ring.reader = Some(pid);
        match ring.dead_reader.take() {
            Some(dead) => (ring.discard(), dead),
            None => return,
        }
    };
    if discarded > 0 {
        info!(
            "console: {discarded} byte(s) typed after its reader (process {dead}) died discarded"
        );
    }
}

/// Process `pid` is finishing. If it was the console's reader, what it
/// left unread is discarded, and so is whatever arrives before the next
/// reader takes the console (`take`). Called before anyone can see that
/// it exited, so its successor never reads first.
pub fn process_gone(pid: u32) {
    STREAM.lock().reader_gone(pid);
    let discarded = {
        let mut ring = RING.lock();
        if ring.reader != Some(pid) {
            return;
        }
        ring.reader = None;
        ring.dead_reader = Some(pid);
        ring.discard()
    };
    if discarded > 0 {
        info!("console: {discarded} typed byte(s) discarded: their reader (process {pid}) is gone");
    }
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
/// through IRQ1. Each byte is echoed as it arrives, like a console: the
/// runner types the next key only after the echo of the previous one
/// (incident 0022), so the i8042's 16-byte queue never overflows. The
/// scancodes reported are those up to the Enter press, the keyboard's
/// acknowledgement of its set-up included: counting them when the line is
/// printed also took in the Enter release or not depending on how fast the
/// console printed (incident 0031).
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
                // Read first: a guard inside the arguments would be held
                // for the whole console write.
                let scancodes = PS2.lock().bytes_at_newline;
                crate::println!();
                crate::println!(
                    "console: PS/2 line '{text}' ({} bytes, {scancodes} scancodes)",
                    line.len(),
                );
                return;
            }
            crate::print!("{}", b as char);
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

/// Self-test (`test=console_orphan`, incident 0047): a reader takes one
/// byte of a line the runner types after `CONSOLE_ORPHAN_READY` and dies
/// with the rest unread, as a crashing shell would; the reader started
/// next must not receive those bytes (they could run as a command).
pub fn orphan_selftest() {
    use crate::object::{KernelObject, ObjectKind};
    use crate::user::process::{self, ExitStatus};
    use aricode_handles::Rights;

    let run = |arg: u64| {
        let pid = process::spawn_with_handles_and_arg(
            "console_orphan",
            crate::user::programs::by_name("console_orphan").expect("console_orphan in the initrd"),
            alloc::vec![(KernelObject::new(ObjectKind::Console), Rights::READ)],
            arg,
            0,
        )
        .expect("console_orphan");
        process::wait(pid)
    };
    let first = run(0);
    assert!(
        matches!(first, ExitStatus::Killed(_)),
        "the first reader must die by its fault: {first:?}"
    );
    assert_eq!(
        run(1),
        ExitStatus::Exited(0),
        "console: bytes typed for a reader that died reached the next one"
    );
    info!("console orphan selftest ok (what a dead reader left unread did not reach the next one)");
}
