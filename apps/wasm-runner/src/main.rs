// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `wasm_runner` (Phase 29 and stage 4, ADR 0019): runs `demo.wasm` from its
//! private directory with `aricode-wasm`. The module sees no syscalls or
//! POSIX: only the functions this host imports into it under `aricode`, and
//! only those its manifest (`demo.manifest`, `caps = print, clock, files`)
//! grants; its linear memory is a DMA buffer bounded by the runner's quota.
//! The whole module is validated before it is instantiated.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time, Handle};
use aricode_wasm::{bytes_cost, Error, FuncType, Host, Instance, Module, ValType, Value};

const MODULE_FILE: &str = "demo.wasm";
const MANIFEST_FILE: &str = "demo.manifest";
/// Module linear memory: 4 pages.
const MEMORY_BYTES: u64 = 4 * aricode_wasm::PAGE as u64;
/// Maximum length of file names a module can pass to `file_*`.
const MAX_FILE_NAME: usize = 32;
/// Fuel an import costs on top of the bytes it moves (`bytes_cost`), so a
/// module's fuel also bounds the time it makes the system work for it
/// (incident 0042, audit M-a). A `file_write` is a durable AriFS replace
/// (journal and data written to disk): at most 50 per call of 50 M units.
const FILE_WRITE_COST: u64 = 1_000_000;
/// A `file_read`: open, IPC reads and close with the filesystem service.
const FILE_READ_COST: u64 = 10_000;
/// A `print`: a system call to the console, plus one unit per byte (the
/// console is far slower than memory).
const PRINT_COST: u64 = 1_000;
/// An `uptime_ms`: one system call.
const CLOCK_COST: u64 = 100;

fn say(msg: &[u8]) {
    sys::print(b"wasm_runner: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// Capabilities a module can request in its manifest.
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

/// Capability API seen by the module, and the capability each function requires.
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

    /// Signature the host implements; the module import must declare exactly
    /// this one (audit 6ee19fb, finding 5).
    fn signature(self) -> (&'static [ValType], &'static [ValType]) {
        use ValType::{I32, I64};
        match self {
            Api::Print => (&[I32, I32], &[]),
            Api::UptimeMs => (&[], &[I64]),
            Api::FileRead | Api::FileWrite => (&[I32, I32, I32, I32], &[I32]),
        }
    }
}

fn same_signature(ty: &FuncType, params: &[ValType], results: &[ValType]) -> bool {
    ty.params.as_slice() == params && ty.results.as_slice() == results
}

/// Module manifest: `caps = a, b` (comments with `#`).
fn parse_caps(text: &str, out: &mut [bool; 3]) -> Result<(), &'static str> {
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("line without '='")?;
        if key.trim() != "caps" {
            return Err("unknown key");
        }
        for c in value.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            let i = Cap::NAMES
                .iter()
                .position(|(n, _)| *n == c)
                .ok_or("unknown capability")?;
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
    /// File name from module memory: short and without `/`.
    fn file_name(memory: &[u8], ptr: usize, len: usize) -> Result<&str, Error> {
        if len == 0 || len > MAX_FILE_NAME {
            return Err(Error::Trap("file name"));
        }
        let bytes = memory
            .get(ptr..ptr.checked_add(len).ok_or(Error::Trap("file name"))?)
            .ok_or(Error::Trap("name out of memory"))?;
        let name = core::str::from_utf8(bytes).map_err(|_| Error::Trap("file name"))?;
        if name.contains('/') {
            return Err(Error::Trap("file name"));
        }
        Ok(name)
    }
}

impl Host for AriHost {
    fn cost(&self, import: usize, args: &[Value]) -> u64 {
        // Lengths are i32 arguments; a larger claim than memory traps in
        // `call`, or runs out of fuel first.
        let len = |i: usize| args.get(i).map_or(0, |&v| v as u32 as usize);
        match self.imports.get(import).copied().flatten() {
            Some(Api::Print) => PRINT_COST + len(1) as u64,
            Some(Api::UptimeMs) => CLOCK_COST,
            Some(Api::FileRead) => FILE_READ_COST + bytes_cost(len(3)),
            Some(Api::FileWrite) => FILE_WRITE_COST + bytes_cost(len(3)),
            None => 0,
        }
    }

    fn call(&mut self, import: usize, args: &[Value], memory: &mut [u8]) -> Result<Value, Error> {
        match self.imports.get(import).copied().flatten() {
            Some(Api::Print) => {
                let (ptr, len) = (args[0] as usize, args[1] as usize);
                let bytes = memory
                    .get(ptr..ptr.checked_add(len).ok_or(Error::Trap("print"))?)
                    .ok_or(Error::Trap("print out of memory"))?;
                sys::print(bytes);
                Ok(0)
            }
            Some(Api::UptimeMs) => Ok(self
                .system
                .and_then(|s| time::uptime_ms(s).ok())
                .unwrap_or(0)),
            // `file_write(name, name_len, data, data_len) -> bytes or -1`:
            // creates or replaces the whole file (atomically) in the private directory.
            Some(Api::FileWrite) => {
                let name = Self::file_name(memory, args[0] as usize, args[1] as usize)?;
                let (ptr, len) = (args[2] as usize, args[3] as usize);
                let data = memory
                    .get(ptr..ptr.checked_add(len).ok_or(Error::Trap("file_write"))?)
                    .ok_or(Error::Trap("data out of memory"))?;
                let Ok(fd) = self.fs.open(name).or_else(|_| self.fs.create(name)) else {
                    return Ok(u32::MAX as u64);
                };
                let r = self.fs.replace(fd, data);
                let _ = self.fs.close(fd);
                Ok(r.map_or(u32::MAX as u64, |n| n as u64))
            }
            // `file_read(name, name_len, buf, buf_len) -> bytes or -1`.
            Some(Api::FileRead) => {
                let name = Self::file_name(memory, args[0] as usize, args[1] as usize)?;
                let (ptr, len) = (args[2] as usize, args[3] as usize);
                // The name lives in module memory: open before taking the
                // output buffer, which borrows that memory mutably.
                let Ok(fd) = self.fs.open(name) else {
                    return Ok(u32::MAX as u64);
                };
                let Some(out) = ptr
                    .checked_add(len)
                    .and_then(|end| memory.get_mut(ptr..end))
                else {
                    let _ = self.fs.close(fd);
                    return Err(Error::Trap("buffer out of memory"));
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
        Error::Malformed(_) => "malformed module",
        Error::Invalid(_) => "invalid module",
        Error::OutOfRange => "index out of range",
        Error::Unsupported(_) => "unsupported instruction",
        Error::Trap(_) => "trap",
        Error::StackOverflow => "stack exhausted",
        Error::Fuel => "too many steps",
        Error::MissingImport => "unknown import",
        Error::NoExport => "no main export",
        Error::OutOfMemory => "module too large for the runner's memory",
    }
}

/// Detail of a validation error or an unsupported instruction.
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

/// Reads a whole file from the private directory (up to `max` bytes).
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
        say(b"need fs and hardware (see the manifest)");
        return 1;
    };
    // 1. Module and its manifest from the private directory.
    let Ok(mut fs) = FsClient::new(ep, hw) else {
        return 2;
    };
    let bytes = match read_file(&mut fs, MODULE_FILE, 64 * 1024) {
        Ok(b) => b,
        Err(3) => {
            say(b"cannot find demo.wasm in my directory");
            return 3;
        }
        Err(5) => {
            say(b"module too large");
            return 5;
        }
        Err(code) => return code,
    };
    let mut buf = [0u8; 20];
    sys::print(b"wasm_runner: module demo.wasm loaded (");
    sys::print(fmt_u64(bytes.len() as u64, &mut buf));
    sys::print(b" bytes)\n");
    let mut granted = [false; 3];
    match read_file(&mut fs, MANIFEST_FILE, 1024) {
        Ok(text) => {
            let parsed = core::str::from_utf8(&text)
                .map_err(|_| "not text")
                .and_then(|t| parse_caps(t, &mut granted));
            if let Err(why) = parsed {
                sys::print(b"wasm_runner: invalid module manifest: ");
                sys::print(why.as_bytes());
                sys::print(b"\n");
                return 11;
            }
        }
        Err(_) => {
            say(b"module has no manifest (demo.manifest): not running");
            return 11;
        }
    }
    sys::print(b"wasm_runner: module manifest:");
    for (i, (name, _)) in Cap::NAMES.iter().enumerate() {
        if granted[i] {
            sys::print(b" ");
            sys::print(name.as_bytes());
        }
    }
    sys::print(b"\n");
    // 2. Parse and validate; resolve imports by name and capability.
    let module = match Module::parse(&bytes) {
        Ok(m) => m,
        Err(e) => {
            report(b"", e);
            return 6;
        }
    };
    let mut imports = Vec::new();
    if imports.try_reserve_exact(module.imports().len()).is_err() {
        report(b"", Error::OutOfMemory);
        return 6;
    }
    for imp in module.imports() {
        let api = if imp.module == b"aricode" {
            Api::by_name(&imp.name)
        } else {
            None
        };
        let Some(api) = api else {
            sys::print(b"wasm_runner: unknown import ");
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
            sys::print(b"wasm_runner: import aricode.");
            sys::print(&imp.name);
            sys::print(b" not granted by the module manifest\n");
            return 12;
        }
        let (params, results) = api.signature();
        let declared = module.types().get(imp.type_index as usize);
        if !declared.is_some_and(|ty| same_signature(ty, params, results)) {
            sys::print(b"wasm_runner: import aricode.");
            sys::print(&imp.name);
            sys::print(b" has an incompatible signature\n");
            return 14;
        }
        imports.push(Some(api));
    }
    // `main(i64) -> i32`: if the module exports anything else, it is not run.
    let main_ok = module
        .exported_function("main")
        .and_then(|i| module.function_type(i).ok())
        .is_some_and(|ty| same_signature(ty, &[ValType::I64], &[ValType::I32]));
    if !main_ok {
        say(b"main has an incompatible signature (expected main(i64) -> i32)");
        return 15;
    }
    // 3. Linear memory in a DMA buffer (counts against the quota).
    let Ok(dma) = sys::dma_alloc(hw, MEMORY_BYTES) else {
        say(b"no memory for the module");
        return 8;
    };
    let Ok(virt) = sys::mmio_map(dma) else {
        return 8;
    };
    // SAFETY: our own DMA buffer of MEMORY_BYTES, mapped RW.
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
    // 4. Module initialization (`start` function) and execution.
    if let Err(e) = instance.start(memory, &mut host) {
        report(b"start function failed: ", e);
        return 10;
    }
    match instance.call("main", &[arg], memory, &mut host) {
        Ok(code) => {
            sys::print(b"wasm_runner: main returned ");
            sys::print(fmt_u64(code as u32 as u64, &mut buf));
            sys::print(b"\n");
            code as u32 as u64
        }
        Err(e) => {
            report(b"module failed: ", e);
            13
        }
    }
}

aricode_rt::entry!(main);
