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
//! order serial does), then `LEASE`, a leaf (the cursor thread takes a
//! returned screen under `FB`; nothing takes `FB` under `LEASE`).

use crate::framebuffer::MmioSurface;
use crate::serial::Serial;
use crate::sync::IrqLock;
use alloc::vec::Vec;
use aricode_fb_console::Console;
use aricode_sync::IrqGuard;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// The screen changed size during boot, or went blank trying (ADR 0079): the
/// console starts over on it, if it had the screen, painted in steps (incident
/// 0031). What it showed stays on serial.
pub fn screen_resized(info: boot_protocol::FramebufferInfo) {
    let base = crate::memory::phys::phys_to_virt(info.address);
    let Some(surface) = MmioSurface::new(info, base) else {
        return;
    };
    let mut guard = wait_fb();
    if guard.is_attached() {
        guard.resize(surface);
        paint_in_steps(guard);
    }
}

/// Lends the screen to a user driver under test (`test=drivers`): the
/// console stops drawing, so its next scroll does not repaint what the
/// driver painted, until `take_back_framebuffer`. `None` without one.
pub fn lend_framebuffer() -> Option<MmioSurface> {
    wait_fb().detach()
}

/// The machine is ending (a power-off the firmware ignored), interrupts
/// are off on this CPU and the other CPUs halted, so no process runs
/// again: the console takes the screen back from its holders if it had
/// lent it, or takes `screen` if it never had one (`gui` on the command
/// line), and shows `last` on it, not only on serial (incident 0065). The
/// holders' mappings stay; nothing runs that could use them. Whether the
/// screen shows it.
pub fn last_words(screen: boot_protocol::FramebufferInfo, last: core::fmt::Arguments) -> bool {
    let surface = {
        let mut lease = LEASE.lock();
        lease.lent.take().or_else(|| lease.returned.take())
    };
    HELD.store(false, Ordering::Release);
    // Bounded: a CPU that panicked while holding the console is not halted
    // by anyone (this one has interrupts off), and serial still works.
    let Some(mut guard) = fb_guard_for(LAST_WORDS_SPINS) else {
        write_serial(format_args!("{last}\n"));
        return false;
    };
    let base =
        |info: boot_protocol::FramebufferInfo| crate::memory::phys::phys_to_virt(info.address);
    match surface {
        Some(surface) => guard.reattach(surface),
        None if !guard.is_attached() && screen.address != 0 => {
            if let Some(surface) = MmioSurface::new(screen, base(screen)) {
                guard.attach(surface);
            }
        }
        None => {}
    }
    let shown = guard.is_attached();
    let _ = Serial.write_fmt(format_args!("{last}\n"));
    guard.write_deferred(format_args!("{last}\n"));
    // Whole, whatever the interrupt state: no tick is left to lose
    // (incident 0031 is why every other path paints a row per hold).
    guard.redraw(usize::MAX);
    drop(guard);
    write_serial(format_args!(
        "console: {}\n",
        if shown {
            "the last lines are on the screen"
        } else {
            "no screen for the last lines"
        }
    ));
    shown
}

/// Turns of the spin loop [`last_words`] waits for the console's lock.
const LAST_WORDS_SPINS: u64 = 50_000_000;

/// The console's lock within `spins` turns, whatever the interrupt state.
fn fb_guard_for(spins: u64) -> Option<FbGuard> {
    for _ in 0..spins {
        if let Some(g) = FB.try_lock() {
            return Some(g);
        }
        core::hint::spin_loop();
    }
    None
}

/// Takes the lent screen back and redraws the console's text on it (the
/// lines logged while it was lent included).
pub fn take_back_framebuffer(surface: MmioSurface) {
    let mut guard = wait_fb();
    guard.reattach(surface);
    paint_in_steps(guard);
}

/// The screen while processes map it (ADR 0074 phase B): a compositor
/// started by init without `gui` on the command line takes a screen the
/// console was drawing on, and every kernel line would scroll the console
/// over its windows. Mapping the framebuffer lends the screen to the
/// mapper; when its last mapping goes (unmapped, or the process gone) the
/// console takes the screen back and draws its text again, the lines
/// logged meanwhile included. The capability to map it is the right to
/// hold it: init hands it only to the compositor.
struct Lease {
    /// The live mappings of the framebuffer: process and page address.
    holders: Vec<(u32, u64)>,
    /// The console's surface while lent.
    lent: Option<MmioSurface>,
    /// A surface whose last holder went, for the cursor thread to give back
    /// to the console: the release can run with interrupts off (a dying
    /// process is reaped right after a context switch), where the console
    /// is never repainted whole (incident 0031).
    returned: Option<MmioSurface>,
    /// A first holder came and its lease was not said yet.
    announce: bool,
}

/// Whether any process maps the framebuffer: kept with `LEASE.holders`,
/// read without a lock by the PS/2 keyboard's interrupt handler.
static HELD: AtomicBool = AtomicBool::new(false);

/// Whether a process holds the screen (maps the framebuffer), whether or
/// not the console had it to lend: the console's keyboard follows the
/// console's screen (`console_input`).
pub fn screen_held() -> bool {
    HELD.load(Ordering::Acquire)
}

static LEASE: IrqLock<Lease> = IrqLock::new(Lease {
    holders: Vec::new(),
    lent: None,
    returned: None,
    announce: false,
});

/// Process `pid` mapped (part of) the framebuffer at page `virt`. Only
/// records it: `process::map_foreign` calls it under the process table's
/// lock, so the holders always match the mappings; the syscall then calls
/// [`screen_take`], which takes the screen from the console.
pub fn screen_hold(pid: u32, virt: u64) {
    let mut lease = LEASE.lock();
    if lease.holders.is_empty() {
        lease.announce = true;
        // A new session: the PS/2 stream starts clean, before the keys
        // start going to it (ADR 0075).
        crate::console_input::stream_cut();
    }
    lease.holders.push((pid, virt));
    HELD.store(true, Ordering::Release);
}

/// Takes the screen from the console while anyone holds it. Thread code:
/// the `mmio_map` syscall, after [`screen_hold`].
pub fn screen_take() {
    let detach = {
        let mut lease = LEASE.lock();
        if lease.holders.is_empty() || lease.lent.is_some() {
            false
        } else if let Some(surface) = lease.returned.take() {
            // Released and mapped again before the cursor thread gave it
            // back: the console never had it.
            lease.lent = Some(surface);
            false
        } else {
            true
        }
    };
    let surface = if detach { wait_fb().detach() } else { None };
    let (holder, lent) = {
        let mut lease = LEASE.lock();
        let holder = lease.holders.first().map(|&(pid, _)| pid);
        match (surface, holder) {
            (Some(s), Some(_)) => lease.lent = Some(s),
            // Unmapped meanwhile: straight back.
            (Some(s), None) => lease.returned = Some(s),
            (None, _) => {}
        }
        let announce = core::mem::take(&mut lease.announce);
        (holder.filter(|_| announce), lease.lent.is_some())
    };
    match holder {
        Some(pid) if lent => log::info!(
            "console: the screen is lent to process {pid}, which mapped it; until it is given back the kernel console writes to serial only and gets no PS/2 keys"
        ),
        // The console had no screen to lend (`gui` on the command line, or
        // no display); its keyboard follows the screen all the same.
        Some(pid) => log::info!(
            "console: process {pid} holds the screen; the console gets no PS/2 keys meanwhile"
        ),
        None => {}
    }
}

/// Process `pid` unmapped its framebuffer mapping at page `virt`, or, with
/// `None`, is gone with all of them. When that was the last holder the
/// console gets its keyboard back, and the screen, if it had lent one, is
/// queued for it: `Some(lent)` then, for the caller to say so once it
/// holds no lock ([`screen_returned_said`]). Paints nothing, so it is safe
/// with interrupts off and under the process table's lock.
#[must_use]
pub fn screen_release(pid: u32, virt: Option<u64>) -> Option<bool> {
    let mut lease = LEASE.lock();
    let before = lease.holders.len();
    lease
        .holders
        .retain(|&(p, v)| p != pid || virt.is_some_and(|virt| v != virt));
    if lease.holders.len() == before || !lease.holders.is_empty() {
        return None;
    }
    HELD.store(false, Ordering::Release);
    // What the stream still holds was typed at the session that ended.
    crate::console_input::stream_cut();
    lease.announce = false;
    let lent = lease.lent.take();
    let had = lent.is_some();
    if lent.is_some() {
        lease.returned = lent;
    }
    Some(had)
}

/// The line for the last holder [`screen_release`] let go: the screen (if
/// the console had lent it) and the keyboard are the console's again, and
/// what the PS/2 keyboard typed meanwhile at the holder's screen was not
/// given to it.
pub fn screen_returned_said(pid: u32, lent: bool) {
    let withheld = crate::console_input::take_withheld();
    if lent {
        log::info!(
            "console: the screen is back from process {pid}; the kernel console draws on it and gets the PS/2 keys again ({withheld} character(s) typed meanwhile were not given to it)"
        );
    } else {
        log::info!(
            "console: process {pid} no longer holds the screen; the console gets the PS/2 keys again ({withheld} character(s) typed meanwhile were not given to it)"
        );
    }
}

/// Ctrl+Alt+Backspace was typed on the PS/2 keyboard while the screen was
/// held: the tick it was typed at (at least 1), 0 if it was not. Set from
/// the keyboard's interrupt handler, which kills nothing itself; the cursor
/// thread acts on it ([`stop_holders`]).
static WANTED_AT: AtomicU64 = AtomicU64::new(0);

/// How long the screen's holders have to give it back by themselves once
/// the owner asked for it (ADR 0075 §4): the compositor hears the chord
/// through `ps2-input` and leaves, so its clients end cleanly.
const GIVE_BACK_TICKS: u64 = 2 * crate::time::TICK_HZ;

/// The owner asked for the screen back on the PS/2 keyboard (ADR 0074
/// phase B). Interrupt context: only notes when.
pub fn screen_wanted_back() {
    let now = crate::time::ticks().max(1);
    let _ = WANTED_AT.compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
}

/// Stops every process still holding the screen 2 s after the owner asked
/// for it on the PS/2 keyboard: on a laptop whose only other way out is the
/// desktop itself, this is how the text console is always reachable even
/// when the compositor hangs (ADR 0074 §4, ADR 0075 §4). Its death returns
/// the screen; init then ends the desktop. Thread code.
fn stop_holders() {
    let at = WANTED_AT.load(Ordering::Acquire);
    if at == 0 {
        return;
    }
    if !HELD.load(Ordering::Acquire) {
        // Given back in time: nothing to stop.
        WANTED_AT.store(0, Ordering::Release);
        return;
    }
    if crate::time::ticks() < at + GIVE_BACK_TICKS {
        return;
    }
    WANTED_AT.store(0, Ordering::Release);
    let mut pids: Vec<u32> = LEASE.lock().holders.iter().map(|&(p, _)| p).collect();
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        log::info!(
            "console: Ctrl+Alt+Backspace on the PS/2 keyboard: process {pid} still holds the screen after 2 s and is stopped so the console gets it back"
        );
        crate::user::process::request_kill(
            crate::user::process::Pid(pid),
            "stopped by Ctrl+Alt+Backspace on the PS/2 keyboard",
        );
    }
}

/// A screen whose holders all went: reattached to the console, which then
/// needs a full repaint. Thread code (the cursor thread).
fn take_back_returned(guard: &mut FbGuard) {
    if let Some(surface) = LEASE.lock().returned.take() {
        guard.reattach(surface);
    }
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
        stop_holders();
        if let Some(mut guard) = FB.try_lock() {
            take_back_returned(&mut guard);
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
