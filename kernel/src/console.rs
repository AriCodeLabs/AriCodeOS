// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
//! Kernel console: always serial, framebuffer if present. Implements
//! `log::Log` for the `log::{trace,debug,info,warn,error}!` macros.
//!
//! The framebuffer console sits behind an `IrqLock`. If the lock is held (an
//! exception inside the console itself), the text goes out on serial only
//! instead of blocking: losing a line on screen is better than hanging.

use crate::framebuffer::MmioSurface;
use crate::serial::Serial;
use crate::sync::IrqLock;
use aricode_fb_console::Console;
use core::fmt::Write;
use log::{Level, LevelFilter, Log, Metadata, Record};

/// The console grid (about 32 KiB) lives here, never on the stack: at boot
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
/// second. A dedicated thread rather than the tick or the idle loop:
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
            if guard.is_attached() {
                guard.blink();
            }
        }
    }
}

/// Writes text without log formatting to every output (markers, banner).
pub fn write_raw(args: core::fmt::Arguments) {
    let _serial = serial_guard();
    let _ = Serial.write_fmt(args);
    if let Some(mut guard) = FB.try_lock() {
        if guard.is_attached() {
            let _ = guard.write_fmt(args);
        }
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
