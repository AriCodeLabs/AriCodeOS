// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! `wasm_runner` (Phase 29, ADR 0019, ADR 0071): runs `demo.wasm` from its
//! private directory with wasmi. The module sees no syscalls or POSIX: only
//! the functions this host imports into it under `aricode`, and only those
//! its manifest (`demo.manifest`, `caps = print, clock, files`) grants. The
//! whole module is validated before it is instantiated; its memories, tables
//! and wasmi's own state live on a heap the runner extends with DMA buffers,
//! bounded by its quota, and every call runs on fuel (ADR 0055).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use aricode_rt::fmt_u64;
use aricode_rt::sys::fs::FsClient;
use aricode_rt::sys::{self, kind, permissions, time, Handle};
use aricode_wasm_host::{
    bytes_cost, charge, check_nesting, engine, file_name, granted, is_main, limits, parse_caps,
    writable, Api, Cap, CLOCK_COST, FILE_READ_COST, FILE_WRITE_COST, FUEL, MANIFEST_FILE,
    MAX_FILE_NAME, MAX_MODULE, MODULE_FILE, PRINT_COST,
};
use wasmi::{Caller, Error, Extern, ExternType, Linker, Module, Store, StoreLimits, TrapCode};

/// The heap the runner adds to its own (ADR 0071), in DMA buffers counted
/// against its quota: physically contiguous, so it asks for several
/// smaller ones rather than one. Each holds a 1 MiB memory with room.
const HEAP_CHUNK: u64 = 2 * 1024 * 1024;
const HEAP_CHUNKS: usize = 3;

fn say(msg: &[u8]) {
    sys::print(b"wasm_runner: ");
    sys::print(msg);
    sys::print(b"\n");
}

/// What the host functions reach: the clock, the private directory, and
/// the limits wasmi enforces on the module's memories and tables.
struct HostState {
    system: Option<Handle>,
    fs: FsClient,
    limits: StoreLimits,
}

type Ctx<'a> = Caller<'a, HostState>;

fn trap(what: &'static str) -> Error {
    Error::new(what)
}

/// The memory the module exports as `memory`, where its pointers point.
fn memory(caller: &Ctx) -> Result<wasmi::Memory, Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| trap("the module exports no memory"))
}

fn range(
    memory: &[u8],
    ptr: i32,
    len: i32,
    what: &'static str,
) -> Result<core::ops::Range<usize>, Error> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    let end = ptr.checked_add(len).ok_or(trap(what))?;
    if end > memory.len() {
        return Err(trap(what));
    }
    Ok(ptr..end)
}

/// File name from module memory (`aricode_wasm_host::file_name`), copied
/// out so the memory can be lent to the filesystem call.
fn name_from(memory: &[u8], ptr: i32, len: i32) -> Result<([u8; MAX_FILE_NAME], usize), Error> {
    if !(1..=MAX_FILE_NAME as i32).contains(&len) {
        return Err(trap("file name"));
    }
    let bytes = &memory[range(memory, ptr, len, "name out of memory")?];
    file_name(bytes).ok_or(trap("file name"))?;
    let mut out = [0u8; MAX_FILE_NAME];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok((out, bytes.len()))
}

fn define(linker: &mut Linker<HostState>, api: Api) -> Result<(), Error> {
    match api {
        Api::Print => {
            linker.func_wrap("aricode", "print", |mut caller: Ctx, ptr: i32, len: i32| {
                charge(&mut caller, PRINT_COST + len as u32 as u64)?;
                let mem = memory(&caller)?;
                let data = mem.data(&caller);
                sys::print(&data[range(data, ptr, len, "print out of memory")?]);
                Ok(())
            })?
        }
        Api::UptimeMs => linker.func_wrap("aricode", "uptime_ms", |mut caller: Ctx| {
            charge(&mut caller, CLOCK_COST)?;
            let ms = caller
                .data()
                .system
                .and_then(|s| time::uptime_ms(s).ok())
                .unwrap_or(0);
            Ok(ms as i64)
        })?,
        // `file_write(name, name_len, data, data_len) -> bytes or -1`:
        // creates or replaces the whole file (atomically) in the private directory.
        Api::FileWrite => linker.func_wrap(
            "aricode",
            "file_write",
            |mut caller: Ctx, name: i32, name_len: i32, ptr: i32, len: i32| {
                charge(&mut caller, FILE_WRITE_COST + bytes_cost(len))?;
                let mem = memory(&caller)?;
                let (data, state) = mem.data_and_store_mut(&mut caller);
                let (name, n) = name_from(data, name, name_len)?;
                let name = core::str::from_utf8(&name[..n]).map_err(|_| trap("file name"))?;
                if !writable(name) {
                    // Its own code or manifest (audit of ADR 0071, L4).
                    return Ok(-1);
                }
                let bytes = &data[range(data, ptr, len, "data out of memory")?];
                let Ok(fd) = state.fs.open(name).or_else(|_| state.fs.create(name)) else {
                    return Ok(-1);
                };
                let r = state.fs.replace(fd, bytes);
                let _ = state.fs.close(fd);
                Ok(r.map_or(-1, |n| n as i32))
            },
        )?,
        // `file_read(name, name_len, buf, buf_len) -> bytes or -1`.
        Api::FileRead => linker.func_wrap(
            "aricode",
            "file_read",
            |mut caller: Ctx, name: i32, name_len: i32, ptr: i32, len: i32| {
                charge(&mut caller, FILE_READ_COST + bytes_cost(len))?;
                let mem = memory(&caller)?;
                let (data, state) = mem.data_and_store_mut(&mut caller);
                let (name, n) = name_from(data, name, name_len)?;
                let name = core::str::from_utf8(&name[..n]).map_err(|_| trap("file name"))?;
                let out = range(data, ptr, len, "buffer out of memory")?;
                let Ok(fd) = state.fs.open(name) else {
                    return Ok(-1);
                };
                let out = &mut data[out];
                let mut done = 0;
                while done < out.len() {
                    match state.fs.read(fd, done as u32, &mut out[done..]) {
                        Ok(0) => break,
                        Ok(n) => done += n,
                        Err(_) => {
                            let _ = state.fs.close(fd);
                            return Ok(-1);
                        }
                    }
                }
                let _ = state.fs.close(fd);
                Ok(done as i32)
            },
        )?,
    };
    Ok(())
}

/// Why a call ended without a result.
fn describe(e: &Error) -> alloc::string::String {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => "too many steps".into(),
        Some(TrapCode::StackOverflow) => "stack exhausted".into(),
        Some(code) => format!("trap ({code})"),
        None => format!("trap ({e})"),
    }
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

/// Adds `HEAP_CHUNKS` DMA buffers to the heap; how many it got.
fn extend_heap(hw: Handle) -> usize {
    let mut added = 0;
    for _ in 0..HEAP_CHUNKS {
        let Ok(dma) = sys::dma_alloc(hw, HEAP_CHUNK) else {
            break;
        };
        let Ok(virt) = sys::mmio_map(dma) else {
            break;
        };
        // SAFETY: our own DMA buffer of HEAP_CHUNK bytes, mapped writable,
        // page-aligned and never unmapped: the heap owns it from here.
        unsafe { aricode_rt::extend_heap(virt as usize, HEAP_CHUNK as usize) };
        added += 1;
    }
    added
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
    let bytes = match read_file(&mut fs, MODULE_FILE, MAX_MODULE) {
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
    let caps = match read_file(&mut fs, MANIFEST_FILE, 1024) {
        Ok(text) => match core::str::from_utf8(&text)
            .map_err(|_| "not text")
            .and_then(parse_caps)
        {
            Ok(caps) => caps,
            Err(why) => {
                sys::print(b"wasm_runner: invalid module manifest: ");
                sys::print(why.as_bytes());
                sys::print(b"\n");
                return 11;
            }
        },
        Err(_) => {
            say(b"module has no manifest (demo.manifest): not running");
            return 11;
        }
    };
    sys::print(b"wasm_runner: module manifest:");
    for (i, (name, _)) in Cap::NAMES.iter().enumerate() {
        if caps[i] {
            sys::print(b" ");
            sys::print(name.as_bytes());
        }
    }
    sys::print(b"\n");
    // 2. The heap for wasmi and the module's memories, from the quota.
    if extend_heap(hw) < HEAP_CHUNKS {
        say(b"no memory for the module");
        return 8;
    }
    // 3. Parse and validate; resolve imports by name and capability.
    if let Err(why) = check_nesting(&bytes) {
        sys::print(format!("wasm_runner: invalid module ({why})\n").as_bytes());
        return 6;
    }
    let engine = engine();
    let module = match Module::new(&engine, &bytes[..]) {
        Ok(m) => m,
        Err(e) => {
            sys::print(format!("wasm_runner: invalid module ({e})\n").as_bytes());
            return 6;
        }
    };
    drop(bytes);
    let mut linker = Linker::<HostState>::new(&engine);
    for imp in module.imports() {
        let api = if imp.module() == "aricode" {
            Api::by_name(imp.name())
        } else {
            None
        };
        let func = match imp.ty() {
            ExternType::Func(f) => Some(f),
            _ => None,
        };
        let (Some(api), Some(func)) = (api, func) else {
            sys::print(
                format!(
                    "wasm_runner: unknown import {}.{}\n",
                    imp.module(),
                    imp.name()
                )
                .as_bytes(),
            );
            return 7;
        };
        if !granted(&caps, api.requires()) {
            sys::print(
                format!(
                    "wasm_runner: import aricode.{} not granted by the module manifest\n",
                    imp.name()
                )
                .as_bytes(),
            );
            return 12;
        }
        if !api.matches(func) {
            sys::print(
                format!(
                    "wasm_runner: import aricode.{} has an incompatible signature\n",
                    imp.name()
                )
                .as_bytes(),
            );
            return 14;
        }
        if define(&mut linker, api).is_err() {
            // Imported twice: the first definition serves both.
        }
    }
    // `main(i64) -> i32`: if the module exports anything else, it is not run.
    let main_ok = module
        .exports()
        .any(|e| e.name() == "main" && matches!(e.ty(), ExternType::Func(f) if is_main(f)));
    if !main_ok {
        say(b"main has an incompatible signature (expected main(i64) -> i32)");
        return 15;
    }
    // 4. Instantiation (data, elements, `start`) and execution.
    let mut store = Store::new(
        &engine,
        HostState {
            system,
            fs,
            limits: limits(),
        },
    );
    store.limiter(|s| &mut s.limits);
    if store.set_fuel(FUEL).is_err() {
        return 9;
    }
    let instance = match linker.instantiate_and_start(&mut store, &module) {
        Ok(i) => i,
        Err(e) if e.as_trap_code().is_some() => {
            sys::print(
                format!("wasm_runner: start function failed: {}\n", describe(&e)).as_bytes(),
            );
            return 10;
        }
        Err(e) => {
            sys::print(format!("wasm_runner: cannot instantiate the module ({e})\n").as_bytes());
            return 9;
        }
    };
    let Ok(main) = instance.get_typed_func::<i64, i32>(&store, "main") else {
        return 15;
    };
    if store.set_fuel(FUEL).is_err() {
        return 9;
    }
    match main.call(&mut store, arg as i64) {
        Ok(code) => {
            sys::print(b"wasm_runner: main returned ");
            sys::print(fmt_u64(code as u32 as u64, &mut buf));
            sys::print(b"\n");
            code as u32 as u64
        }
        Err(e) => {
            sys::print(format!("wasm_runner: module failed: {}\n", describe(&e)).as_bytes());
            13
        }
    }
}

aricode_rt::entry!(main);
