//! Consola del kernel: serie siempre, framebuffer si existe. Implementa
//! `log::Log` para las macros `log::{trace,debug,info,warn,error}!`.
//!
//! Estado global en `sync::SingleCpu`: una CPU y sin interrupciones externas
//! hasta la Fase 9.

use crate::framebuffer::MmioSurface;
use crate::serial::Serial;
use crate::sync::SingleCpu;
use aricode_fb_console::Console;
use core::fmt::Write;
use log::{Level, LevelFilter, Log, Metadata, Record};

static FB: SingleCpu<Option<Console<MmioSurface>>> = SingleCpu::new(None);
static LOGGER: KernelLogger = KernelLogger;

/// Activa la consola de framebuffer. Idempotente; ignora si no hay GOP.
pub fn init_framebuffer(info: boot_protocol::FramebufferInfo) -> bool {
    match MmioSurface::new(info) {
        Some(surface) => {
            // SAFETY: ver `SingleCpu`.
            unsafe { *FB.get() = Some(Console::new(surface)) };
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
    let _ = Serial.write_fmt(args);
    // SAFETY: ver `SingleCpu`.
    if let Some(fb) = unsafe { &mut *FB.get() } {
        let _ = fb.write_fmt(args);
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
