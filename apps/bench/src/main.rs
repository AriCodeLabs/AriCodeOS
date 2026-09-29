//! `bench` (etapa 5): medidas reproducibles desde una aplicación normal del
//! broker (`docs/performance.md`): latencia de una syscall, de una llamada
//! IPC completa a fsd y de operaciones de fichero, con percentiles p50, p95 y
//! p99 sobre el TSC calibrado contra el reloj del sistema. Los números solo
//! se comparan entre ejecuciones con la misma build y el mismo modo de QEMU.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time, Handle};

const SYSCALLS: usize = 10_000;
const IPC_CALLS: usize = 1_000;
const FILE_OPS: usize = 100;
const CHUNK: usize = 4096;

fn rdtsc() -> u64 {
    // SAFETY: rdtsc solo lee el contador de tiempo.
    unsafe { core::arch::x86_64::_rdtsc() }
}

fn num(v: u64) {
    let mut b = [0u8; 20];
    sys::print(fmt_u64(v, &mut b));
}

/// Percentil `p` (0..=100) de muestras ya ordenadas.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len() - 1) * p / 100;
    sorted[idx]
}

/// Imprime `bench: NOMBRE (N): p50 X UNIDAD p95 Y UNIDAD p99 Z UNIDAD`.
fn report(name: &[u8], samples: &mut [u64], cycles_per_unit: u64, unit: &[u8]) {
    samples.sort_unstable();
    sys::print(b"bench: ");
    sys::print(name);
    sys::print(b" (");
    num(samples.len() as u64);
    sys::print(b"):");
    for (label, p) in [(b" p50 " as &[u8], 50), (b" p95 ", 95), (b" p99 ", 99)] {
        sys::print(label);
        num(percentile(samples, p) / cycles_per_unit.max(1));
        sys::print(b" ");
        sys::print(unit);
    }
    sys::print(b"\n");
}

/// Ciclos de TSC por milisegundo, medidos contra el reloj del sistema.
fn calibrate(system: Handle) -> Option<u64> {
    let start = time::uptime_us(system).ok()?;
    let t0 = rdtsc();
    time::sleep_ms(200);
    let t1 = rdtsc();
    let end = time::uptime_us(system).ok()?;
    let us = end.saturating_sub(start);
    (us > 0).then(|| (t1 - t0) * 1000 / us)
}

fn main(_arg: u64) -> u64 {
    let (mut system, mut fs_ep, mut hardware) = (None, None, None);
    permissions::granted(|h, _| {
        if permissions::is(h, kind::SYSTEM) {
            system = Some(h);
        } else if permissions::is(h, kind::ENDPOINT) {
            fs_ep = Some(h);
        } else if permissions::is(h, kind::HARDWARE) {
            hardware = Some(h);
        }
    });
    let (Some(system), Some(ep), Some(hw)) = (system, fs_ep, hardware) else {
        sys::print(b"bench: necesito system y fs (ver el manifiesto)\n");
        return 1;
    };
    let Some(per_ms) = calibrate(system) else {
        sys::print(b"bench: no se pudo calibrar el TSC\n");
        return 2;
    };
    let per_us = (per_ms / 1000).max(1);
    let per_ns = per_ms / 1_000_000; // 0 por debajo de 1 GHz: se usa per_us*1000
    sys::print(b"bench: calibracion ");
    num(per_ms);
    sys::print(b" ciclos TSC por ms\n");
    // 1. Syscall minima.
    let mut samples = Vec::with_capacity(SYSCALLS);
    for _ in 0..SYSCALLS {
        let t0 = rdtsc();
        let _ = sys::thread_get_tid();
        samples.push(rdtsc() - t0);
    }
    // En ns: ciclos * 1000 / ciclos_por_us evita la division por cero.
    for s in samples.iter_mut() {
        *s = *s * 1000 / per_us;
    }
    let _ = per_ns;
    report(b"syscall thread_get_tid", &mut samples, 1, b"ns");
    // 2. IPC completa a fsd (peticion, respuesta, endpoint por llamada).
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 3;
    };
    let mut name = [0u8; 32];
    let mut samples = Vec::with_capacity(IPC_CALLS);
    for _ in 0..IPC_CALLS {
        let t0 = rdtsc();
        if fs.list(0, &mut name).is_err() {
            return 4;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"ipc fsd list", &mut samples, per_us, b"us");
    // 3. Ficheros: reemplazo atomico y lectura de 4 KiB.
    let fd = match fs.open("medidas").or_else(|_| fs.create("medidas")) {
        Ok(fd) => fd,
        Err(_) => return 5,
    };
    let data = alloc::vec![0x5Au8; CHUNK];
    let mut samples = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let t0 = rdtsc();
        if fs.replace(fd, &data) != Ok(CHUNK) {
            return 6;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"replace 4 KiB", &mut samples, per_us, b"us");
    let mut back = alloc::vec![0u8; CHUNK];
    let mut samples = Vec::with_capacity(FILE_OPS);
    for _ in 0..FILE_OPS {
        let t0 = rdtsc();
        if fs.read(fd, 0, &mut back) != Ok(CHUNK) {
            return 7;
        }
        samples.push(rdtsc() - t0);
    }
    report(b"read 4 KiB", &mut samples, per_us, b"us");
    let _ = fs.close(fd);
    sys::print(b"bench: fin\n");
    0
}

aricode_rt::entry!(main);
