// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Kernel console: always serial, framebuffer if present. Implements
//! `log::Log` for the `log::{trace,debug,info,warn,error}!` macros.
//!
//! The framebuffer console sits behind an `IrqLock`. Every hold of it is
//! short: a message is queued into the console's cells, and the screen is
//! painted one text row per hold, so interrupts are never off for a whole
//! repaint (incident 0031: timer ticks were lost during each scroll).
//! Thread code (interrupts enabled) waits for the lock and paints everything
//! pending, row by row. Code running with interrupts off (handlers,
//! exceptions) only tries for a bounded time and paints one row; the rest
//! is left to the next thread writer or to the cursor thread, which paints
//! whatever is pending before it blinks. If the lock stays held (an
//! exception inside the console itself) the text goes out on serial only
//! instead of blocking: losing a line on screen is better than hanging.
//!
//! Lock order: `SERIAL` then `FB` (a message is queued for the screen
//! before its serial lock is released, so the screen shows messages in the
//! order serial does); `FB` is a leaf.

use crate::framebuffer::MmioSurface;
use crate::serial::Serial;
use crate::sync::IrqLock;
use aricode_fb_console::Console;
use aricode_sync::IrqGuard;
use core::fmt::Write;
use log::{Level, LevelFilter, Log, Metadata, Record};

/// The console (its text and the shadow of the screen, about 64 KiB) lives here, never on the stack: at boot
/// the kernel runs on AriBoot's 64 KiB stack and building the console by
/// value overflowed it (corrupting the adjacent initrd).
static FB: IrqLock<Console<MmioSurface>> = IrqLock::new(Console::empty());
/// Serializes each whole message on serial: without it, a kernel `info!`
/// and a `console_write` from another thread interleave character by
/// character. It does not disable interrupts (serial is slow); with
/// interrupts disabled (handler or critical section) it only tries, so as
/// not to wait on a thread that cannot run.
static SERIAL: aricode_sync::SpinLock<()> = aricode_sync::SpinLock::new(());
static LOGGER: KernelLogger = KernelLogger;

/// Enables the framebuffer console. Idempotent; does nothing without GOP.
pub fn init_framebuffer(info: boot_protocol::FramebufferInfo) -> bool {
    let base = if info.address == 0 {
        0
    } else {
        crate::memory::phys::phys_to_virt(info.address)
    };
    match MmioSurface::new(info, base) {
        Some(surface) => {
            FB.lock().attach(surface);
            true
        }
        None => false,
    }
}

/// Lends the screen to a user driver under test (`test=drivers`): the
/// console stops drawing, so its next scroll does not repaint what the
/// driver painted, until `take_back_framebuffer`. `None` without one.
pub fn lend_framebuffer() -> Option<MmioSurface> {
    wait_fb().detach()
}

/// Takes the lent screen back and redraws the console's text on it (the
/// lines logged while it was lent included).
pub fn take_back_framebuffer(surface: MmioSurface) {
    let mut guard = wait_fb();
    guard.reattach(surface);
    paint_in_steps(guard);
}

/// Text rows painted per hold of `FB`. A row is at most one glyph per
/// column (about 20 000 pixel stores at 1280 px wide), so interrupts stay
/// off for a row, never for a whole screen.
const ROWS_PER_HOLD: usize = 1;

/// Paints what is pending, `ROWS_PER_HOLD` rows per hold, releasing the
/// lock (and so restoring interrupts) between rows: the tick and other
/// CPUs' writers get in between. Whoever holds the lock next paints the
/// rows still dirty, whatever wrote them, so a writer preempted between
/// rows leaves nothing inconsistent. Thread code only.
fn paint_in_steps(mut guard: FbGuard) {
    while !guard.redraw(ROWS_PER_HOLD) {
        drop(guard);
        guard = wait_fb();
    }
}

type FbGuard = IrqGuard<'static, Console<MmioSurface>, crate::sync::KernelIrq>;

/// Waits for `FB` from thread code, without `IrqLock::lock`'s deadlock
/// panic (audit M2 of incident 0031): every holder keeps interrupts off,
/// so it runs on another CPU, holds the lock for one row (one glyph for
/// the cursor) and waits on nothing, so the wait always ends; the policy
/// is undone between attempts, as `lock` does, so this CPU keeps taking
/// interrupts meanwhile. Losing the race to other writers only delays it.
fn wait_fb() -> FbGuard {
    loop {
        if let Some(g) = FB.try_lock() {
            return g;
        }
        core::hint::spin_loop();
    }
}

/// Console lock for one message, as `serial_guard`: with interrupts
/// enabled it waits (`wait_fb`); with them disabled (handler, exception)
/// it only retries for a bounded time, since this same CPU may be the
/// holder (an exception inside the console).
fn fb_guard(interrupts: bool) -> Option<FbGuard> {
    if interrupts {
        return Some(wait_fb());
    }
    for _ in 0..200_000 {
        if let Some(g) = FB.try_lock() {
            return Some(g);
        }
        core::hint::spin_loop();
    }
    None
}

/// Installs the global logger with the given maximum level.
pub fn init_logger(level: LevelFilter) {
    // set_logger only fails if one was already set: keep the existing one then.
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

/// Serial lock for one message. With interrupts enabled it waits; with them
/// disabled (handler, exception, panic) it only retries for a bounded time:
/// another CPU releases it quickly, but this same CPU (re-entry from an
/// exception inside the console) never would.
fn serial_guard() -> Option<aricode_sync::IrqGuard<'static, (), aricode_sync::NoIrq>> {
    if crate::sync::interrupts_enabled() {
        return Some(SERIAL.lock());
    }
    for _ in 0..200_000 {
        if let Some(g) = SERIAL.try_lock() {
            return Some(g);
        }
        core::hint::spin_loop();
    }
    None
}

/// Serial only, one whole message under the lock (exceptions and panic).
pub fn write_serial(args: core::fmt::Arguments) {
    let _serial = serial_guard();
    let _ = Serial.write_fmt(args);
}

/// Kernel thread that toggles the framebuffer console cursor twice per
/// second. Before blinking it paints whatever is still pending (rows left
/// by a writer with interrupts off, or by one preempted between rows), in
/// steps: `blink` itself draws at most one glyph. A dedicated thread rather than the tick or the idle loop:
/// drawing from a handler is not appropriate, and one more call in the idle
/// loop (which spins a lot) slowed the whole system down by 25% in QEMU. If
/// the console is busy the blink is skipped; it only blinks when idle.
pub fn start_cursor_thread() {
    if let Err(e) = crate::task::scheduler::spawn("cursor", cursor_thread, 0) {
        log::warn!("console: no cursor thread ({e})");
    }
}

extern "C" fn cursor_thread(_arg: usize) {
    loop {
        crate::task::scheduler::sleep_ms(500);
        if let Some(mut guard) = FB.try_lock() {
            if guard.needs_redraw() {
                paint_in_steps(guard);
            } else if guard.is_attached() {
                guard.blink();
            }
        }
    }
}

/// Writes text without log formatting to every output (markers, banner).
/// Serial first; the message is queued into the screen console's cells
/// before the serial lock is released, so both show messages in the same
/// order. The text is queued even while the screen is lent out
/// (`lend_framebuffer`), to be drawn when it comes back. Then the screen
/// is painted once per message (a message of many lines scrolls the cells
/// many times but paints only the cells that changed), without the serial
/// lock: painting does not hold up other writers' serial output.
pub fn write_raw(args: core::fmt::Arguments) {
    let interrupts = crate::sync::interrupts_enabled();
    let guard = {
        let _serial = serial_guard();
        let _ = Serial.write_fmt(args);
        let Some(mut guard) = fb_guard(interrupts) else {
            return;
        };
        guard.write_deferred(args);
        guard
    };
    if interrupts {
        paint_in_steps(guard);
    } else {
        // Interrupts are off already: one row now (a line that did not
        // scroll is all there is), the rest by the next thread writer or
        // the cursor thread, never a whole screen in this hold.
        let mut guard = guard;
        guard.redraw(ROWS_PER_HOLD);
    }
}

struct KernelLogger;

impl Log for KernelLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let tag = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };
        write_raw(format_args!("[{tag}] {}\n", record.args()));
    }

    fn flush(&self) {}
}
