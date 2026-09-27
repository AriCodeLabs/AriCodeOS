//! `wasm_runner` (Fase 29): ejecuta `demo.wasm` de su directorio privado con
//! `aricode-wasm`. El módulo no ve syscalls ni POSIX: solo las funciones que
//! este anfitrión le importa bajo `aricode` (`print`, `uptime_ms`), y su
//! memoria lineal es un buffer DMA acotado por la cuota del manifiesto.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time, Handle};
use aricode_wasm::{Error, Host, Instance, Module, Value};

const MODULE_FILE: &str = "demo.wasm";
/// Memoria lineal del módulo: 4 páginas.
const MEMORY_BYTES: u64 = 4 * aricode_wasm::PAGE as u64;

fn say(msg: &[u8]) {
    sys::print(b"wasm_runner: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Capability API que ve el módulo.
#[derive(Clone, Copy)]
enum Api {
    Print,
    UptimeMs,
}

struct AriHost {
    imports: Vec<Option<Api>>,
    system: Option<Handle>,
}

impl Host for AriHost {
    fn call(&mut self, import: usize, args: &[Value], memory: &mut [u8]) -> Result<Value, Error> {
        match self.imports.get(import).copied().flatten() {
            Some(Api::Print) => {
                let (ptr, len) = (args[0] as usize, args[1] as usize);
                let bytes = memory
                    .get(ptr..ptr.checked_add(len).ok_or(Error::Trap("print"))?)
                    .ok_or(Error::Trap("print fuera de memoria"))?;
                sys::print(bytes);
                Ok(0)
            }
            Some(Api::UptimeMs) => Ok(self
                .system
                .and_then(|s| time::uptime_ms(s).ok())
                .unwrap_or(0)),
            None => Err(Error::MissingImport),
        }
    }
}

fn describe(e: Error) -> &'static str {
    match e {
        Error::Malformed(_) => "modulo malformado",
        Error::OutOfRange => "indice fuera de rango",
        Error::Unsupported(_) => "instruccion no soportada",
        Error::Trap(_) => "trap",
        Error::StackOverflow => "pila agotada",
        Error::Fuel => "demasiados pasos",
        Error::MissingImport => "importacion desconocida",
        Error::NoExport => "sin exportacion main",
    }
}

fn main(arg: u64) -> u64 {
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
    let (Some(ep), Some(hw)) = (fs_ep, hardware) else {
        say(b"necesito fs y hardware (ver el manifiesto)");
        return 1;
    };
    // 1. Cargar el modulo desde el directorio privado.
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let Ok(fd) = fs.open(MODULE_FILE) else {
        say(b"no encuentro demo.wasm en mi directorio");
        return 3;
    };
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match fs.read(fd, bytes.len() as u32, &mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(_) => return 4,
        }
        if bytes.len() > 64 * 1024 {
            say(b"modulo demasiado grande");
            return 5;
        }
    }
    let _ = fs.close(fd);
    let mut buf = [0u8; 20];
    sys::print(b"wasm_runner: modulo demo.wasm cargado (");
    sys::print(fmt_u64(bytes.len() as u64, &mut buf));
    sys::print(b" bytes)\n");
    // 2. Analizar y resolver importaciones por nombre.
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            say(describe(e).as_bytes());
            return 6;
        }
    };
    let mut imports = Vec::new();
    for imp in &module.imports {
        let api = if imp.module == b"aricode" {
            match imp.name.as_slice() {
                b"print" => Some(Api::Print),
                b"uptime_ms" => Some(Api::UptimeMs),
                _ => None,
            }
        } else {
            None
        };
        if api.is_none() {
            sys::print(b"wasm_runner: importacion desconocida ");
            sys::print(&imp.module);
            sys::print(b".");
            sys::print(&imp.name);
            sys::print(b"\n");
            return 7;
        }
        imports.push(api);
    }
    // 3. Memoria lineal en un buffer DMA (cuenta contra la cuota).
    let Ok(dma) = sys::dma_alloc(hw, MEMORY_BYTES) else {
        say(b"sin memoria para el modulo");
        return 8;
    };
    let Ok(virt) = sys::mmio_map(dma) else {
        return 8;
    };
    // SAFETY: buffer DMA propio de MEMORY_BYTES, mapeado RW.
    let memory = unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, MEMORY_BYTES as usize) };
    let mut instance = match Instance::new(&module, memory) {
        Ok(i) => i,
        Err(e) => {
            say(describe(e).as_bytes());
            return 9;
        }
    };
    let mut host = AriHost { imports, system };
    // 4. Ejecutar.
    match instance.call("main", &[arg], memory, &mut host) {
        Ok(code) => {
            sys::print(b"wasm_runner: main devolvio ");
            sys::print(fmt_u64(code as u32 as u64, &mut buf));
            sys::print(b"\n");
            code as u32 as u64
        }
        Err(e) => {
            sys::print(b"wasm_runner: el modulo fallo: ");
            sys::print(describe(e).as_bytes());
            sys::print(b"\n");
            10
        }
    }
}

aricode_rt::entry!(main);
