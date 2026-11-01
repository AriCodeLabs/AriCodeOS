// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The WebAssembly runner's policy over wasmi (ADR 0071), kept apart from
//! its system calls so it is tested on the host:
//! - the engine: profile v2 (`docs/wasm-profile.md`), fuel on;
//! - the limits on what a module may hold (`StoreLimits`);
//! - the fuel an import costs, charged before it runs (ADR 0055);
//! - the module manifest and the Capability API's names and signatures.
#![no_std]

extern crate alloc;

use wasmi::{
    Caller, CompilationMode, Config, Engine, Error, FuncType, StoreLimits, StoreLimitsBuilder,
    TrapCode, ValType,
};

/// Largest module the runner loads.
pub const MAX_MODULE: usize = 64 * 1024;
/// What a module may hold, so the heap is never exhausted by it: memories
/// of at most 1 MiB, four of them, four tables of 16 Ki elements, one
/// instance, a value stack of 64 Ki slots (512 KiB) and 1024 nested calls.
pub const MEMORY_LIMIT: usize = 1024 * 1024;
pub const MEMORIES: usize = 4;
pub const TABLES: usize = 4;
pub const TABLE_ELEMENTS: usize = 16 * 1024;
pub const STACK_SLOTS: usize = 64 * 1024;
pub const CALL_DEPTH: usize = 1024;
/// Fuel for the `start` function and for `main`, each.
pub const FUEL: u64 = 50_000_000;
/// Bytes per unit of fuel that bulk operations and imports move (wasmi's
/// default, the 64 of ADR 0055).
pub const WORK_BYTES: u64 = 64;
/// Maximum length of file names a module can pass to `file_*`.
pub const MAX_FILE_NAME: usize = 32;
/// Fuel an import costs on top of the bytes it moves, so a module's fuel
/// also bounds the time it makes the system work for it (incident 0042,
/// audit M-a). A `file_write` is a durable AriFS replace (journal and data
/// written to disk): at most 50 per call of 50 M units.
pub const FILE_WRITE_COST: u64 = 1_000_000;
/// A `file_read`: open, IPC reads and close with the filesystem service.
pub const FILE_READ_COST: u64 = 10_000;
/// A `print`: a system call to the console, plus one unit per byte (the
/// console is far slower than memory).
pub const PRINT_COST: u64 = 1_000;
/// An `uptime_ms`: one system call.
pub const CLOCK_COST: u64 = 100;

/// The engine: every stable WebAssembly feature except SIMD (profile v2),
/// fuel on, lazy translation with eager validation, bounded stacks.
pub fn engine() -> Engine {
    let mut cfg = Config::default();
    cfg.consume_fuel(true)
        .compilation_mode(CompilationMode::LazyTranslation)
        .set_max_recursion_depth(CALL_DEPTH)
        .set_max_stack_height(STACK_SLOTS)
        .floats(true)
        .wasm_mutable_global(true)
        .wasm_sign_extension(true)
        .wasm_saturating_float_to_int(true)
        .wasm_multi_value(true)
        .wasm_multi_memory(true)
        .wasm_bulk_memory(true)
        .wasm_reference_types(true)
        .wasm_tail_call(true)
        .wasm_extended_const(true)
        .wasm_memory64(true)
        .wasm_custom_page_sizes(false)
        .wasm_wide_arithmetic(false);
    Engine::new(&cfg)
}

/// The limits wasmi enforces on one module's memories, tables and
/// instances. A grow past them returns -1, as a failed grow does.
pub fn limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(MEMORY_LIMIT)
        .memories(MEMORIES)
        .tables(TABLES)
        .table_elements(TABLE_ELEMENTS)
        .instances(1)
        .trap_on_grow_failure(false)
        .build()
}

/// Charges an import's price before it runs; one the module cannot pay
/// for never runs, and the call ends out of fuel.
pub fn charge<T>(caller: &mut Caller<'_, T>, cost: u64) -> Result<(), Error> {
    let fuel = caller.get_fuel()?;
    if fuel < cost {
        caller.set_fuel(0)?;
        return Err(TrapCode::OutOfFuel.into());
    }
    caller.set_fuel(fuel - cost)
}

/// Fuel for `len` bytes an import moves.
pub fn bytes_cost(len: i32) -> u64 {
    len as u32 as u64 / WORK_BYTES
}

/// Capabilities a module can request in its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Print,
    Clock,
    Files,
}

impl Cap {
    pub const NAMES: [(&'static str, Cap); 3] = [
        ("print", Cap::Print),
        ("clock", Cap::Clock),
        ("files", Cap::Files),
    ];
}

/// Capability API seen by the module, and the capability each function requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    Print,
    UptimeMs,
    FileRead,
    FileWrite,
}

impl Api {
    pub fn by_name(name: &str) -> Option<Api> {
        Some(match name {
            "print" => Api::Print,
            "uptime_ms" => Api::UptimeMs,
            "file_read" => Api::FileRead,
            "file_write" => Api::FileWrite,
            _ => return None,
        })
    }

    pub fn requires(self) -> Cap {
        match self {
            Api::Print => Cap::Print,
            Api::UptimeMs => Cap::Clock,
            Api::FileRead | Api::FileWrite => Cap::Files,
        }
    }

    /// Signature the host implements; the module import must declare exactly
    /// this one (audit 6ee19fb, finding 5).
    pub fn signature(self) -> (&'static [ValType], &'static [ValType]) {
        use ValType::{I32, I64};
        match self {
            Api::Print => (&[I32, I32], &[]),
            Api::UptimeMs => (&[], &[I64]),
            Api::FileRead | Api::FileWrite => (&[I32, I32, I32, I32], &[I32]),
        }
    }

    /// Whether `ty` is this function's signature.
    pub fn matches(self, ty: &FuncType) -> bool {
        let (params, results) = self.signature();
        ty.params() == params && ty.results() == results
    }
}

/// `main(i64) -> i32`, the only entry point the runner calls.
pub fn is_main(ty: &FuncType) -> bool {
    ty.params() == [ValType::I64] && ty.results() == [ValType::I32]
}

/// Module manifest: `caps = a, b` (comments with `#`); the capabilities
/// granted, in `Cap::NAMES` order.
pub fn parse_caps(text: &str) -> Result<[bool; 3], &'static str> {
    let mut out = [false; 3];
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
    Ok(out)
}

/// Whether `granted` (from `parse_caps`) covers `cap`.
pub fn granted(granted: &[bool; 3], cap: Cap) -> bool {
    Cap::NAMES
        .iter()
        .position(|(_, c)| *c == cap)
        .is_some_and(|i| granted[i])
}

/// A file name for `file_*`: 1 to `MAX_FILE_NAME` UTF-8 bytes without `/`.
pub fn file_name(bytes: &[u8]) -> Option<&str> {
    let name = core::str::from_utf8(bytes).ok()?;
    (!name.is_empty() && name.len() <= MAX_FILE_NAME && !name.contains('/')).then_some(name)
}

#[cfg(test)]
mod tests;
