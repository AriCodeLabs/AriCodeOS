//! `wasm_runner` (Fase 29 y etapa 4, ADR 0019): ejecuta `demo.wasm` de su
//! directorio privado con `aricode-wasm`. El módulo no ve syscalls ni POSIX:
//! solo las funciones que este anfitrión le importa bajo `aricode`, y solo
//! las que su manifiesto (`demo.manifest`, `caps = print, clock, files`)
//! le concede; su memoria lineal es un buffer DMA acotado por la cuota del
//! runner. El módulo se valida entero antes de instanciarse.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time, Handle};
use aricode_wasm::{Error, Host, Instance, Module, Value};

const MODULE_FILE: &str = "demo.wasm";
const MANIFEST_FILE: &str = "demo.manifest";
/// Memoria lineal del módulo: 4 páginas.
const MEMORY_BYTES: u64 = 4 * aricode_wasm::PAGE as u64;
/// Nombres de fichero que un módulo puede pasar a `file_*`.
const MAX_FILE_NAME: usize = 32;

fn say(msg: &[u8]) {
    sys::print(b"wasm_runner: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Capacidades que un módulo puede pedir en su manifiesto.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cap {
    Print,
    Clock,
    Files,
}

impl Cap {
    const NAMES: [(&'static str, Cap); 3] = [
        ("print", Cap::Print),
        ("clock", Cap::Clock),
        ("files", Cap::Files),
    ];
}

/// Capability API que ve el módulo, y la capacidad que exige cada función.
#[derive(Clone, Copy)]
enum Api {
    Print,
    UptimeMs,
    FileRead,
    FileWrite,
}

impl Api {
    fn by_name(name: &[u8]) -> Option<Api> {
        Some(match name {
            b"print" => Api::Print,
            b"uptime_ms" => Api::UptimeMs,
            b"file_read" => Api::FileRead,
            b"file_write" => Api::FileWrite,
            _ => return None,
        })
    }
    fn requires(self) -> Cap {
        match self {
            Api::Print => Cap::Print,
            Api::UptimeMs => Cap::Clock,
            Api::FileRead | Api::FileWrite => Cap::Files,
        }
    }
}

/// Manifiesto del módulo: `caps = a, b` (comentarios con `#`).
fn parse_caps(text: &str, out: &mut [bool; 3]) -> Result<(), &'static str> {
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("linea sin '='")?;
        if key.trim() != "caps" {
            return Err("clave desconocida");
        }
        for c in value.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let i = Cap::NAMES
                .iter()
                .position(|(n, _)| *n == c)
                .ok_or("capacidad desconocida")?;
            out[i] = true;
        }
    }
    Ok(())
}

struct AriHost {
    imports: Vec<Option<Api>>,
    system: Option<Handle>,
    fs: FsClient,
}

impl AriHost {
    /// Nombre de fichero desde la memoria del módulo: corto y sin `/`.
    fn file_name(memory: &[u8], ptr: usize, len: usize) -> Result<&str, Error> {
        if len == 0 || len > MAX_FILE_NAME {
            return Err(Error::Trap("nombre de fichero"));
        }
        let bytes = memory
            .get(
                ptr..ptr
                    .checked_add(len)
                    .ok_or(Error::Trap("nombre de fichero"))?,
            )
            .ok_or(Error::Trap("nombre fuera de memoria"))?;
        let name = core::str::from_utf8(bytes).map_err(|_| Error::Trap("nombre de fichero"))?;
        if name.contains('/') {
            return Err(Error::Trap("nombre de fichero"));
        }
        Ok(name)
    }
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
            // `file_write(name, name_len, data, data_len) -> bytes o -1`:
            // crea o reemplaza entero (atomico) en el directorio privado.
            Some(Api::FileWrite) => {
                let name = Self::file_name(memory, args[0] as usize, args[1] as usize)?;
                let (ptr, len) = (args[2] as usize, args[3] as usize);
                let data = memory
                    .get(ptr..ptr.checked_add(len).ok_or(Error::Trap("file_write"))?)
                    .ok_or(Error::Trap("datos fuera de memoria"))?;
                let Ok(fd) = self.fs.open(name).or_else(|_| self.fs.create(name)) else {
                    return Ok(u32::MAX as u64);
                };
                let r = self.fs.replace(fd, data);
                let _ = self.fs.close(fd);
                Ok(r.map_or(u32::MAX as u64, |n| n as u64))
            }
            // `file_read(name, name_len, buf, buf_len) -> bytes o -1`.
            Some(Api::FileRead) => {
                let name = Self::file_name(memory, args[0] as usize, args[1] as usize)?;
                let (ptr, len) = (args[2] as usize, args[3] as usize);
                // El nombre vive en la memoria del modulo: abrir antes de
                // tomar el buffer de salida, que la presta mutable.
                let Ok(fd) = self.fs.open(name) else {
                    return Ok(u32::MAX as u64);
                };
                let Some(out) = ptr
                    .checked_add(len)
                    .and_then(|end| memory.get_mut(ptr..end))
                else {
                    let _ = self.fs.close(fd);
                    return Err(Error::Trap("buffer fuera de memoria"));
                };
                let mut done = 0;
                while done < out.len() {
                    match self.fs.read(fd, done as u32, &mut out[done..]) {
                        Ok(0) => break,
                        Ok(n) => done += n,
                        Err(_) => {
                            done = usize::MAX;
                            break;
                        }
                    }
                }
                let _ = self.fs.close(fd);
                Ok(if done == usize::MAX {
                    u32::MAX as u64
                } else {
                    done as u64
                })
            }
            None => Err(Error::MissingImport),
        }
    }
}

fn describe(e: Error) -> &'static str {
    match e {
        Error::Malformed(_) => "modulo malformado",
        Error::Invalid(_) => "modulo invalido",
        Error::OutOfRange => "indice fuera de rango",
        Error::Unsupported(_) => "instruccion no soportada",
        Error::Trap(_) => "trap",
        Error::StackOverflow => "pila agotada",
        Error::Fuel => "demasiados pasos",
        Error::MissingImport => "importacion desconocida",
        Error::NoExport => "sin exportacion main",
    }
}

/// Detalle de un error de validación o de instrucción no soportada.
fn detail(e: Error) -> Option<&'static str> {
    match e {
        Error::Invalid(what) | Error::Unsupported(what) | Error::Malformed(what) => Some(what),
        _ => None,
    }
}

fn report(prefix: &[u8], e: Error) {
    sys::print(b"wasm_runner: ");
    sys::print(prefix);
    sys::print(describe(e).as_bytes());
    if let Some(d) = detail(e) {
        sys::print(b" (");
        sys::print(d.as_bytes());
        sys::print(b")");
    }
    sys::print(b"\n");
}

/// Lee un fichero entero del directorio privado (hasta `max` bytes).
fn read_file(fs: &mut FsClient, name: &str, max: usize) -> Result<Vec<u8>, u64> {
    let fd = fs.open(name).map_err(|_| 3u64)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match fs.read(fd, bytes.len() as u32, &mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(_) => {
                let _ = fs.close(fd);
                return Err(4);
            }
        }
        if bytes.len() > max {
            let _ = fs.close(fd);
            return Err(5);
        }
    }
    let _ = fs.close(fd);
    Ok(bytes)
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
    // 1. Modulo y su manifiesto desde el directorio privado.
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let bytes = match read_file(&mut fs, MODULE_FILE, 64 * 1024) {
        Ok(b) => b,
        Err(3) => {
            say(b"no encuentro demo.wasm en mi directorio");
            return 3;
        }
        Err(5) => {
            say(b"modulo demasiado grande");
            return 5;
        }
        Err(code) => return code,
    };
    let mut buf = [0u8; 20];
    sys::print(b"wasm_runner: modulo demo.wasm cargado (");
    sys::print(fmt_u64(bytes.len() as u64, &mut buf));
    sys::print(b" bytes)\n");
    let mut granted = [false; 3];
    match read_file(&mut fs, MANIFEST_FILE, 1024) {
        Ok(text) => {
            let parsed = core::str::from_utf8(&text)
                .map_err(|_| "no es texto")
                .and_then(|t| parse_caps(t, &mut granted));
            if let Err(why) = parsed {
                sys::print(b"wasm_runner: manifiesto del modulo invalido: ");
                sys::print(why.as_bytes());
                sys::print(b"\n");
                return 11;
            }
        }
        Err(_) => {
            say(b"el modulo no trae manifiesto (demo.manifest): no se ejecuta");
            return 11;
        }
    }
    sys::print(b"wasm_runner: manifiesto del modulo:");
    for (i, (name, _)) in Cap::NAMES.iter().enumerate() {
        if granted[i] {
            sys::print(b" ");
            sys::print(name.as_bytes());
        }
    }
    sys::print(b"\n");
    // 2. Analizar y validar; resolver importaciones por nombre y capacidad.
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            report(b"", e);
            return 6;
        }
    };
    let mut imports = Vec::new();
    for imp in &module.imports {
        let api = if imp.module == b"aricode" {
            Api::by_name(&imp.name)
        } else {
            None
        };
        let Some(api) = api else {
            sys::print(b"wasm_runner: importacion desconocida ");
            sys::print(&imp.module);
            sys::print(b".");
            sys::print(&imp.name);
            sys::print(b"\n");
            return 7;
        };
        let cap = api.requires();
        let allowed = Cap::NAMES
            .iter()
            .position(|(_, c)| *c == cap)
            .is_some_and(|i| granted[i]);
        if !allowed {
            sys::print(b"wasm_runner: importacion aricode.");
            sys::print(&imp.name);
            sys::print(b" no concedida por el manifiesto del modulo\n");
            return 12;
        }
        imports.push(Some(api));
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
            report(b"", e);
            return 9;
        }
    };
    let mut host = AriHost {
        imports,
        system,
        fs,
    };
    // 4. Inicializacion del modulo (funcion `start`) y ejecucion.
    if let Err(e) = instance.start(memory, &mut host) {
        report(b"la funcion start fallo: ", e);
        return 10;
    }
    match instance.call("main", &[arg], memory, &mut host) {
        Ok(code) => {
            sys::print(b"wasm_runner: main devolvio ");
            sys::print(fmt_u64(code as u32 as u64, &mut buf));
            sys::print(b"\n");
            code as u32 as u64
        }
        Err(e) => {
            report(b"el modulo fallo: ", e);
            13
        }
    }
}

aricode_rt::entry!(main);
