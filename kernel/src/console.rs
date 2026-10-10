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

/// La rejilla de la consola (unas 32 KiB) vive aquí, nunca en la pila: al
/// arrancar, el kernel corre sobre la pila de 64 KiB de AriBoot y construir
/// la consola por valor la desbordaba (corrompía el initrd contiguo).
static FB: IrqLock<Console<MmioSurface>> = IrqLock::new(Console::empty());
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
            FB.lock().attach(surface);
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

/// Lock de la serie para un mensaje. Con interrupciones habilitadas espera;
/// con ellas deshabilitadas (handler, excepción, pánico) solo insiste un rato
/// acotado: otra CPU lo suelta enseguida, pero la propia CPU (reentrada
/// desde una excepción dentro de la consola) no lo haría nunca.
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

/// Solo serie, un mensaje completo bajo el lock (excepciones y pánico).
pub fn write_serial(args: core::fmt::Arguments) {
    let _serial = serial_guard();
    let _ = Serial.write_fmt(args);
}

/// Alterna el cursor de la consola de framebuffer (desde el tick del reloj;
/// si la consola está ocupada se salta el parpadeo).
pub fn blink_cursor() {
    if let Some(mut guard) = FB.try_lock() {
        if guard.is_attached() {
            guard.blink();
        }
    }
}

/// Escribe texto sin formato de log en todas las salidas (marcadores, banner).
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
