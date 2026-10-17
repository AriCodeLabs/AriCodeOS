// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! Module validation before execution (stage 4, ADR 0019): the typing
//! algorithm from the specification appendix (typed operand stack, control
//! frames, polymorphic unreachable code) restricted to the subset the
//! interpreter executes. A module that does not validate is not
//! instantiated: out-of-range indices, unbalanced stacks and unsupported
//! instructions are rejected here, not halfway through execution.

use super::{Error, Function, Module, Reader, ValType};
use alloc::vec::Vec;

/// Maximum nested blocks inside a function.
const MAX_NESTING: usize = 1024;

/// Type on the validation stack: `None` is "unknown" (after unreachable
/// code), which matches anything.
type Slot = Option<ValType>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Func,
    Block,
    Loop,
    If,
    Else,
}

struct Frame {
    kind: Kind,
    start: Vec<ValType>,
    end: Vec<ValType>,
    height: usize,
    unreachable: bool,
}

struct Validator<'m, 'a> {
    module: &'m Module<'a>,
    /// Type (index) of each function, imported ones first.
    func_types: Vec<u32>,
    locals: Vec<ValType>,
    vals: Vec<Slot>,
    ctrls: Vec<Frame>,
}

fn invalid(what: &'static str) -> Error {
    Error::Invalid(what)
}

impl<'m, 'a> Validator<'m, 'a> {
    fn push_val(&mut self, t: Slot) {
        self.vals.push(t);
    }

    fn pop_val(&mut self) -> Result<Slot, Error> {
        let frame = self.ctrls.last().ok_or(invalid("no frame"))?;
        if self.vals.len() == frame.height {
            return if frame.unreachable {
                Ok(None)
            } else {
                Err(invalid("empty stack"))
            };
        }
        self.vals.pop().ok_or(invalid("empty stack"))
    }

    fn pop_expect(&mut self, expect: ValType) -> Result<(), Error> {
        match self.pop_val()? {
            Some(actual) if actual != expect => Err(invalid("operand type")),
            _ => Ok(()),
        }
    }

    fn pop_vals(&mut self, types: &[ValType]) -> Result<(), Error> {
        for t in types.iter().rev() {
            self.pop_expect(*t)?;
        }
        Ok(())
    }

    fn push_vals(&mut self, types: &[ValType]) {
        for t in types {
            self.push_val(Some(*t));
        }
    }

    fn push_ctrl(
        &mut self,
        kind: Kind,
        start: Vec<ValType>,
        end: Vec<ValType>,
    ) -> Result<(), Error> {
        if self.ctrls.len() >= MAX_NESTING {
            return Err(invalid("nesting"));
        }
        let height = self.vals.len();
        self.push_vals(&start);
        self.ctrls.push(Frame {
            kind,
            start,
            end,
            height,
            unreachable: false,
        });
        Ok(())
    }

    fn pop_ctrl(&mut self) -> Result<Frame, Error> {
        let (end, height) = {
            let frame = self.ctrls.last().ok_or(invalid("end without block"))?;
            (frame.end.clone(), frame.height)
        };
        self.pop_vals(&end)?;
        if self.vals.len() != height {
            return Err(invalid("leftover values at block end"));
        }
        self.ctrls.pop().ok_or(invalid("end without block"))
    }

    fn label_types(&self, depth: u32) -> Result<Vec<ValType>, Error> {
        let idx = self
            .ctrls
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(invalid("label out of range"))?;
        let frame = &self.ctrls[idx];
        Ok(if frame.kind == Kind::Loop {
            frame.start.clone()
        } else {
            frame.end.clone()
        })
    }

    fn unreachable(&mut self) -> Result<(), Error> {
        let frame = self.ctrls.last_mut().ok_or(invalid("no frame"))?;
        self.vals.truncate(frame.height);
        frame.unreachable = true;
        Ok(())
    }

    fn func_type(&self, index: u32) -> Result<&'m super::FuncType, Error> {
        let ti = *self
            .func_types
            .get(index as usize)
            .ok_or(invalid("function out of range"))?;
        self.module
            .types
            .get(ti as usize)
            .ok_or(invalid("type out of range"))
    }

    /// Block type: empty, one result, or a function type by index.
    fn block_type(&self, r: &mut Reader) -> Result<(Vec<ValType>, Vec<ValType>), Error> {
        let b = *r.b.get(r.pos).ok_or(invalid("block type"))?;
        if b == 0x40 {
            r.pos += 1;
            return Ok((Vec::new(), Vec::new()));
        }
        if matches!(b, 0x7C..=0x7F) {
            r.pos += 1;
            return Ok((Vec::new(), alloc::vec![r_valtype(b)?]));
        }
        let ti = r.i32()?;
        let ty = self
            .module
            .types
            .get(ti as usize)
            .ok_or(invalid("block type out of range"))?;
        Ok((ty.params.clone(), ty.results.clone()))
    }

    fn memory(&self) -> Result<(), Error> {
        if self.module.memory_pages.is_none() {
            return Err(invalid("memory instruction without memory"));
        }
        Ok(())
    }

    fn function(&mut self, f: &Function) -> Result<(), Error> {
        let ty = self.func_type(f.index)?;
        let (params, results) = (ty.params.clone(), ty.results.clone());
        self.locals.clear();
        self.locals.extend_from_slice(&params);
        let mut r = Reader::new(self.module.bytes, f.locals_decl);
        for _ in 0..r.u32()? {
            let n = r.u32()?;
            let t = r.valtype()?;
            for _ in 0..n {
                self.locals.push(t);
            }
        }
        if r.pos != f.code_start {
            return Err(invalid("locals declaration"));
        }
        self.vals.clear();
        self.ctrls.clear();
        self.push_ctrl(Kind::Func, Vec::new(), results)?;
        let code = &self.module.bytes[..f.code_end];
        let mut r = Reader::new(code, f.code_start);
        while !self.ctrls.is_empty() {
            let op = r.u8()?;
            self.instruction(op, &mut r)?;
        }
        if r.pos != f.code_end {
            return Err(invalid("code after function end"));
        }
        Ok(())
    }

    fn instruction(&mut self, op: u8, r: &mut Reader) -> Result<(), Error> {
        use ValType::{I32, I64};
        match op {
            0x00 => self.unreachable()?,
            0x01 => {}
            0x02 | 0x03 => {
                let (start, end) = self.block_type(r)?;
                self.pop_vals(&start)?;
                let kind = if op == 0x02 { Kind::Block } else { Kind::Loop };
                self.push_ctrl(kind, start, end)?;
            }
            0x04 => {
                let (start, end) = self.block_type(r)?;
                self.pop_expect(I32)?;
                self.pop_vals(&start)?;
                self.push_ctrl(Kind::If, start, end)?;
            }
            0x05 => {
                let frame = self.pop_ctrl()?;
                if frame.kind != Kind::If {
                    return Err(invalid("else without if"));
                }
                self.push_ctrl(Kind::Else, frame.start, frame.end)?;
            }
            0x0B => {
                let frame = self.pop_ctrl()?;
                if frame.kind == Kind::If && frame.start != frame.end {
                    return Err(invalid("if without else with results"));
                }
                self.push_vals(&frame.end);
            }
            0x0C => {
                let types = self.label_types(r.u32()?)?;
                self.pop_vals(&types)?;
                self.unreachable()?;
            }
            0x0D => {
                let types = self.label_types(r.u32()?)?;
                self.pop_expect(I32)?;
                self.pop_vals(&types)?;
                self.push_vals(&types);
            }
            0x0E => {
                let n = r.u32()?;
                let mut labels = Vec::new();
                for _ in 0..n {
                    labels.push(r.u32()?);
                }
                let default = self.label_types(r.u32()?)?;
                self.pop_expect(I32)?;
                for l in labels {
                    let types = self.label_types(l)?;
                    if types.len() != default.len() {
                        return Err(invalid("br_table with different arities"));
                    }
                    // Every label must accept what is on the stack.
                    self.pop_vals(&types)?;
                    self.push_vals(&types);
                }
                self.pop_vals(&default)?;
                self.unreachable()?;
            }
            0x0F => {
                let end = self.ctrls[0].end.clone();
                self.pop_vals(&end)?;
                self.unreachable()?;
            }
            0x10 => {
                let ty = self.func_type(r.u32()?)?;
                let (params, results) = (ty.params.clone(), ty.results.clone());
                self.pop_vals(&params)?;
                self.push_vals(&results);
            }
            0x11 => {
                let ti = r.u32()?;
                if r.u8()? != 0 {
                    return Err(invalid("call_indirect: table"));
                }
                if self.module.table.is_empty() {
                    return Err(invalid("call_indirect without table"));
                }
                let ty = self
                    .module
                    .types
                    .get(ti as usize)
                    .ok_or(invalid("type out of range"))?;
                let (params, results) = (ty.params.clone(), ty.results.clone());
                self.pop_expect(I32)?;
                self.pop_vals(&params)?;
                self.push_vals(&results);
            }
            0x1A => {
                self.pop_val()?;
            }
            0x1B => {
                self.pop_expect(I32)?;
                let a = self.pop_val()?;
                let b = self.pop_val()?;
                let t = match (a, b) {
                    (Some(x), Some(y)) if x != y => {
                        return Err(invalid("select with different types"))
                    }
                    (Some(x), _) | (_, Some(x)) => Some(x),
                    (None, None) => None,
                };
                self.push_val(t);
            }
            0x1C => {
                if r.u32()? != 1 {
                    return Err(invalid("select with several types"));
                }
                let t = r.valtype()?;
                self.pop_expect(I32)?;
                self.pop_expect(t)?;
                self.pop_expect(t)?;
                self.push_val(Some(t));
            }
            0x20..=0x22 => {
                let t = *self
                    .locals
                    .get(r.u32()? as usize)
                    .ok_or(invalid("local out of range"))?;
                match op {
                    0x20 => self.push_val(Some(t)),
                    0x21 => self.pop_expect(t)?,
                    _ => {
                        self.pop_expect(t)?;
                        self.push_val(Some(t));
                    }
                }
            }
            0x23 | 0x24 => {
                let g = *self
                    .module
                    .globals
                    .get(r.u32()? as usize)
                    .ok_or(invalid("global out of range"))?;
                if op == 0x23 {
                    self.push_val(Some(g.ty));
                } else {
                    if !g.mutable {
                        return Err(invalid("global.set on an immutable global"));
                    }
                    self.pop_expect(g.ty)?;
                }
            }
            0x28..=0x35 => {
                self.memory()?;
                r.u32()?;
                r.u32()?;
                let t = match op {
                    0x2A | 0x2B => return Err(Error::Unsupported("floating point")),
                    0x28 | 0x2C..=0x2F => I32,
                    _ => I64,
                };
                self.pop_expect(I32)?;
                self.push_val(Some(t));
            }
            0x36..=0x3E => {
                self.memory()?;
                r.u32()?;
                r.u32()?;
                let t = match op {
                    0x38 | 0x39 => return Err(Error::Unsupported("floating point")),
                    0x36 | 0x3A | 0x3B => I32,
                    _ => I64,
                };
                self.pop_expect(t)?;
                self.pop_expect(I32)?;
            }
            0x3F => {
                self.memory()?;
                r.u8()?;
                self.push_val(Some(I32));
            }
            0x40 => {
                self.memory()?;
                r.u8()?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0x41 => {
                r.i32()?;
                self.push_val(Some(I32));
            }
            0x42 => {
                r.i64()?;
                self.push_val(Some(I64));
            }
            0x43 | 0x44 => return Err(Error::Unsupported("floating point")),
            0x45 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0x46..=0x4F => {
                self.pop_expect(I32)?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0x50 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I32));
            }
            0x51..=0x5A => {
                self.pop_expect(I64)?;
                self.pop_expect(I64)?;
                self.push_val(Some(I32));
            }
            0x67..=0x69 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0x6A..=0x78 => {
                self.pop_expect(I32)?;
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0x79..=0x7B => {
                self.pop_expect(I64)?;
                self.push_val(Some(I64));
            }
            0x7C..=0x8A => {
                self.pop_expect(I64)?;
                self.pop_expect(I64)?;
                self.push_val(Some(I64));
            }
            0xA7 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I32));
            }
            0xAC | 0xAD => {
                self.pop_expect(I32)?;
                self.push_val(Some(I64));
            }
            0xC0 | 0xC1 => {
                self.pop_expect(I32)?;
                self.push_val(Some(I32));
            }
            0xC2..=0xC4 => {
                self.pop_expect(I64)?;
                self.push_val(Some(I64));
            }
            0xFC => match r.u32()? {
                10 => {
                    self.memory()?;
                    r.u8()?;
                    r.u8()?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                }
                11 => {
                    self.memory()?;
                    r.u8()?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                    self.pop_expect(I32)?;
                }
                _ => return Err(Error::Unsupported("0xFC prefix")),
            },
            0x5B..=0x66 | 0x8B..=0xA6 | 0xA8..=0xAB | 0xAE..=0xBF => {
                return Err(Error::Unsupported("floating point"))
            }
            _ => return Err(Error::Unsupported("opcode")),
        }
        Ok(())
    }
}

fn r_valtype(b: u8) -> Result<ValType, Error> {
    Ok(match b {
        0x7F => ValType::I32,
        0x7E => ValType::I64,
        0x7D => ValType::F32,
        0x7C => ValType::F64,
        _ => return Err(invalid("value type")),
    })
}

/// Validates the whole module: indices of types, functions, table, globals,
/// exports and `start`, and the body of each function with the typing
/// algorithm. `Module::parse` calls it before returning the module.
pub fn validate(module: &Module) -> Result<(), Error> {
    let mut func_types = Vec::new();
    for imp in &module.imports {
        if module.types.get(imp.type_index as usize).is_none() {
            return Err(invalid("import with type out of range"));
        }
        func_types.push(imp.type_index);
    }
    for f in &module.functions {
        if module.types.get(f.type_index as usize).is_none() {
            return Err(invalid("function with type out of range"));
        }
        func_types.push(f.type_index);
    }
    let total = func_types.len() as u32;
    for entry in module.table.iter().flatten() {
        if *entry >= total {
            return Err(invalid("table element out of range"));
        }
    }
    for e in &module.exports {
        let ok = match e.kind {
            0 => e.index < total,
            1 => module.table_limits.is_some() && e.index == 0,
            2 => module.memory_pages.is_some() && e.index == 0,
            3 => (e.index as usize) < module.globals.len(),
            _ => false,
        };
        if !ok {
            return Err(invalid("export out of range"));
        }
    }
    if let Some((min, max)) = module.memory_pages {
        if min > 65536 || max.is_some_and(|max| max < min || max > 65536) {
            return Err(invalid("memory limits"));
        }
    }
    if let Some((min, max)) = module.table_limits {
        if max.is_some_and(|max| max < min) {
            return Err(invalid("table limits"));
        }
    }
    if let Some(s) = module.start {
        let ti = *func_types
            .get(s as usize)
            .ok_or(invalid("start out of range"))?;
        let ty = &module.types[ti as usize];
        if !ty.params.is_empty() || !ty.results.is_empty() {
            return Err(invalid("start with params or results"));
        }
    }
    for g in &module.globals {
        if matches!(g.ty, ValType::F32 | ValType::F64) {
            return Err(Error::Unsupported("floating point"));
        }
        if g.init_ty != g.ty {
            return Err(invalid("global initializer type"));
        }
    }
    let mut v = Validator {
        module,
        func_types,
        locals: Vec::new(),
        vals: Vec::new(),
        ctrls: Vec::new(),
    };
    for (i, f) in module.functions.iter().enumerate() {
        let f = Function {
            index: module.imports.len() as u32 + i as u32,
            ..*f
        };
        v.function(&f)?;
    }
    Ok(())
}
