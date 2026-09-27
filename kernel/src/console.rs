//! Consola del kernel: serie siempre, framebuffer si existe. Implementa
//! `log::Log` para las macros `log::{trace,debug,info,warn,error}!`.
//!
//! La consola de framebuffer va tras un `IrqLock`. Si el lock está tomado
//! (una excepción dentro de la propia consola), el texto sale solo por serie
//! en vez de bloquear: perder una línea en pantalla es mejor que colgar.

use crate::framebuffer::MmioSurface;
use crate::serial::Serial;
use crate::sync::IrqLock;
use aricode_fb_console::Console;
use core::fmt::Write;
use log::{Level, LevelFilter, Log, Metadata, Record};

static FB: IrqLock<Option<Console<MmioSurface>>> = IrqLock::new(None);
/// Serializa cada mensaje completo por la serie: sin él, un `info!` del
/// kernel y un `console_write` de otro hilo se mezclan carácter a carácter.
/// No deshabilita interrupciones (la serie es lenta); con interrupciones
/// deshabilitadas (handler o sección crítica) solo se intenta, para no
/// esperar a un hilo que no puede correr.
static SERIAL: aricode_sync::SpinLock<()> = aricode_sync::SpinLock::new(());
static LOGGER: KernelLogger = KernelLogger;

/// Activa la consola de framebuffer. Idempotente; ignora si no hay GOP.
pub fn init_framebuffer(info: boot_protocol::FramebufferInfo) -> bool {
    let base = if info.address == 0 {
        0
    } else {
        crate::memory::phys::phys_to_virt(info.address)
    };
    match MmioSurface::new(info, base) {
        Some(surface) => {
            *FB.lock() = Some(Console::new(surface));
            true
        }
        None => false,
    }
}

/// Instala el logger global con el nivel máximo indicado.
pub fn init_logger(level: LevelFilter) {
    // set_logger solo falla si ya había uno: en ese caso mantener el existente.
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

/// Escribe texto sin formato de log en todas las salidas (marcadores, banner).
pub fn write_raw(args: core::fmt::Arguments) {
    let _serial = if crate::sync::interrupts_enabled() {
        Some(SERIAL.lock())
    } else {
        SERIAL.try_lock()
    };
    let _ = Serial.write_fmt(args);
    if let Some(mut guard) = FB.try_lock() {
        if let Some(fb) = guard.as_mut() {
            let _ = fb.write_fmt(args);
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
