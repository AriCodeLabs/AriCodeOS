// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! WebAssembly interpreter (Phase 29): the integer MVP subset that `rustc`
//! (`-C target-cpu=mvp`) and other compilers produce for `no_std` modules:
//! structured control, locals and globals, linear memory with checked
//! bounds, direct and indirect calls and function imports. No floating
//! point (traps with `Unsupported`), no SIMD.
//!
//! The module is parsed and validated once ([`Module::parse`]), which also
//! records where every block ends and what every `br_table` targets;
//! execution ([`Instance::call`]) walks the code byte by byte with a
//! bounded value stack and label stack. Every memory operation checks
//! bounds: a hostile module can only fail, never escape its memory.
//! Linear memory is supplied by the host as a `&mut [u8]` (in the
//! system, a DMA buffer bounded by the application's quota).
//!
//! Fuel counts work, not instructions: every instruction costs one unit
//! and an instruction whose work grows with an operand (bulk memory, values
//! a branch or a return keeps, locals zeroed by a call) costs one more unit
//! per [`WORK_BYTES`] bytes it touches, so run time stays proportional to
//! the fuel spent (incident 0042). Allocation failures are errors, never
//! panics: the host program's heap is small (`wasm_runner`: 256 KiB).

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod validate;

use alloc::vec::Vec;
use validate::validate;

pub const PAGE: usize = 65536;
/// Maximum depth of nested calls.
pub const MAX_CALL_DEPTH: usize = 64;
/// Values on the operand stack, in total.
pub const MAX_STACK: usize = 16 * 1024;
/// Locals (parameters included) of all active calls, in total.
pub const MAX_LOCALS: usize = 16 * 1024;
/// Parameters, and results, of a function type (the limits of the
/// WebAssembly JavaScript API): bounds the work of validating and running
/// a block, a call or a branch.
pub const MAX_ARITY: usize = 1000;
/// Fuel per `call` invocation (against infinite loops), in units of work:
/// one per instruction plus one per [`WORK_BYTES`] bytes of variable work.
pub const MAX_STEPS: u64 = 50_000_000;
/// Bytes moved, zeroed or compared that cost one unit of fuel.
pub const WORK_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Malformed module or one with unsupported sections.
    Malformed(&'static str),
    /// Function, type or index out of range.
    OutOfRange,
    Unsupported(&'static str),
    /// Runtime trap: `unreachable`, division by zero, out-of-bounds memory access...
    Trap(&'static str),
    /// Call depth, operand stack, locals or labels exhausted (also when the
    /// heap cannot hold them).
    StackOverflow,
    /// Fuel exhausted ([`MAX_STEPS`]).
    Fuel,
    /// An import has no host.
    MissingImport,
    /// The module does not validate (typing, indices, stacks): rejected before
    /// instantiating it (ADR 0019).
    Invalid(&'static str),
    /// Export not found.
    NoExport,
    /// The heap cannot hold the parsed module or its instance.
    OutOfMemory,
}

/// `v.push(x)`, or `full` if the allocator cannot make room.
fn try_push<T>(v: &mut Vec<T>, x: T, full: Error) -> Result<(), Error> {
    if v.len() == v.capacity() {
        v.try_reserve(1).map_err(|_| full)?;
    }
    v.push(x);
    Ok(())
}

/// A copy of `bytes` on the heap, or `OutOfMemory`.
fn try_to_vec(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(bytes.len())
        .map_err(|_| Error::OutOfMemory)?;
    v.extend_from_slice(bytes);
    Ok(v)
}

/// Fuel for `bytes` bytes of variable work.
fn bytes_cost(bytes: usize) -> u64 {
    (bytes / WORK_BYTES) as u64
}

/// Fuel for moving or zeroing `n` values.
fn values_cost(n: usize) -> u64 {
    bytes_cost(n.saturating_mul(core::mem::size_of::<Value>()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// Value on the stack: types are checked by module construction; here
/// everything is a `u64` (zero-extended i32, i64, f32/f64 bits).
pub type Value = u64;

#[derive(Debug, Clone)]
pub struct Import {
    pub module: Vec<u8>,
    pub name: Vec<u8>,
    pub type_index: u32,
}

#[derive(Debug, Clone, Copy)]
struct Export {
    name_start: usize,
    name_len: usize,
    kind: u8,
    index: u32,
}

#[derive(Debug, Clone, Copy)]
struct Function {
    /// Index of the function in the function space (imported ones first).
    index: u32,
    type_index: u32,
    /// Declared locals (without parameters).
    locals: u32,
    /// Position of the locals declaration (types) inside the module.
    locals_decl: usize,
    /// Range of the body (instructions) inside the module.
    code_start: usize,
    code_end: usize,
}

#[derive(Debug, Clone, Copy)]
struct Global {
    ty: ValType,
    mutable: bool,
    init: Value,
    /// Type of the initializer constant (must match `ty`).
    init_ty: ValType,
}

#[derive(Debug, Clone, Copy)]
struct DataSegment {
    offset: u32,
    start: usize,
    len: usize,
}

/// Where a `block` or an `if` ends: positions in the module, after its
/// type, after its `else` (0: none) and after its `end`. 12 bytes per
/// block (audit 2026-10-25 of incident 0042, M1).
#[derive(Debug, Clone, Copy)]
struct BlockEnds {
    start: u32,
    else_pos: u32,
    end: u32,
}

/// A `br_table` (position after its opcode): its `n` label depths and
/// then the default are `br_depths[first..=first + n]`.
#[derive(Debug, Clone, Copy)]
struct BrTable {
    pos: u32,
    first: u32,
    n: u32,
}

/// What the validator records so execution never decodes code it does
/// not run: shared by every instance of the module. Entries are in code
/// order, found by binary search.
#[derive(Default)]
struct SideTables {
    blocks: Vec<BlockEnds>,
    br_tables: Vec<BrTable>,
    /// Label depths, below `MAX_NESTING + 1`.
    br_depths: Vec<u16>,
}

/// Parsed and validated module; keeps a reference to the original bytes.
/// Read-only once parsed: instances rely on what validation checked.
pub struct Module<'a> {
    bytes: &'a [u8],
    types: Vec<FuncType>,
    imports: Vec<Import>,
    functions: Vec<Function>,
    table: Vec<Option<u32>>,
    /// Declared table limits (minimum, maximum), if there is a table.
    table_limits: Option<(u32, Option<u32>)>,
    memory_pages: Option<(u32, Option<u32>)>,
    globals: Vec<Global>,
    exports: Vec<Export>,
    data: Vec<DataSegment>,
    start: Option<u32>,
    side: SideTables,
}

/// Bytes of the module read (tests: execution reads the instructions it
/// runs, not the code around them).
#[cfg(test)]
mod meter {
    std::thread_local! {
        pub static CODE_READS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8], pos: usize) -> Self {
        Self { b, pos }
    }
    fn u8(&mut self) -> Result<u8, Error> {
        #[cfg(test)]
        meter::CODE_READS.with(|c| c.set(c.get() + 1));
        let v = *self
            .b
            .get(self.pos)
            .ok_or(Error::Malformed("unexpected end"))?;
        self.pos += 1;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 32 {
                return Err(Error::Malformed("leb128 u32"));
            }
            result |= ((byte & 0x7F) as u32) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }
    fn i32(&mut self) -> Result<i32, Error> {
        let mut result = 0i64;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 35 {
                return Err(Error::Malformed("leb128 i32"));
            }
            result |= ((byte & 0x7F) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result as i32);
            }
        }
    }
    fn i64(&mut self) -> Result<i64, Error> {
        let mut result = 0i64;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            if shift >= 70 {
                return Err(Error::Malformed("leb128 i64"));
            }
            if shift < 64 {
                result |= ((byte & 0x7F) as i64) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        #[cfg(test)]
        meter::CODE_READS.with(|c| c.set(c.get() + n as u64));
        let s = self
            .b
            .get(self.pos..self.pos.checked_add(n).ok_or(Error::Malformed("length"))?)
            .ok_or(Error::Malformed("unexpected end"))?;
        self.pos += n;
        Ok(s)
    }
    fn valtype(&mut self) -> Result<ValType, Error> {
        Ok(match self.u8()? {
            0x7F => ValType::I32,
            0x7E => ValType::I64,
            0x7D => ValType::F32,
            0x7C => ValType::F64,
            _ => return Err(Error::Malformed("value type")),
        })
    }
}

/// Evaluates a constant initialization expression (`i32.const`, `i64.const`).
/// Constant expression with the type of its constant (to validate global
/// initializers and segment offsets).
fn const_expr_typed(r: &mut Reader) -> Result<(ValType, Value), Error> {
    let v = match r.u8()? {
        0x41 => (ValType::I32, r.i32()? as u32 as u64),
        0x42 => (ValType::I64, r.i64()? as u64),
        0x43 => {
            let b = r.bytes(4)?;
            (
                ValType::F32,
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64,
            )
        }
        0x44 => {
            let b = r.bytes(8)?;
            (
                ValType::F64,
                u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            )
        }
        _ => return Err(Error::Unsupported("constant expression")),
    };
    if r.u8()? != 0x0B {
        return Err(Error::Malformed("end of constant expression"));
    }
    Ok(v)
}

impl<'a> Module<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[4..8] != [1, 0, 0, 0] {
            return Err(Error::Malformed("header"));
        }
        // Side tables keep positions as u32.
        if u32::try_from(bytes.len()).is_err() {
            return Err(Error::Unsupported("module larger than 4 GiB"));
        }
        let oom = Error::OutOfMemory;
        let mut m = Module {
            bytes,
            types: Vec::new(),
            imports: Vec::new(),
            functions: Vec::new(),
            table: Vec::new(),
            table_limits: None,
            memory_pages: None,
            globals: Vec::new(),
            exports: Vec::new(),
            data: Vec::new(),
            start: None,
            side: SideTables::default(),
        };
        let mut func_types: Vec<u32> = Vec::new();
        let mut r = Reader::new(bytes, 8);
        while r.pos < bytes.len() {
            let id = r.u8()?;
            let size = r.u32()? as usize;
            let start = r.pos;
            let end = start.checked_add(size).ok_or(Error::Malformed("section"))?;
            if end > bytes.len() {
                return Err(Error::Malformed("truncated section"));
            }
            let mut s = Reader::new(bytes, start);
            match id {
                0 => {} // custom
                1 => {
                    for _ in 0..s.u32()? {
                        if s.u8()? != 0x60 {
                            return Err(Error::Malformed("function type"));
                        }
                        let params = valtypes(&mut s)?;
                        let results = valtypes(&mut s)?;
                        try_push(&mut m.types, FuncType { params, results }, oom)?;
                    }
                }
                2 => {
                    for _ in 0..s.u32()? {
                        let ml = s.u32()? as usize;
                        let module = try_to_vec(s.bytes(ml)?)?;
                        let nl = s.u32()? as usize;
                        let name = try_to_vec(s.bytes(nl)?)?;
                        match s.u8()? {
                            0 => {
                                let type_index = s.u32()?;
                                let import = Import {
                                    module,
                                    name,
                                    type_index,
                                };
                                try_push(&mut m.imports, import, oom)?;
                            }
                            _ => return Err(Error::Unsupported("non-function import")),
                        }
                    }
                }
                3 => {
                    for _ in 0..s.u32()? {
                        try_push(&mut func_types, s.u32()?, oom)?;
                    }
                }
                4 => {
                    for _ in 0..s.u32()? {
                        if s.u8()? != 0x70 {
                            return Err(Error::Malformed("table type"));
                        }
                        let flags = s.u8()?;
                        let min = s.u32()?;
                        let max = if flags & 1 != 0 { Some(s.u32()?) } else { None };
                        if min > 4096 {
                            return Err(Error::Unsupported("large table"));
                        }
                        m.table.clear();
                        m.table.try_reserve_exact(min as usize).map_err(|_| oom)?;
                        m.table.resize(min as usize, None);
                        m.table_limits = Some((min, max));
                    }
                }
                5 => {
                    for _ in 0..s.u32()? {
                        let flags = s.u8()?;
                        let min = s.u32()?;
                        let max = if flags & 1 != 0 { Some(s.u32()?) } else { None };
                        m.memory_pages = Some((min, max));
                    }
                }
                6 => {
                    for _ in 0..s.u32()? {
                        let ty = s.valtype()?;
                        let mutable = s.u8()? == 1;
                        let (init_ty, init) = const_expr_typed(&mut s)?;
                        let global = Global {
                            ty,
                            mutable,
                            init,
                            init_ty,
                        };
                        try_push(&mut m.globals, global, oom)?;
                    }
                }
                7 => {
                    for _ in 0..s.u32()? {
                        let name_len = s.u32()? as usize;
                        let name_start = s.pos;
                        s.bytes(name_len)?;
                        let kind = s.u8()?;
                        let index = s.u32()?;
                        let export = Export {
                            name_start,
                            name_len,
                            kind,
                            index,
                        };
                        try_push(&mut m.exports, export, oom)?;
                    }
                }
                8 => m.start = Some(s.u32()?),
                9 => {
                    for _ in 0..s.u32()? {
                        let flags = s.u32()?;
                        if flags != 0 {
                            return Err(Error::Unsupported("element segment"));
                        }
                        let (offset_ty, offset) = const_expr_typed(&mut s)?;
                        if offset_ty != ValType::I32 {
                            return Err(Error::Invalid("non-i32 element offset"));
                        }
                        let offset = offset as usize;
                        let n = s.u32()? as usize;
                        for i in 0..n {
                            let f = s.u32()?;
                            let slot = m
                                .table
                                .get_mut(offset + i)
                                .ok_or(Error::Malformed("element outside the table"))?;
                            *slot = Some(f);
                        }
                    }
                }
                10 => {
                    let count = s.u32()? as usize;
                    if count != func_types.len() {
                        return Err(Error::Malformed("bodies != functions"));
                    }
                    for (i, type_index) in func_types.iter().copied().enumerate() {
                        let body_size = s.u32()? as usize;
                        let body_start = s.pos;
                        let index = m.imports.len() as u32 + i as u32;
                        let body_end = body_start
                            .checked_add(body_size)
                            .ok_or(Error::Malformed("body"))?;
                        if body_end > end {
                            return Err(Error::Malformed("truncated body"));
                        }
                        let mut locals = 0u32;
                        for _ in 0..s.u32()? {
                            let n = s.u32()?;
                            s.valtype()?;
                            locals = locals.checked_add(n).ok_or(Error::Malformed("locals"))?;
                        }
                        if locals > 4096 {
                            return Err(Error::Unsupported("too many locals"));
                        }
                        let function = Function {
                            index,
                            type_index,
                            locals,
                            locals_decl: body_start,
                            code_start: s.pos,
                            code_end: body_end,
                        };
                        try_push(&mut m.functions, function, oom)?;
                        s.pos = body_end;
                    }
                }
                11 => {
                    for _ in 0..s.u32()? {
                        let flags = s.u32()?;
                        if flags != 0 {
                            return Err(Error::Unsupported("passive data segment"));
                        }
                        let (offset_ty, offset) = const_expr_typed(&mut s)?;
                        if offset_ty != ValType::I32 {
                            return Err(Error::Invalid("non-i32 data offset"));
                        }
                        let offset = offset as u32;
                        let len = s.u32()? as usize;
                        let start = s.pos;
                        s.bytes(len)?;
                        try_push(&mut m.data, DataSegment { offset, start, len }, oom)?;
                    }
                }
                12 => {} // data count
                _ => return Err(Error::Malformed("unknown section")),
            }
            r.pos = end;
        }
        if func_types.len() != m.functions.len() {
            return Err(Error::Malformed("missing bodies"));
        }
        m.side = validate(&m)?;
        Ok(m)
    }

    /// Function types, by index.
    pub fn types(&self) -> &[FuncType] {
        &self.types
    }

    /// Imported functions, in function-index order.
    pub fn imports(&self) -> &[Import] {
        &self.imports
    }

    fn export_name(&self, e: &Export) -> &'a [u8] {
        &self.bytes[e.name_start..e.name_start + e.name_len]
    }

    /// Index of the exported function with that name.
    pub fn exported_function(&self, name: &str) -> Option<u32> {
        self.exports
            .iter()
            .find(|e| e.kind == 0 && self.export_name(e) == name.as_bytes())
            .map(|e| e.index)
    }

    /// Type of function `index` (imported ones first).
    pub fn function_type(&self, index: u32) -> Result<&FuncType, Error> {
        let ti = if (index as usize) < self.imports.len() {
            self.imports[index as usize].type_index
        } else {
            self.functions
                .get(index as usize - self.imports.len())
                .ok_or(Error::OutOfRange)?
                .type_index
        };
        self.types.get(ti as usize).ok_or(Error::OutOfRange)
    }

    /// Declared initial memory pages.
    pub fn initial_pages(&self) -> u32 {
        self.memory_pages.map(|(min, _)| min).unwrap_or(0)
    }

    /// The `block` or `if` whose code starts at `start` (after its type).
    fn block(&self, start: usize) -> Result<BlockEnds, Error> {
        let blocks = &self.side.blocks;
        blocks
            .binary_search_by_key(&(start as u32), |b| b.start)
            .map(|i| blocks[i])
            .map_err(|_| Error::Malformed("block without end"))
    }

    /// Label depth chosen by `index` in the `br_table` at `pos` (after its
    /// opcode): the default when `index` is past its labels.
    fn br_depth(&self, pos: usize, index: u32) -> Result<u32, Error> {
        let tables = &self.side.br_tables;
        let t = tables
            .binary_search_by_key(&(pos as u32), |t| t.pos)
            .map(|i| tables[i])
            .map_err(|_| Error::Malformed("br_table"))?;
        let at = t.first as usize + index.min(t.n) as usize;
        self.side
            .br_depths
            .get(at)
            .map(|&d| d as u32)
            .ok_or(Error::Malformed("br_table"))
    }
}

/// A count and that many value types (params or results of a function
/// type), at most [`MAX_ARITY`].
fn valtypes(r: &mut Reader) -> Result<Vec<ValType>, Error> {
    let n = r.u32()? as usize;
    if n > MAX_ARITY {
        return Err(Error::Unsupported("too many params or results"));
    }
    let mut types = Vec::new();
    types.try_reserve_exact(n).map_err(|_| Error::OutOfMemory)?;
    for _ in 0..n {
        types.push(r.valtype()?);
    }
    Ok(types)
}

/// Functions the host offers the module. `call(import index,
/// arguments, memory)` returns the result (or 0 if there is none).
pub trait Host {
    fn call(&mut self, import: usize, args: &[Value], memory: &mut [u8]) -> Result<Value, Error>;
}

struct Label {
    /// Where a `br` goes: after the `end`, or the start of a `loop`; the
    /// `else` of an `if` also jumps there.
    target: usize,
    is_loop: bool,
    /// Stack height on entry (without the block parameters).
    height: usize,
    /// Values the block leaves when exiting through `br`.
    arity: usize,
    /// Block parameters: what a `br` to a `loop` keeps
    /// (audit 6ee19fb, observation).
    params: usize,
}

struct Frame {
    func: usize,
    pc: usize,
    locals_base: usize,
    labels_base: usize,
    stack_base: usize,
    results: usize,
}

/// Instance: module plus mutable state (globals, host memory).
pub struct Instance<'m, 'a> {
    module: &'m Module<'a>,
    globals: Vec<Value>,
    stack: Vec<Value>,
    labels: Vec<Label>,
    memory_pages: u32,
    /// Fuel spent by the current call ([`MAX_STEPS`]).
    steps: u64,
    /// Fuel per call: `MAX_STEPS` (tests lower it).
    fuel: u64,
}

impl<'m, 'a> Instance<'m, 'a> {
    /// Creates the instance and copies the data segments into `memory`.
    pub fn new(module: &'m Module<'a>, memory: &mut [u8]) -> Result<Self, Error> {
        let pages = module.initial_pages();
        let active = (pages as usize) * PAGE;
        if active > memory.len() {
            return Err(Error::Trap("not enough memory for the module"));
        }
        // Linear memory starts zeroed even if the host buffer carries
        // leftovers; data segments must fall inside the initial pages, not
        // inside the whole buffer.
        memory[..active].fill(0);
        for d in &module.data {
            let end = (d.offset as usize)
                .checked_add(d.len)
                .ok_or(Error::Trap("data segment out of memory"))?;
            let dst = memory[..active]
                .get_mut(d.offset as usize..end)
                .ok_or(Error::Trap("data segment out of memory"))?;
            dst.copy_from_slice(&module.bytes[d.start..d.start + d.len]);
        }
        let mut globals = Vec::new();
        globals
            .try_reserve_exact(module.globals.len())
            .map_err(|_| Error::OutOfMemory)?;
        globals.extend(module.globals.iter().map(|g| g.init));
        Ok(Self {
            module,
            globals,
            stack: Vec::new(),
            labels: Vec::new(),
            memory_pages: pages,
            steps: 0,
            fuel: MAX_STEPS,
        })
    }

    pub fn global(&self, index: usize) -> Option<Value> {
        self.globals.get(index).copied()
    }

    /// Bytes of linear memory currently valid (`memory_pages * PAGE`).
    pub fn memory_len(&self) -> usize {
        self.memory_pages as usize * PAGE
    }

    /// Runs the module's `start` function if it declares one. Must be called
    /// once, after `new` and before any export; propagates its traps.
    pub fn start(&mut self, memory: &mut [u8], host: &mut dyn Host) -> Result<(), Error> {
        match self.module.start {
            Some(index) => self.invoke(index, &[], memory, host).map(|_| ()),
            None => Ok(()),
        }
    }

    /// Calls the exported function `name` with `args`; returns its first
    /// result (0 if it has none).
    pub fn call(
        &mut self,
        name: &str,
        args: &[Value],
        memory: &mut [u8],
        host: &mut dyn Host,
    ) -> Result<Value, Error> {
        let index = self.module.exported_function(name).ok_or(Error::NoExport)?;
        self.invoke(index, args, memory, host)
    }

    pub fn invoke(
        &mut self,
        index: u32,
        args: &[Value],
        memory: &mut [u8],
        host: &mut dyn Host,
    ) -> Result<Value, Error> {
        self.stack.clear();
        self.labels.clear();
        self.steps = 0;
        let ty = self.module.function_type(index)?;
        if ty.params.len() != args.len() {
            return Err(Error::OutOfRange);
        }
        let results = ty.results.len();
        self.stack
            .try_reserve(args.len())
            .map_err(|_| Error::StackOverflow)?;
        self.stack.extend_from_slice(args);
        self.run(index, memory, host)?;
        Ok(if results > 0 {
            self.stack.pop().unwrap_or(0)
        } else {
            0
        })
    }

    fn push(&mut self, v: Value) -> Result<(), Error> {
        if self.stack.len() >= MAX_STACK {
            return Err(Error::StackOverflow);
        }
        try_push(&mut self.stack, v, Error::StackOverflow)
    }

    fn push_label(&mut self, l: Label) -> Result<(), Error> {
        try_push(&mut self.labels, l, Error::StackOverflow)
    }

    /// Spends `units` of fuel: every operation charges in proportion to its
    /// work before doing it.
    fn charge(&mut self, units: u64) -> Result<(), Error> {
        self.steps = self.steps.saturating_add(units);
        if self.steps > self.fuel {
            return Err(Error::Fuel);
        }
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, Error> {
        self.stack.pop().ok_or(Error::Trap("empty stack"))
    }

    fn pop32(&mut self) -> Result<u32, Error> {
        self.pop().map(|v| v as u32)
    }

    fn branch(&mut self, depth: u32, frame_labels_base: usize) -> Result<Option<usize>, Error> {
        let idx = self
            .labels
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(Error::Malformed("label"))?;
        if idx < frame_labels_base {
            return Err(Error::Malformed("label of another frame"));
        }
        let (target, height, arity, is_loop, params) = {
            let l = &self.labels[idx];
            (l.target, l.height, l.arity, l.is_loop, l.params)
        };
        // Keep the block's output values (or the parameters of the
        // `loop`, which re-enters) and discard the rest.
        let keep = if is_loop { params } else { arity };
        self.charge(values_cost(keep))?;
        let len = self.stack.len();
        if len < height + keep {
            return Err(Error::Trap("stack too short in br"));
        }
        let from = len - keep;
        self.stack.copy_within(from..len, height);
        self.stack.truncate(height + keep);
        if is_loop {
            self.labels.truncate(idx + 1);
        } else {
            self.labels.truncate(idx);
        }
        Ok(Some(target))
    }

    fn run(&mut self, entry: u32, memory: &mut [u8], host: &mut dyn Host) -> Result<(), Error> {
        let module = self.module;
        let n_imports = module.imports.len();
        let mut frames: Vec<Frame> = Vec::new();
        // Reserved whole: a call never allocates a frame.
        frames
            .try_reserve_exact(MAX_CALL_DEPTH)
            .map_err(|_| Error::StackOverflow)?;
        let mut locals: Vec<Value> = Vec::new();

        // Entering a function: parameters already on the stack.
        let enter = |this: &mut Self,
                     frames: &mut Vec<Frame>,
                     locals: &mut Vec<Value>,
                     index: u32|
         -> Result<(), Error> {
            if frames.len() >= MAX_CALL_DEPTH {
                return Err(Error::StackOverflow);
            }
            let f = module
                .functions
                .get(index as usize - n_imports)
                .ok_or(Error::OutOfRange)?;
            let ty = module.function_type(index)?;
            let nparams = ty.params.len();
            if this.stack.len() < nparams {
                return Err(Error::Trap("missing arguments"));
            }
            // Parameters move and declared locals start at zero: work
            // that grows with them.
            let frame_locals = nparams + f.locals as usize;
            if locals.len() + frame_locals > MAX_LOCALS {
                return Err(Error::StackOverflow);
            }
            this.charge(values_cost(frame_locals))?;
            locals
                .try_reserve(frame_locals)
                .map_err(|_| Error::StackOverflow)?;
            let locals_base = locals.len();
            let args_start = this.stack.len() - nparams;
            locals.extend_from_slice(&this.stack[args_start..]);
            this.stack.truncate(args_start);
            locals.resize(locals_base + frame_locals, 0);
            let labels_base = this.labels.len();
            // Implicit function label: a `br` to it is a `return`.
            this.push_label(Label {
                target: f.code_end,
                is_loop: false,
                height: this.stack.len(),
                arity: ty.results.len(),
                params: 0,
            })?;
            frames.push(Frame {
                func: index as usize - n_imports,
                pc: f.code_start,
                locals_base,
                labels_base,
                stack_base: this.stack.len(),
                results: ty.results.len(),
            });
            Ok(())
        };

        let call_import = |this: &mut Self,
                           host: &mut dyn Host,
                           memory: &mut [u8],
                           import: usize|
         -> Result<(), Error> {
            let ty = module.function_type(import as u32)?;
            let n = ty.params.len();
            if this.stack.len() < n {
                return Err(Error::Trap("missing arguments"));
            }
            let args_start = this.stack.len() - n;
            let active = this.memory_len().min(memory.len());
            let result = host.call(import, &this.stack[args_start..], &mut memory[..active])?;
            this.stack.truncate(args_start);
            if !ty.results.is_empty() {
                this.push(result)?;
            }
            Ok(())
        };

        if (entry as usize) < n_imports {
            return call_import(self, host, memory, entry as usize);
        }
        enter(self, &mut frames, &mut locals, entry)?;

        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(());
            };
            let func = module.functions[frame.func];
            let code = &module.bytes[..func.code_end];
            if frame.pc >= code.len() {
                // End of function (without an explicit `end`): return.
                let fr = frames.pop().unwrap();
                self.finish_frame(&fr, &mut locals)?;
                continue;
            }
            let pc = frame.pc;
            let frame_labels_base = frame.labels_base;
            let locals_base = frame.locals_base;
            self.charge(1)?;
            let mut r = Reader::new(code, pc);
            let op = r.u8()?;
            let mut next_pc: Option<usize> = None;
            match op {
                0x00 => return Err(Error::Trap("unreachable")),
                0x01 => {}
                0x02 | 0x03 => {
                    let (params, results) = block_arity(&mut r, module)?;
                    let is_loop = op == 0x03;
                    let target = if is_loop {
                        r.pos
                    } else {
                        module.block(r.pos)?.end as usize
                    };
                    self.push_label(Label {
                        target,
                        is_loop,
                        height: self.stack.len() - params,
                        arity: results,
                        params,
                    })?;
                }
                0x04 => {
                    let (params, results) = block_arity(&mut r, module)?;
                    let block = module.block(r.pos)?;
                    let end = block.end as usize;
                    let cond = self.pop32()?;
                    if cond != 0 || block.else_pos != 0 {
                        self.push_label(Label {
                            target: end,
                            is_loop: false,
                            height: self.stack.len() - params,
                            arity: results,
                            params,
                        })?;
                    }
                    if cond == 0 {
                        next_pc = Some(match block.else_pos {
                            0 => end,
                            p => p as usize,
                        });
                    }
                }
                0x05 => {
                    // End of the `then` branch: jump to the block's `end`.
                    let l = self
                        .labels
                        .pop()
                        .ok_or(Error::Malformed("else without block"))?;
                    next_pc = Some(l.target);
                }
                0x0B => {
                    if self.labels.len() == frame_labels_base + 1 {
                        // Function `end`: return.
                        let fr = frames.pop().unwrap();
                        self.labels.pop();
                        self.finish_frame(&fr, &mut locals)?;
                        continue;
                    }
                    self.labels.pop();
                }
                0x0C => {
                    let depth = r.u32()?;
                    next_pc = self.branch_or_return(depth, frame_labels_base)?;
                    if next_pc.is_none() {
                        let fr = frames.pop().unwrap();
                        self.finish_frame(&fr, &mut locals)?;
                        continue;
                    }
                }
                0x0D => {
                    let depth = r.u32()?;
                    if self.pop32()? != 0 {
                        next_pc = self.branch_or_return(depth, frame_labels_base)?;
                        if next_pc.is_none() {
                            let fr = frames.pop().unwrap();
                            self.finish_frame(&fr, &mut locals)?;
                            continue;
                        }
                    }
                }
                0x0E => {
                    // The validator decoded its labels: picking one reads
                    // none of them (audit H1).
                    let index = self.pop32()?;
                    let depth = module.br_depth(r.pos, index)?;
                    next_pc = self.branch_or_return(depth, frame_labels_base)?;
                    if next_pc.is_none() {
                        let fr = frames.pop().unwrap();
                        self.finish_frame(&fr, &mut locals)?;
                        continue;
                    }
                }
                0x0F => {
                    let fr = frames.pop().unwrap();
                    self.finish_frame(&fr, &mut locals)?;
                    continue;
                }
                0x10 => {
                    let index = r.u32()?;
                    frame.pc = r.pos;
                    if (index as usize) < n_imports {
                        call_import(self, host, memory, index as usize)?;
                    } else {
                        enter(self, &mut frames, &mut locals, index)?;
                    }
                    continue;
                }
                0x11 => {
                    let type_index = r.u32()?;
                    r.u8()?; // table 0
                    let elem = self.pop32()? as usize;
                    let index = module
                        .table
                        .get(elem)
                        .copied()
                        .flatten()
                        .ok_or(Error::Trap("call_indirect: empty entry"))?;
                    let expected = module
                        .types
                        .get(type_index as usize)
                        .ok_or(Error::OutOfRange)?;
                    // Comparing the types is work that grows with them.
                    self.charge(bytes_cost(expected.params.len() + expected.results.len()))?;
                    if module.function_type(index)? != expected {
                        return Err(Error::Trap("call_indirect: type mismatch"));
                    }
                    frame.pc = r.pos;
                    if (index as usize) < n_imports {
                        call_import(self, host, memory, index as usize)?;
                    } else {
                        enter(self, &mut frames, &mut locals, index)?;
                    }
                    continue;
                }
                0x1A => {
                    self.pop()?;
                }
                0x1B | 0x1C => {
                    if op == 0x1C {
                        for _ in 0..r.u32()? {
                            r.valtype()?;
                        }
                    }
                    let c = self.pop32()?;
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(if c != 0 { a } else { b })?;
                }
                0x20 => {
                    let i = r.u32()? as usize;
                    let v = *locals.get(locals_base + i).ok_or(Error::OutOfRange)?;
                    self.push(v)?;
                }
                0x21 => {
                    let i = r.u32()? as usize;
                    let v = self.pop()?;
                    *locals.get_mut(locals_base + i).ok_or(Error::OutOfRange)? = v;
                }
                0x22 => {
                    let i = r.u32()? as usize;
                    let v = *self.stack.last().ok_or(Error::Trap("empty stack"))?;
                    *locals.get_mut(locals_base + i).ok_or(Error::OutOfRange)? = v;
                }
                0x23 => {
                    let i = r.u32()? as usize;
                    let v = *self.globals.get(i).ok_or(Error::OutOfRange)?;
                    self.push(v)?;
                }
                0x24 => {
                    let i = r.u32()? as usize;
                    let v = self.pop()?;
                    *self.globals.get_mut(i).ok_or(Error::OutOfRange)? = v;
                }
                0x28..=0x35 => {
                    let _align = r.u32()?;
                    let offset = r.u32()?;
                    let base = self.pop32()?;
                    let active = self.memory_len();
                    let v = load(&memory[..active], base, offset, op)?;
                    self.push(v)?;
                }
                0x36..=0x3E => {
                    let _align = r.u32()?;
                    let offset = r.u32()?;
                    let v = self.pop()?;
                    let base = self.pop32()?;
                    let active = self.memory_len();
                    store(&mut memory[..active], base, offset, op, v)?;
                }
                0x3F => {
                    r.u8()?;
                    self.push(self.memory_pages as u64)?;
                }
                0x40 => {
                    r.u8()?;
                    let delta = self.pop32()?;
                    let old = self.memory_pages;
                    let new = old as u64 + delta as u64;
                    let max = module
                        .memory_pages
                        .and_then(|(_, max)| max)
                        .unwrap_or(u32::MAX) as u64;
                    if new * PAGE as u64 <= memory.len() as u64 && new <= max {
                        // New pages start zeroed.
                        self.charge(bytes_cost(delta as usize * PAGE))?;
                        memory[old as usize * PAGE..new as usize * PAGE].fill(0);
                        self.memory_pages = new as u32;
                        self.push(old as u64)?;
                    } else {
                        self.push(u32::MAX as u64)?;
                    }
                }
                0x41 => {
                    let v = r.i32()?;
                    self.push(v as u32 as u64)?;
                }
                0x42 => {
                    let v = r.i64()?;
                    self.push(v as u64)?;
                }
                0x43 | 0x44 => return Err(Error::Unsupported("floating point")),
                0x45..=0x4F | 0x67..=0x78 => {
                    let v = self.exec_i32(op)?;
                    self.push(v)?;
                }
                0x50..=0x5A | 0x79..=0x8A => {
                    let v = self.exec_i64(op)?;
                    self.push(v)?;
                }
                0xA7 => {
                    let v = self.pop()?;
                    self.push(v as u32 as u64)?;
                }
                0xAC => {
                    let v = self.pop32()? as i32 as i64 as u64;
                    self.push(v)?;
                }
                0xAD => {
                    let v = self.pop32()? as u64;
                    self.push(v)?;
                }
                0xC0 => {
                    let v = self.pop32()? as i8 as i32 as u32 as u64;
                    self.push(v)?;
                }
                0xC1 => {
                    let v = self.pop32()? as i16 as i32 as u32 as u64;
                    self.push(v)?;
                }
                0xC2 => {
                    let v = self.pop()? as i8 as i64 as u64;
                    self.push(v)?;
                }
                0xC3 => {
                    let v = self.pop()? as i16 as i64 as u64;
                    self.push(v)?;
                }
                0xC4 => {
                    let v = self.pop()? as i32 as i64 as u64;
                    self.push(v)?;
                }
                0xFC => match r.u32()? {
                    10 => {
                        r.u8()?;
                        r.u8()?;
                        let n = self.pop32()? as usize;
                        let src = self.pop32()? as usize;
                        let dst = self.pop32()? as usize;
                        let end_src = src.checked_add(n).ok_or(Error::Trap("memory.copy"))?;
                        let end_dst = dst.checked_add(n).ok_or(Error::Trap("memory.copy"))?;
                        let active = self.memory_len();
                        if end_src > active || end_dst > active {
                            return Err(Error::Trap("out-of-bounds memory access"));
                        }
                        self.charge(bytes_cost(n))?;
                        memory.copy_within(src..end_src, dst);
                    }
                    11 => {
                        r.u8()?;
                        let n = self.pop32()? as usize;
                        let v = self.pop32()? as u8;
                        let dst = self.pop32()? as usize;
                        let end = dst.checked_add(n).ok_or(Error::Trap("memory.fill"))?;
                        if end > self.memory_len() {
                            return Err(Error::Trap("out-of-bounds memory access"));
                        }
                        self.charge(bytes_cost(n))?;
                        memory[dst..end].fill(v);
                    }
                    _ => return Err(Error::Unsupported("0xFC prefix")),
                },
                _ => return Err(Error::Unsupported("opcode")),
            }
            let frame = frames.last_mut().unwrap();
            frame.pc = next_pc.unwrap_or(r.pos);
        }
    }

    /// `br` that may be a function return (implicit label).
    fn branch_or_return(
        &mut self,
        depth: u32,
        frame_labels_base: usize,
    ) -> Result<Option<usize>, Error> {
        let idx = self
            .labels
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(Error::Malformed("label"))?;
        if idx == frame_labels_base {
            return Ok(None); // return: `finish_frame` trims the stack
        }
        self.branch(depth, frame_labels_base)
    }

    /// Function exit: leaves `results` values on top of `stack_base` and
    /// frees the frame's locals and labels.
    fn finish_frame(&mut self, fr: &Frame, locals: &mut Vec<Value>) -> Result<(), Error> {
        let len = self.stack.len();
        if len < fr.stack_base + fr.results {
            return Err(Error::Trap("not enough results"));
        }
        self.charge(values_cost(fr.results))?;
        let from = len - fr.results;
        self.stack.copy_within(from..len, fr.stack_base);
        self.stack.truncate(fr.stack_base + fr.results);
        locals.truncate(fr.locals_base);
        self.labels.truncate(fr.labels_base);
        Ok(())
    }

    fn exec_i32(&mut self, op: u8) -> Result<Value, Error> {
        if op == 0x45 {
            return Ok((self.pop32()? == 0) as u64);
        }
        if (0x67..=0x69).contains(&op) {
            let a = self.pop32()?;
            return Ok(match op {
                0x67 => a.leading_zeros(),
                0x68 => a.trailing_zeros(),
                _ => a.count_ones(),
            } as u64);
        }
        let b = self.pop32()?;
        let a = self.pop32()?;
        let (sa, sb) = (a as i32, b as i32);
        Ok(match op {
            0x46 => (a == b) as u64,
            0x47 => (a != b) as u64,
            0x48 => (sa < sb) as u64,
            0x49 => (a < b) as u64,
            0x4A => (sa > sb) as u64,
            0x4B => (a > b) as u64,
            0x4C => (sa <= sb) as u64,
            0x4D => (a <= b) as u64,
            0x4E => (sa >= sb) as u64,
            0x4F => (a >= b) as u64,
            0x6A => a.wrapping_add(b) as u64,
            0x6B => a.wrapping_sub(b) as u64,
            0x6C => a.wrapping_mul(b) as u64,
            0x6D => {
                if sb == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                sa.checked_div(sb).ok_or(Error::Trap("division overflow"))? as u32 as u64
            }
            0x6E => {
                if b == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                (a / b) as u64
            }
            0x6F => {
                if sb == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                sa.wrapping_rem(sb) as u32 as u64
            }
            0x70 => {
                if b == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                (a % b) as u64
            }
            0x71 => (a & b) as u64,
            0x72 => (a | b) as u64,
            0x73 => (a ^ b) as u64,
            0x74 => a.wrapping_shl(b) as u64,
            0x75 => sa.wrapping_shr(b) as u32 as u64,
            0x76 => a.wrapping_shr(b) as u64,
            0x77 => a.rotate_left(b) as u64,
            0x78 => a.rotate_right(b) as u64,
            _ => return Err(Error::Unsupported("opcode i32")),
        })
    }

    fn exec_i64(&mut self, op: u8) -> Result<Value, Error> {
        if op == 0x50 {
            return Ok((self.pop()? == 0) as u64);
        }
        if (0x79..=0x7B).contains(&op) {
            let a = self.pop()?;
            return Ok(match op {
                0x79 => a.leading_zeros(),
                0x7A => a.trailing_zeros(),
                _ => a.count_ones(),
            } as u64);
        }
        let b = self.pop()?;
        let a = self.pop()?;
        let (sa, sb) = (a as i64, b as i64);
        Ok(match op {
            0x51 => (a == b) as u64,
            0x52 => (a != b) as u64,
            0x53 => (sa < sb) as u64,
            0x54 => (a < b) as u64,
            0x55 => (sa > sb) as u64,
            0x56 => (a > b) as u64,
            0x57 => (sa <= sb) as u64,
            0x58 => (a <= b) as u64,
            0x59 => (sa >= sb) as u64,
            0x5A => (a >= b) as u64,
            0x7C => a.wrapping_add(b),
            0x7D => a.wrapping_sub(b),
            0x7E => a.wrapping_mul(b),
            0x7F => {
                if sb == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                sa.checked_div(sb).ok_or(Error::Trap("division overflow"))? as u64
            }
            0x80 => {
                if b == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                a / b
            }
            0x81 => {
                if sb == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                sa.wrapping_rem(sb) as u64
            }
            0x82 => {
                if b == 0 {
                    return Err(Error::Trap("division by zero"));
                }
                a % b
            }
            0x83 => a & b,
            0x84 => a | b,
            0x85 => a ^ b,
            0x86 => a.wrapping_shl(b as u32),
            0x87 => sa.wrapping_shr(b as u32) as u64,
            0x88 => a.wrapping_shr(b as u32),
            0x89 => a.rotate_left(b as u32),
            0x8A => a.rotate_right(b as u32),
            _ => return Err(Error::Unsupported("opcode i64")),
        })
    }
}

/// Arity (parameters, results) of a block type.
fn block_arity(r: &mut Reader, module: &Module) -> Result<(usize, usize), Error> {
    let b = *r.b.get(r.pos).ok_or(Error::Malformed("block type"))?;
    if b == 0x40 {
        r.pos += 1;
        return Ok((0, 0));
    }
    if matches!(b, 0x7C..=0x7F) {
        r.pos += 1;
        return Ok((0, 1));
    }
    let ti = r.i32()?;
    let ty = module.types.get(ti as usize).ok_or(Error::OutOfRange)?;
    Ok((ty.params.len(), ty.results.len()))
}

fn skip_blocktype(r: &mut Reader) -> Result<(), Error> {
    let b = *r.b.get(r.pos).ok_or(Error::Malformed("block type"))?;
    if b == 0x40 || matches!(b, 0x7C..=0x7F) {
        r.pos += 1;
    } else {
        r.i32()?;
    }
    Ok(())
}

/// Skips the immediates of an instruction (to locate block ends).
fn skip_immediates(op: u8, r: &mut Reader) -> Result<(), Error> {
    match op {
        0x0C | 0x0D | 0x10 | 0x20..=0x24 => {
            r.u32()?;
        }
        0x0E => {
            let n = r.u32()?;
            for _ in 0..=n {
                r.u32()?;
            }
        }
        0x11 => {
            r.u32()?;
            r.u8()?;
        }
        0x28..=0x3E => {
            r.u32()?;
            r.u32()?;
        }
        0x3F | 0x40 => {
            r.u8()?;
        }
        0x41 => {
            r.i32()?;
        }
        0x42 => {
            r.i64()?;
        }
        0x43 => {
            r.bytes(4)?;
        }
        0x44 => {
            r.bytes(8)?;
        }
        0x1C => {
            for _ in 0..r.u32()? {
                r.valtype()?;
            }
        }
        0xFC => match r.u32()? {
            10 => {
                r.u8()?;
                r.u8()?;
            }
            11 => {
                r.u8()?;
            }
            _ => return Err(Error::Unsupported("0xFC prefix")),
        },
        _ => {}
    }
    Ok(())
}

fn effective(base: u32, offset: u32, size: usize, memory: &[u8]) -> Result<usize, Error> {
    let addr = base as u64 + offset as u64;
    let end = addr + size as u64;
    if end > memory.len() as u64 {
        return Err(Error::Trap("out-of-bounds memory access"));
    }
    Ok(addr as usize)
}

fn load(memory: &[u8], base: u32, offset: u32, op: u8) -> Result<Value, Error> {
    let size = match op {
        0x28 | 0x34 | 0x35 => 4,
        0x29 => 8,
        0x2C | 0x2D | 0x30 | 0x31 => 1,
        0x2E | 0x2F | 0x32 | 0x33 => 2,
        _ => return Err(Error::Unsupported("floating-point load")),
    };
    let a = effective(base, offset, size, memory)?;
    let b = &memory[a..a + size];
    Ok(match op {
        0x28 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64,
        0x29 => u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        0x2C => b[0] as i8 as i32 as u32 as u64,
        0x2D => b[0] as u64,
        0x2E => i16::from_le_bytes([b[0], b[1]]) as i32 as u32 as u64,
        0x2F => u16::from_le_bytes([b[0], b[1]]) as u64,
        0x30 => b[0] as i8 as i64 as u64,
        0x31 => b[0] as u64,
        0x32 => i16::from_le_bytes([b[0], b[1]]) as i64 as u64,
        0x33 => u16::from_le_bytes([b[0], b[1]]) as u64,
        0x34 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64 as u64,
        0x35 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64,
        _ => unreachable!(),
    })
}

fn store(memory: &mut [u8], base: u32, offset: u32, op: u8, v: Value) -> Result<(), Error> {
    let size = match op {
        0x36 | 0x3E => 4,
        0x37 => 8,
        0x3A | 0x3C => 1,
        0x3B | 0x3D => 2,
        _ => return Err(Error::Unsupported("floating-point store")),
    };
    let a = effective(base, offset, size, memory)?;
    let bytes = v.to_le_bytes();
    memory[a..a + size].copy_from_slice(&bytes[..size]);
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    /// Minimal assembler for the tests: a module with types, imports,
    /// functions, one memory page, exports and data.
    struct Builder {
        types: Vec<(Vec<u8>, Vec<u8>)>,
        imports: Vec<(&'static str, &'static str, u32)>,
        funcs: Vec<(u32, u32, Vec<u8>)>,
        exports: Vec<(&'static str, u32)>,
        data: Vec<(u32, Vec<u8>)>,
        table: Vec<u32>,
        globals: Vec<(u8, bool, i64)>,
        start: Option<u32>,
    }

    fn leb(mut v: u32, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn sleb(mut v: i64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
            if done {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn section(id: u8, body: &[u8], out: &mut Vec<u8>) {
        out.push(id);
        leb(body.len() as u32, out);
        out.extend_from_slice(body);
    }

    impl Builder {
        fn new() -> Self {
            Self {
                types: Vec::new(),
                imports: Vec::new(),
                funcs: Vec::new(),
                exports: Vec::new(),
                data: Vec::new(),
                table: Vec::new(),
                globals: Vec::new(),
                start: None,
            }
        }
        fn ty(&mut self, params: &[u8], results: &[u8]) -> u32 {
            self.types.push((params.to_vec(), results.to_vec()));
            self.types.len() as u32 - 1
        }
        fn import(&mut self, module: &'static str, name: &'static str, ty: u32) -> u32 {
            self.imports.push((module, name, ty));
            self.imports.len() as u32 - 1
        }
        fn func(&mut self, ty: u32, locals: u32, code: &[u8]) -> u32 {
            self.funcs.push((ty, locals, code.to_vec()));
            self.imports.len() as u32 + self.funcs.len() as u32 - 1
        }
        fn export(&mut self, name: &'static str, f: u32) {
            self.exports.push((name, f));
        }
        fn build(&self) -> Vec<u8> {
            let mut out = b"\0asm\x01\0\0\0".to_vec();
            let mut b = Vec::new();
            leb(self.types.len() as u32, &mut b);
            for (p, r) in &self.types {
                b.push(0x60);
                leb(p.len() as u32, &mut b);
                b.extend_from_slice(p);
                leb(r.len() as u32, &mut b);
                b.extend_from_slice(r);
            }
            section(1, &b, &mut out);
            if !self.imports.is_empty() {
                let mut b = Vec::new();
                leb(self.imports.len() as u32, &mut b);
                for (m, n, t) in &self.imports {
                    leb(m.len() as u32, &mut b);
                    b.extend_from_slice(m.as_bytes());
                    leb(n.len() as u32, &mut b);
                    b.extend_from_slice(n.as_bytes());
                    b.push(0);
                    leb(*t, &mut b);
                }
                section(2, &b, &mut out);
            }
            let mut b = Vec::new();
            leb(self.funcs.len() as u32, &mut b);
            for (t, _, _) in &self.funcs {
                leb(*t, &mut b);
            }
            section(3, &b, &mut out);
            if !self.table.is_empty() {
                let mut b = vec![1, 0x70, 0];
                leb(self.table.len() as u32, &mut b);
                section(4, &b, &mut out);
            }
            section(5, &[1, 0, 1], &mut out); // memory: 1 page
            if !self.globals.is_empty() {
                let mut b = Vec::new();
                leb(self.globals.len() as u32, &mut b);
                for (ty, mutable, init) in &self.globals {
                    b.push(*ty);
                    b.push(*mutable as u8);
                    b.push(if *ty == 0x7F { 0x41 } else { 0x42 });
                    sleb(*init, &mut b);
                    b.push(0x0B);
                }
                section(6, &b, &mut out);
            }
            let mut b = Vec::new();
            leb(self.exports.len() as u32, &mut b);
            for (n, f) in &self.exports {
                leb(n.len() as u32, &mut b);
                b.extend_from_slice(n.as_bytes());
                b.push(0);
                leb(*f, &mut b);
            }
            section(7, &b, &mut out);
            if let Some(s) = self.start {
                let mut b = Vec::new();
                leb(s, &mut b);
                section(8, &b, &mut out);
            }
            if !self.table.is_empty() {
                let mut b = vec![1, 0, 0x41, 0, 0x0B];
                leb(self.table.len() as u32, &mut b);
                for f in &self.table {
                    leb(*f, &mut b);
                }
                section(9, &b, &mut out);
            }
            let mut b = Vec::new();
            leb(self.funcs.len() as u32, &mut b);
            for (_, locals, code) in &self.funcs {
                let mut body = Vec::new();
                if *locals > 0 {
                    body.push(1);
                    leb(*locals, &mut body);
                    body.push(0x7F);
                } else {
                    body.push(0);
                }
                body.extend_from_slice(code);
                body.push(0x0B);
                leb(body.len() as u32, &mut b);
                b.extend_from_slice(&body);
            }
            section(10, &b, &mut out);
            if !self.data.is_empty() {
                let mut b = Vec::new();
                leb(self.data.len() as u32, &mut b);
                for (off, bytes) in &self.data {
                    b.push(0);
                    b.push(0x41);
                    sleb(*off as i64, &mut b);
                    b.push(0x0B);
                    leb(bytes.len() as u32, &mut b);
                    b.extend_from_slice(bytes);
                }
                section(11, &b, &mut out);
            }
            out
        }
    }

    struct NoHost;
    impl Host for NoHost {
        fn call(&mut self, _: usize, _: &[Value], _: &mut [u8]) -> Result<Value, Error> {
            Err(Error::MissingImport)
        }
    }

    fn run(wasm: &[u8], name: &str, args: &[Value]) -> Result<Value, Error> {
        let m = Module::parse(wasm)?;
        let mut mem = vec![0u8; PAGE];
        let mut inst = Instance::new(&m, &mut mem)?;
        inst.call(name, args, &mut mem, &mut NoHost)
    }

    // Codes: 0x20 local.get, 0x41 i32.const, 0x6A add, 0x6C mul, 0x6B sub

    #[test]
    fn arithmetic_and_locals() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7F, 0x7F], &[0x7F]);
        // (a + b) * 2 - 1
        let f = b.func(
            t,
            0,
            &[0x20, 0, 0x20, 1, 0x6A, 0x41, 2, 0x6C, 0x41, 1, 0x6B],
        );
        b.export("f", f);
        assert_eq!(run(&b.build(), "f", &[3, 4]), Ok(13));
        assert_eq!(run(&b.build(), "nope", &[]), Err(Error::NoExport));
    }

    #[test]
    fn loop_sums_with_br_if_and_locals() {
        // sum(n): i=0,acc=0; loop { acc+=i; i+=1; br_if i<n } -> acc
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let code = [
            0x03, 0x40, // loop
            0x20, 2, 0x20, 1, 0x6A, 0x21, 2, // acc += i
            0x20, 1, 0x41, 1, 0x6A, 0x22, 1, // i += 1 (tee)
            0x20, 0, 0x49, // i <u n
            0x0D, 0,    // br_if 0
            0x0B, // end loop
            0x20, 2,
        ];
        let f = b.func(t, 2, &code);
        b.export("sum", f);
        assert_eq!(run(&b.build(), "sum", &[10]), Ok(45));
        assert_eq!(run(&b.build(), "sum", &[1]), Ok(0));
    }

    /// One call with `fuel` units: its result, the fuel it spent, the code
    /// bytes it read and global 0 at the end (the tests' modules count
    /// their own loop passes there).
    struct Metered {
        result: Result<Value, Error>,
        steps: u64,
        reads: u64,
        counter: Value,
    }

    fn metered(wasm: &[u8], name: &str, args: &[Value], fuel: u64) -> Metered {
        let m = Module::parse(wasm).unwrap();
        let mut mem = vec![0u8; PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        inst.fuel = fuel;
        let before = meter::CODE_READS.with(|c| c.get());
        let result = inst.call(name, args, &mut mem, &mut NoHost);
        Metered {
            result,
            steps: inst.steps,
            reads: meter::CODE_READS.with(|c| c.get()) - before,
            counter: inst.global(0).unwrap_or(0),
        }
    }

    #[test]
    fn entering_a_block_costs_no_scan_of_its_body() {
        // loop { block { 4000 x nop } ; i += 1 ; br_if i < n }: each pass
        // enters the block, whose end lies 4000 instructions further on.
        // Finding that end again on every entry would read about twice the
        // bytes of the instructions run (audit 2026-10-25 B, L5).
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let mut code = vec![0x03, 0x40, 0x02, 0x40];
        code.extend(core::iter::repeat_n(0x01, 4000));
        code.extend_from_slice(&[
            0x0B, // end block
            0x20, 1, 0x41, 1, 0x6A, 0x22, 1, // i += 1 (tee)
            0x20, 0, 0x49, 0x0D, 0,    // br_if i <u n
            0x0B, // end loop
            0x20, 1,
        ]);
        let f = b.func(t, 1, &code);
        b.export("f", f);
        let run = metered(&b.build(), "f", &[200], MAX_STEPS);
        assert_eq!(run.result, Ok(200));
        assert!(
            run.reads < run.steps + run.steps / 4,
            "{} code bytes read for {} units of fuel",
            run.reads,
            run.steps
        );
    }

    #[test]
    fn block_table_is_compact_and_built_by_validation() {
        // The largest module the runner loads (64 KiB) with a body of
        // nothing but blocks: one 12-byte entry per block, reserved once
        // (audit M1: a map of ~63 B per block did not fit the runner's
        // 256 KiB heap and panicked it).
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        let mut code = Vec::new();
        let mut opened = 0;
        while code.len() < 64 * 1024 - 100 {
            // Every tenth one an `if` with an `else` (on 0: runs the `else`).
            if opened % 10 == 0 {
                code.extend_from_slice(&[0x41, 0, 0x04, 0x40, 0x05, 0x0B]);
            } else {
                code.extend_from_slice(&[0x02, 0x40, 0x0B]);
            }
            opened += 1;
        }
        code.extend_from_slice(&[0x41, 7]);
        let f = b.func(t, 0, &code);
        b.export("f", f);
        let wasm = b.build();
        assert!(wasm.len() <= 64 * 1024, "{} bytes", wasm.len());
        let m = Module::parse(&wasm).unwrap();
        assert_eq!(m.side.blocks.len(), opened);
        assert_eq!(m.side.blocks.capacity(), opened, "reserved exactly");
        assert_eq!(core::mem::size_of::<BlockEnds>(), 12);
        assert!(m.side.blocks.len() * 12 <= 4 * wasm.len());
        // Instances share the table; running reads only what runs.
        for _ in 0..2 {
            let run = metered(&wasm, "f", &[], MAX_STEPS);
            assert_eq!(run.result, Ok(7));
            assert!(run.reads <= wasm.len() as u64, "{} bytes read", run.reads);
        }
    }

    #[test]
    fn br_table_picks_a_label_without_decoding_the_others() {
        // loop { block { br_table [0; 60000] 0 (index a) } ; g += 1 ; br 0 }
        // Decoding the labels on every pass cost 60000 reads for one unit
        // of fuel (audit H1).
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[]);
        b.globals.push((0x7F, true, 0));
        let n = 60_000u32;
        let mut code = vec![0x03, 0x40, 0x02, 0x40, 0x20, 0, 0x0E];
        leb(n, &mut code);
        code.extend(core::iter::repeat_n(0, n as usize + 1));
        code.extend_from_slice(&[0x0B, 0x23, 0, 0x41, 1, 0x6A, 0x24, 0, 0x0C, 0, 0x0B]);
        let f = b.func(t, 0, &code);
        b.export("f", f);
        let wasm = b.build();
        for index in [0, 59_999, 70_000] {
            let run = metered(&wasm, "f", &[index], 100_000);
            assert_eq!(run.result, Err(Error::Fuel));
            assert!(run.counter > 1000, "{} passes", run.counter);
            assert!(run.reads <= 4 * run.steps, "{} reads", run.reads);
        }
    }

    #[test]
    fn br_table_with_multibyte_labels_and_default() {
        // As in `br_table_globals_and_indirect_calls`, with labels encoded
        // in two and three bytes (0x80 0x00 is 0): the side table holds
        // the decoded depths.
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let code = [
            0x02, 0x7F, 0x02, 0x40, 0x02, 0x40, 0x02, 0x40, 0x20, 0, 0x0E, 2, 0x80, 0x00, 0x81,
            0x00, 0x82, 0x80, 0x00, 0x0B, 0x41, 0xE4, 0x00, 0x0C, 2, 0x0B, 0x41, 0xC8, 0x01, 0x0C,
            1, 0x0B, 0x41, 0xAC, 0x02, 0x0B,
        ];
        let sw = b.func(t, 0, &code);
        b.export("sw", sw);
        let wasm = b.build();
        assert_eq!(run(&wasm, "sw", &[0]), Ok(100));
        assert_eq!(run(&wasm, "sw", &[1]), Ok(200));
        assert_eq!(run(&wasm, "sw", &[2]), Ok(300));
        assert_eq!(run(&wasm, "sw", &[u32::MAX as u64]), Ok(300));
    }

    #[test]
    fn bulk_memory_costs_fuel_by_length() {
        // loop { memory.fill/copy of the whole page ; g += 1 ; br 0 }: one
        // unit of fuel used to fill 64 KiB (audit M2).
        for op in [&[0xFC, 11, 0][..], &[0xFC, 10, 0, 0][..]] {
            let mut b = Builder::new();
            let t = b.ty(&[], &[]);
            b.globals.push((0x7F, true, 0));
            let mut code = vec![0x03, 0x40, 0x41, 0, 0x41, 0, 0x41, 0x80, 0x80, 0x04];
            code.extend_from_slice(op);
            code.extend_from_slice(&[0x23, 0, 0x41, 1, 0x6A, 0x24, 0, 0x0C, 0, 0x0B]);
            let f = b.func(t, 0, &code);
            b.export("f", f);
            let fuel = 100_000;
            let run = metered(&b.build(), "f", &[], fuel);
            assert_eq!(run.result, Err(Error::Fuel));
            let bytes = run.counter * PAGE as u64;
            assert!(run.counter >= 1);
            assert!(
                bytes / WORK_BYTES as u64 <= fuel,
                "{bytes} bytes for {fuel} units"
            );
        }
    }

    #[test]
    fn branch_keeping_many_values_costs_fuel_by_them() {
        // 1000 values ; loop (param i32 x 1000) { g += 1 ; br 0 }: each `br`
        // keeps the loop's 1000 parameters (audit M3).
        let mut b = Builder::new();
        let t = b.ty(&[], &[]);
        let wide = b.ty(&[0x7F; 1000], &[]);
        b.globals.push((0x7F, true, 0));
        let mut code = Vec::new();
        for _ in 0..1000 {
            code.extend_from_slice(&[0x41, 0]);
        }
        code.extend_from_slice(&[0x03, wide as u8]);
        code.extend_from_slice(&[0x23, 0, 0x41, 1, 0x6A, 0x24, 0, 0x0C, 0, 0x0B]);
        let f = b.func(t, 0, &code);
        b.export("f", f);
        let fuel = 100_000;
        let run = metered(&b.build(), "f", &[], fuel);
        assert_eq!(run.result, Err(Error::Fuel));
        assert!(run.counter >= 1);
        assert!(
            values_cost(run.counter as usize * 1000) <= fuel,
            "{} passes",
            run.counter
        );
    }

    #[test]
    fn calls_pay_for_their_locals_and_locals_are_bounded() {
        // g += 1 ; call self, with 4096 locals: the locals of all frames
        // are bounded (audit L2: 64 frames x 4096 locals reached 2 MiB).
        let mut b = Builder::new();
        let t = b.ty(&[], &[]);
        b.globals.push((0x7F, true, 0));
        let deep = b.func(t, 4096, &[0x23, 0, 0x41, 1, 0x6A, 0x24, 0, 0x10, 0]);
        b.export("deep", deep);
        let wide = b.func(t, 4096, &[0x23, 0, 0x41, 1, 0x6A, 0x24, 0]);
        // loop { call wide ; br 0 }
        let caller = b.func(t, 0, &[0x03, 0x40, 0x10, wide as u8, 0x0C, 0, 0x0B]);
        b.export("caller", caller);
        let wasm = b.build();
        let run = metered(&wasm, "deep", &[], MAX_STEPS);
        assert_eq!(run.result, Err(Error::StackOverflow));
        assert!(
            run.counter as usize <= MAX_LOCALS / 4096,
            "{} frames",
            run.counter
        );
        // Zeroing 4096 locals costs fuel on every call.
        let fuel = 100_000;
        let run = metered(&wasm, "caller", &[], fuel);
        assert_eq!(run.result, Err(Error::Fuel));
        assert!(run.counter >= 1);
        assert!(values_cost(run.counter as usize * 4096) <= fuel);
    }

    #[test]
    fn validating_br_table_checks_each_label_depth_once() {
        // block (result i32 x 1000) { block (result i32 x 1000) {
        //   unreachable ; br_table [0 1 0 1 ...; 20000] 0 } }: checking
        // every label cost labels x arity (audit L3).
        let mut b = Builder::new();
        let wide = b.ty(&[], &[0x7F; 1000]);
        let mut code = vec![0x02, wide as u8, 0x02, wide as u8, 0x00, 0x41, 0, 0x0E];
        leb(20_000, &mut code);
        for i in 0..=20_000 {
            code.push((i % 2) as u8);
        }
        code.extend_from_slice(&[0x0B, 0x0B]);
        let f = b.func(wide, 0, &code);
        b.export("f", f);
        let wasm = b.build();
        validate::meter::TYPE_CHECKS.with(|c| c.set(0));
        assert!(Module::parse(&wasm).is_ok());
        let checks = validate::meter::TYPE_CHECKS.with(|c| c.get());
        assert!(checks < 20_000, "{checks} type checks");
    }

    #[test]
    fn function_types_are_bounded() {
        let mut b = Builder::new();
        b.ty(&[0x7F; MAX_ARITY + 1], &[]);
        assert_eq!(
            Module::parse(&b.build()).err(),
            Some(Error::Unsupported("too many params or results"))
        );
        let mut b = Builder::new();
        b.ty(&[], &[0x7E; MAX_ARITY]);
        assert!(Module::parse(&b.build()).is_ok());
    }

    #[test]
    fn if_else_block_results_and_select() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        // if (a) { 10 } else { 20 } + select(a, 1, 2)
        let code = [
            0x20, 0, 0x04, 0x7F, 0x41, 10, 0x05, 0x41, 20, 0x0B, // if/else -> i32
            0x41, 1, 0x41, 2, 0x20, 0, 0x1B, // select
            0x6A,
        ];
        let f = b.func(t, 0, &code);
        b.export("f", f);
        assert_eq!(run(&b.build(), "f", &[1]), Ok(11));
        assert_eq!(run(&b.build(), "f", &[0]), Ok(22));
    }

    #[test]
    fn calls_recursion_and_return() {
        // fib(n) = n < 2 ? n : fib(n-1) + fib(n-2)
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let code = [
            0x20, 0, 0x41, 2, 0x48, // n < 2
            0x04, 0x40, 0x20, 0, 0x0F, 0x0B, // if { return n }
            0x20, 0, 0x41, 1, 0x6B, 0x10, 0, // fib(n-1)
            0x20, 0, 0x41, 2, 0x6B, 0x10, 0, // fib(n-2)
            0x6A,
        ];
        let f = b.func(t, 0, &code);
        b.export("fib", f);
        assert_eq!(run(&b.build(), "fib", &[10]), Ok(55));
        assert_eq!(run(&b.build(), "fib", &[20]), Ok(6765));
    }

    #[test]
    fn memory_loads_stores_and_bounds() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7F, 0x7F], &[0x7F]);
        // store32 [addr] = v ; return load8_u [addr+1]
        let code = [
            0x20, 0, 0x20, 1, 0x36, 2, 0, // i32.store align=2 offset=0
            0x20, 0, 0x2D, 0, 1, // i32.load8_u offset=1
        ];
        let f = b.func(t, 0, &code);
        b.export("f", f);
        b.data.push((16, b"hola".to_vec()));
        let wasm = b.build();
        assert_eq!(run(&wasm, "f", &[100, 0x11223344]), Ok(0x33));
        assert_eq!(
            run(&wasm, "f", &[PAGE as u64 - 2, 1]),
            Err(Error::Trap("out-of-bounds memory access"))
        );
        // The initial data reaches memory.
        let m = Module::parse(&wasm).unwrap();
        let mut mem = vec![0u8; PAGE];
        let _ = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(&mem[16..20], b"hola");
    }

    #[test]
    fn traps_and_limits() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let div = b.func(t, 0, &[0x41, 1, 0x20, 0, 0x6D]); // 1 / a
        b.export("div", div);
        let unreach = b.func(t, 0, &[0x00]);
        b.export("unreach", unreach);
        let forever = b.func(t, 0, &[0x03, 0x40, 0x0C, 0, 0x0B, 0x41, 0]);
        b.export("forever", forever);
        let recurse = b.func(t, 0, &[0x20, 0, 0x10, 3]); // calls itself (index 3)
        b.export("recurse", recurse);
        let wasm = b.build();
        assert_eq!(
            run(&wasm, "div", &[0]),
            Err(Error::Trap("division by zero"))
        );
        assert_eq!(run(&wasm, "div", &[2]), Ok(0));
        assert_eq!(run(&wasm, "unreach", &[0]), Err(Error::Trap("unreachable")));
        assert_eq!(run(&wasm, "forever", &[0]), Err(Error::Fuel));
        assert_eq!(run(&wasm, "recurse", &[0]), Err(Error::StackOverflow));
    }

    #[test]
    fn imports_reach_the_host() {
        struct Log(Vec<Value>);
        impl Host for Log {
            fn call(
                &mut self,
                import: usize,
                args: &[Value],
                memory: &mut [u8],
            ) -> Result<Value, Error> {
                assert_eq!(import, 0);
                self.0.extend_from_slice(args);
                Ok(memory[0] as u64 + 100)
            }
        }
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let imp = b.import("aricode", "echo", t);
        let f = b.func(t, 0, &[0x20, 0, 0x10, imp as u8]);
        b.export("f", f);
        b.data.push((0, vec![7]));
        let wasm = b.build();
        let m = Module::parse(&wasm).unwrap();
        assert_eq!(m.imports[0].name, b"echo");
        let mut mem = vec![0u8; PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        let mut host = Log(Vec::new());
        assert_eq!(inst.call("f", &[42], &mut mem, &mut host), Ok(107));
        assert_eq!(host.0, vec![42]);
    }

    #[test]
    fn br_table_globals_and_indirect_calls() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        b.globals.push((0x7F, true, 5));
        // br_table: block block block (br_table [0 1] 2) ... -> 100/200/300
        let code = [
            0x02, 0x7F, 0x02, 0x40, 0x02, 0x40, 0x02, 0x40, 0x20, 0, 0x0E, 2, 0, 1, 2, 0x0B, 0x41,
            0xE4, 0x00, 0x0C, 2, 0x0B, 0x41, 0xC8, 0x01, 0x0C, 1, 0x0B, 0x41, 0xAC, 0x02, 0x0B,
        ];
        let sw = b.func(t, 0, &code);
        b.export("sw", sw);
        // global: g = g + a; return g
        let g = b.func(t, 0, &[0x23, 0, 0x20, 0, 0x6A, 0x24, 0, 0x23, 0]);
        b.export("g", g);
        // indirect: call_indirect table[a] with type t, argument 1
        b.table = vec![sw, g];
        let ind = b.func(t, 0, &[0x41, 1, 0x20, 0, 0x11, t as u8, 0]);
        b.export("ind", ind);
        let wasm = b.build();
        assert_eq!(run(&wasm, "sw", &[0]), Ok(100));
        assert_eq!(run(&wasm, "sw", &[1]), Ok(200));
        assert_eq!(run(&wasm, "sw", &[7]), Ok(300));
        assert_eq!(run(&wasm, "g", &[3]), Ok(8));
        assert_eq!(run(&wasm, "ind", &[0]), Ok(200), "sw(1)");
        assert_eq!(run(&wasm, "ind", &[1]), Ok(6), "g(1)");
        assert_eq!(
            run(&wasm, "ind", &[5]),
            Err(Error::Trap("call_indirect: empty entry"))
        );
    }

    #[test]
    fn i64_and_conversions() {
        let mut b = Builder::new();
        let t = b.ty(&[0x7E, 0x7E], &[0x7E]);
        let f = b.func(t, 0, &[0x20, 0, 0x20, 1, 0x7E, 0x42, 1, 0x7C]); // a*b+1
        b.export("mul", f);
        let t2 = b.ty(&[0x7F], &[0x7E]);
        let ext = b.func(t2, 0, &[0x20, 0, 0xAC]); // i64.extend_i32_s
        b.export("ext", ext);
        let wasm = b.build();
        assert_eq!(run(&wasm, "mul", &[1 << 40, 3]), Ok(3 << 40 | 1));
        assert_eq!(
            run(&wasm, "ext", &[(-5i32) as u32 as u64]),
            Ok((-5i64) as u64)
        );
    }

    #[test]
    fn bulk_memory_copy_and_fill() {
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        // fill [0..4) = 7 ; copy [8..12) <- [0..4) ; load8 [10]
        let code = [
            0x41, 0, 0x41, 7, 0x41, 4, 0xFC, 11, 0, // memory.fill
            0x41, 8, 0x41, 0, 0x41, 4, 0xFC, 10, 0, 0, // memory.copy
            0x41, 10, 0x2D, 0, 0, // i32.load8_u
        ];
        let f = b.func(t, 0, &code);
        b.export("f", f);
        assert_eq!(run(&b.build(), "f", &[]), Ok(7));
        // Fill outside the page: trap, no panic.
        let mut big = Builder::new();
        let t2 = big.ty(&[], &[0x7F]);
        let g = big.func(
            t2,
            0,
            &[
                0x41, 0xFF, 0xFF, 0x03, 0x41, 0, 0x41, 0x10, 0xFC, 11, 0, 0x41, 1,
            ],
        );
        big.export("g", g);
        assert_eq!(
            run(&big.build(), "g", &[]),
            Err(Error::Trap("out-of-bounds memory access"))
        );
    }

    #[test]
    fn memory_is_bounded_by_current_pages_not_host_buffer() {
        // 1-page module (max 1) over a 4-page buffer: reading at 65536 traps.
        let mut b = Builder::new();
        let t = b.ty(&[0x7F], &[0x7F]);
        let f = b.func(t, 0, &[0x20, 0, 0x2D, 0, 0]);
        b.export("ld", f);
        let g = b.func(t, 0, &[0x20, 0, 0x41, 0x5A, 0x3A, 0, 0, 0x41, 1]);
        b.export("st", g);
        let wasm = b.build();
        let m = Module::parse(&wasm).unwrap();
        let mut mem = vec![0u8; 4 * PAGE];
        mem[PAGE] = 0x5A;
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(inst.memory_len(), PAGE);
        assert_eq!(
            inst.call("ld", &[PAGE as u64], &mut mem, &mut NoHost),
            Err(Error::Trap("out-of-bounds memory access"))
        );
        assert_eq!(
            inst.call("st", &[PAGE as u64], &mut mem, &mut NoHost),
            Err(Error::Trap("out-of-bounds memory access"))
        );
        assert_eq!(
            inst.call("ld", &[(PAGE - 1) as u64], &mut mem, &mut NoHost),
            Ok(0)
        );
        // The host does not see beyond the active memory either.
        struct Peek;
        impl Host for Peek {
            fn call(&mut self, _: usize, _: &[Value], memory: &mut [u8]) -> Result<Value, Error> {
                Ok(memory.len() as u64)
            }
        }
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        let imp = b.import("aricode", "len", t);
        let f = b.func(t, 0, &[0x10, imp as u8]);
        b.export("f", f);
        let wasm = b.build();
        let m = Module::parse(&wasm).unwrap();
        let mut mem = vec![0u8; 4 * PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(inst.call("f", &[], &mut mem, &mut Peek), Ok(PAGE as u64));
    }

    #[test]
    fn memory_starts_and_grows_zeroed() {
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        let ld0 = b.func(t, 0, &[0x41, 0, 0x2D, 0, 0]);
        b.export("ld0", ld0);
        // grow 1 page and read the first byte of the new one.
        let grow = b.func(t, 0, &[0x41, 1, 0x40, 0, 0x1A, 0x41, 0, 0x2D, 0, 0]);
        b.export("grow", grow);
        b.data.push((8, vec![1, 2, 3]));
        let wasm = b.build();
        // The test builder declares memory {min 1} without a maximum.
        let m = Module::parse(&wasm).unwrap();
        let mut mem = vec![0xAAu8; 4 * PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(mem[0], 0, "initial memory zeroed");
        assert_eq!(&mem[8..11], &[1, 2, 3], "data applied");
        assert_eq!(inst.call("ld0", &[], &mut mem, &mut NoHost), Ok(0));
        // After growing, byte 65536 (previously 0xAA in the buffer) must be 0.
        let mut b2 = Builder::new();
        let t2 = b2.ty(&[], &[0x7F]);
        let g = b2.func(
            t2,
            0,
            &[0x41, 1, 0x40, 0, 0x1A, 0x41, 0x80, 0x80, 0x04, 0x2D, 0, 0],
        );
        b2.export("g", g);
        let wasm2 = b2.build();
        let m2 = Module::parse(&wasm2).unwrap();
        let mut mem2 = vec![0xAAu8; 4 * PAGE];
        let mut inst2 = Instance::new(&m2, &mut mem2).unwrap();
        assert_eq!(inst2.call("g", &[], &mut mem2, &mut NoHost), Ok(0));
        assert_eq!(inst2.memory_len(), 2 * PAGE);
    }

    #[test]
    fn start_function_runs_before_exports() {
        let mut b = Builder::new();
        let tv = b.ty(&[], &[]);
        let t = b.ty(&[], &[0x7F]);
        b.globals.push((0x7F, true, 0));
        let start = b.func(tv, 0, &[0x41, 42, 0x24, 0]); // global = 42
        let get = b.func(t, 0, &[0x23, 0]);
        b.export("get", get);
        b.start = Some(start);
        let wasm = b.build();
        let m = Module::parse(&wasm).unwrap();
        assert_eq!(m.start, Some(start));
        let mut mem = vec![0u8; PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(
            inst.call("get", &[], &mut mem, &mut NoHost),
            Ok(0),
            "no start yet"
        );
        inst.start(&mut mem, &mut NoHost).unwrap();
        assert_eq!(inst.call("get", &[], &mut mem, &mut NoHost), Ok(42));
        // A start that traps is propagated.
        let mut b = Builder::new();
        let tv = b.ty(&[], &[]);
        let bad = b.func(tv, 0, &[0x00]);
        b.start = Some(bad);
        let wasm = b.build();
        let m = Module::parse(&wasm).unwrap();
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(
            inst.start(&mut mem, &mut NoHost),
            Err(Error::Trap("unreachable"))
        );
    }

    #[test]
    fn rejects_bad_modules() {
        assert_eq!(
            Module::parse(b"nope").err(),
            Some(Error::Malformed("header"))
        );
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        b.func(t, 0, &[0x43, 0, 0, 0, 0]); // f32.const
        b.export("f", 0);
        assert_eq!(
            run(&b.build(), "f", &[]),
            Err(Error::Unsupported("floating point"))
        );
        let mut truncated = b.build();
        truncated.truncate(truncated.len() - 3);
        assert!(Module::parse(&truncated).is_err());
    }

    /// Minimal modules from audit 6ee19fb (finding 6 and observation),
    /// checked against Node's `WebAssembly.validate`: three invalid ones and a
    /// `loop` with a parameter that must return 0.
    #[test]
    fn audit_fixtures_structural_validation_and_loop_params() {
        // i32 global initialized with i64.const
        let wrong_global = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7F, 0x00, 0x42,
            0x01, 0x0B,
        ];
        assert_eq!(
            Module::parse(&wrong_global).err(),
            Some(Error::Invalid("global initializer type"))
        );
        // export of memory 99 without memory
        let missing_memory = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x07, 0x05, 0x01, 0x01, 0x6D, 0x02,
            0x63,
        ];
        assert_eq!(
            Module::parse(&missing_memory).err(),
            Some(Error::Invalid("export out of range"))
        );
        // memory with minimum 2 and maximum 1
        let inverted_limits = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x01, 0x02, 0x01,
        ];
        assert_eq!(
            Module::parse(&inverted_limits).err(),
            Some(Error::Invalid("memory limits"))
        );
        // loop (param i32) with br_if: counts down from 2 and returns 0
        let loop_params = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0A, 0x02, 0x60, 0x00, 0x01,
            0x7F, 0x60, 0x01, 0x7F, 0x01, 0x7F, 0x03, 0x02, 0x01, 0x00, 0x07, 0x08, 0x01, 0x04,
            0x6D, 0x61, 0x69, 0x6E, 0x00, 0x00, 0x0A, 0x16, 0x01, 0x14, 0x01, 0x01, 0x7F, 0x41,
            0x02, 0x03, 0x01, 0x22, 0x00, 0x41, 0x01, 0x6B, 0x22, 0x00, 0x20, 0x00, 0x0D, 0x00,
            0x0B, 0x0B,
        ];
        assert_eq!(run(&loop_params, "main", &[]), Ok(0));
    }

    /// The validator rejects before execution what the interpreter would
    /// only discover halfway through (ADR 0019).
    #[test]
    fn validator_rejects_ill_typed_modules() {
        fn invalid(code: &[u8], params: &[u8], results: &[u8]) -> Option<Error> {
            let mut b = Builder::new();
            let t = b.ty(params, results);
            let f = b.func(t, 1, code);
            b.export("f", f);
            Module::parse(&b.build()).err()
        }
        // Empty stack when adding.
        assert_eq!(
            invalid(&[0x41, 1, 0x6A], &[], &[0x7F]),
            Some(Error::Invalid("empty stack"))
        );
        // i64 where i32 is expected.
        assert_eq!(
            invalid(&[0x42, 1, 0x41, 2, 0x6A], &[], &[0x7F]),
            Some(Error::Invalid("operand type"))
        );
        // Leftover value at function end.
        assert_eq!(
            invalid(&[0x41, 1, 0x41, 2], &[], &[0x7F]),
            Some(Error::Invalid("leftover values at block end"))
        );
        // Nonexistent label.
        assert_eq!(
            invalid(&[0x0C, 5], &[], &[]),
            Some(Error::Invalid("label out of range"))
        );
        // `if` with a result and no `else`.
        assert_eq!(
            invalid(&[0x41, 1, 0x04, 0x7F, 0x41, 7, 0x0B], &[], &[0x7F]),
            Some(Error::Invalid("if without else with results"))
        );
        // Local and global out of range; unknown opcode.
        assert_eq!(
            invalid(&[0x20, 9], &[], &[0x7F]),
            Some(Error::Invalid("local out of range"))
        );
        assert_eq!(
            invalid(&[0x23, 0], &[], &[0x7F]),
            Some(Error::Invalid("global out of range"))
        );
        assert_eq!(
            invalid(&[0xD0], &[], &[]),
            Some(Error::Unsupported("opcode"))
        );
        // Unreachable code: the stack is polymorphic and the rest validates.
        assert_eq!(invalid(&[0x00, 0x6A], &[], &[0x7F]), None);
        // A correct module still passes.
        assert_eq!(invalid(&[0x20, 0, 0x41, 1, 0x6A], &[0x7F], &[0x7F]), None);
    }
}
