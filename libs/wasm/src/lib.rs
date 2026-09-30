//! Intérprete WebAssembly (Fase 29): el subconjunto MVP con enteros que
//! producen `rustc` (`-C target-cpu=mvp`) y otros compiladores para módulos
//! `no_std`: control estructurado, locales y globales, memoria lineal con
//! límites comprobados, llamadas directas e indirectas e importaciones de
//! funciones. Sin coma flotante (traps con `Unsupported`), sin SIMD.
//!
//! El módulo se analiza una vez ([`Module::parse`]); la ejecución
//! ([`Instance::call`]) recorre el código byte a byte con una pila de valores
//! y una de etiquetas acotadas. Toda operación sobre memoria comprueba
//! límites: un módulo hostil solo puede fallar, nunca salir de su memoria.
//! La memoria lineal la aporta el anfitrión como un `&mut [u8]` (en el
//! sistema, un buffer DMA acotado por la cuota de la aplicación).

#![no_std]

extern crate alloc;

mod validate;

use alloc::vec::Vec;
pub use validate::validate;

pub const PAGE: usize = 65536;
/// Profundidad máxima de llamadas anidadas.
pub const MAX_CALL_DEPTH: usize = 64;
/// Valores en la pila de operandos, en total.
pub const MAX_STACK: usize = 16 * 1024;
/// Pasos de ejecución por llamada a `call` (contra bucles infinitos).
pub const MAX_STEPS: u64 = 50_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Módulo malformado o con secciones no soportadas.
    Malformed(&'static str),
    /// Función, tipo o índice fuera de rango.
    OutOfRange,
    Unsupported(&'static str),
    /// Trap en ejecución: `unreachable`, división por cero, acceso fuera de memoria...
    Trap(&'static str),
    StackOverflow,
    /// Demasiados pasos.
    Fuel,
    /// Una importación no tiene anfitrión.
    MissingImport,
    /// El módulo no valida (tipado, índices, pilas): se rechaza antes de
    /// instanciarlo (ADR 0019).
    Invalid(&'static str),
    /// Exportación no encontrada.
    NoExport,
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

/// Valor en la pila: los tipos se comprueban por construcción del módulo;
/// aquí todo es un `u64` (i32 con extensión de ceros, i64, bits de f32/f64).
pub type Value = u64;

#[derive(Debug, Clone)]
pub struct Import {
    pub module: Vec<u8>,
    pub name: Vec<u8>,
    pub type_index: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Export {
    pub name_start: usize,
    pub name_len: usize,
    pub kind: u8,
    pub index: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Function {
    /// Índice de la función en el espacio de funciones (importadas primero).
    pub index: u32,
    pub type_index: u32,
    /// Locales declaradas (sin parámetros).
    pub locals: u32,
    /// Posición de la declaración de locales (tipos) dentro del módulo.
    pub locals_decl: usize,
    /// Rango del cuerpo (instrucciones) dentro del módulo.
    pub code_start: usize,
    pub code_end: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Global {
    pub ty: ValType,
    pub mutable: bool,
    pub init: Value,
    /// Tipo de la constante del inicializador (debe coincidir con `ty`).
    pub init_ty: ValType,
}

#[derive(Debug, Clone, Copy)]
pub struct DataSegment {
    pub offset: u32,
    pub start: usize,
    pub len: usize,
}

/// Módulo analizado; conserva una referencia a los bytes originales.
pub struct Module<'a> {
    bytes: &'a [u8],
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    pub functions: Vec<Function>,
    pub table: Vec<Option<u32>>,
    /// Límites declarados de la tabla (mínimo, máximo), si hay tabla.
    pub table_limits: Option<(u32, Option<u32>)>,
    pub memory_pages: Option<(u32, Option<u32>)>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub data: Vec<DataSegment>,
    pub start: Option<u32>,
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
        let v = *self
            .b
            .get(self.pos)
            .ok_or(Error::Malformed("fin inesperado"))?;
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
        let s = self
            .b
            .get(
                self.pos
                    ..self
                        .pos
                        .checked_add(n)
                        .ok_or(Error::Malformed("longitud"))?,
            )
            .ok_or(Error::Malformed("fin inesperado"))?;
        self.pos += n;
        Ok(s)
    }
    fn valtype(&mut self) -> Result<ValType, Error> {
        Ok(match self.u8()? {
            0x7F => ValType::I32,
            0x7E => ValType::I64,
            0x7D => ValType::F32,
            0x7C => ValType::F64,
            _ => return Err(Error::Malformed("tipo de valor")),
        })
    }
}

/// Evalúa una expresión constante de inicialización (`i32.const`, `i64.const`).
/// Expresión constante con el tipo de su constante (para validar
/// inicializadores de globales y desplazamientos de segmentos).
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
        _ => return Err(Error::Unsupported("expresion constante")),
    };
    if r.u8()? != 0x0B {
        return Err(Error::Malformed("fin de expresion constante"));
    }
    Ok(v)
}

impl<'a> Module<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" || bytes[4..8] != [1, 0, 0, 0] {
            return Err(Error::Malformed("cabecera"));
        }
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
        };
        let mut func_types: Vec<u32> = Vec::new();
        let mut r = Reader::new(bytes, 8);
        while r.pos < bytes.len() {
            let id = r.u8()?;
            let size = r.u32()? as usize;
            let start = r.pos;
            let end = start.checked_add(size).ok_or(Error::Malformed("seccion"))?;
            if end > bytes.len() {
                return Err(Error::Malformed("seccion truncada"));
            }
            let mut s = Reader::new(bytes, start);
            match id {
                0 => {} // custom
                1 => {
                    for _ in 0..s.u32()? {
                        if s.u8()? != 0x60 {
                            return Err(Error::Malformed("tipo de funcion"));
                        }
                        let np = s.u32()?;
                        let mut params = Vec::new();
                        for _ in 0..np {
                            params.push(s.valtype()?);
                        }
                        let nr = s.u32()?;
                        let mut results = Vec::new();
                        for _ in 0..nr {
                            results.push(s.valtype()?);
                        }
                        m.types.push(FuncType { params, results });
                    }
                }
                2 => {
                    for _ in 0..s.u32()? {
                        let ml = s.u32()? as usize;
                        let module = s.bytes(ml)?.to_vec();
                        let nl = s.u32()? as usize;
                        let name = s.bytes(nl)?.to_vec();
                        match s.u8()? {
                            0 => {
                                let type_index = s.u32()?;
                                m.imports.push(Import {
                                    module,
                                    name,
                                    type_index,
                                });
                            }
                            _ => return Err(Error::Unsupported("importacion no funcional")),
                        }
                    }
                }
                3 => {
                    for _ in 0..s.u32()? {
                        func_types.push(s.u32()?);
                    }
                }
                4 => {
                    for _ in 0..s.u32()? {
                        if s.u8()? != 0x70 {
                            return Err(Error::Malformed("tipo de tabla"));
                        }
                        let flags = s.u8()?;
                        let min = s.u32()?;
                        let max = if flags & 1 != 0 { Some(s.u32()?) } else { None };
                        if min > 4096 {
                            return Err(Error::Unsupported("tabla grande"));
                        }
                        m.table = alloc::vec![None; min as usize];
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
                        m.globals.push(Global {
                            ty,
                            mutable,
                            init,
                            init_ty,
                        });
                    }
                }
                7 => {
                    for _ in 0..s.u32()? {
                        let name_len = s.u32()? as usize;
                        let name_start = s.pos;
                        s.bytes(name_len)?;
                        let kind = s.u8()?;
                        let index = s.u32()?;
                        m.exports.push(Export {
                            name_start,
                            name_len,
                            kind,
                            index,
                        });
                    }
                }
                8 => m.start = Some(s.u32()?),
                9 => {
                    for _ in 0..s.u32()? {
                        let flags = s.u32()?;
                        if flags != 0 {
                            return Err(Error::Unsupported("segmento de elementos"));
                        }
                        let (offset_ty, offset) = const_expr_typed(&mut s)?;
                        if offset_ty != ValType::I32 {
                            return Err(Error::Invalid("desplazamiento de elementos no i32"));
                        }
                        let offset = offset as usize;
                        let n = s.u32()? as usize;
                        for i in 0..n {
                            let f = s.u32()?;
                            let slot = m
                                .table
                                .get_mut(offset + i)
                                .ok_or(Error::Malformed("elemento fuera de la tabla"))?;
                            *slot = Some(f);
                        }
                    }
                }
                10 => {
                    let count = s.u32()? as usize;
                    if count != func_types.len() {
                        return Err(Error::Malformed("cuerpos != funciones"));
                    }
                    for (i, type_index) in func_types.iter().copied().enumerate() {
                        let body_size = s.u32()? as usize;
                        let body_start = s.pos;
                        let index = m.imports.len() as u32 + i as u32;
                        let body_end = body_start
                            .checked_add(body_size)
                            .ok_or(Error::Malformed("cuerpo"))?;
                        if body_end > end {
                            return Err(Error::Malformed("cuerpo truncado"));
                        }
                        let mut locals = 0u32;
                        for _ in 0..s.u32()? {
                            let n = s.u32()?;
                            s.valtype()?;
                            locals = locals.checked_add(n).ok_or(Error::Malformed("locales"))?;
                        }
                        if locals > 4096 {
                            return Err(Error::Unsupported("demasiadas locales"));
                        }
                        m.functions.push(Function {
                            index,
                            type_index,
                            locals,
                            locals_decl: body_start,
                            code_start: s.pos,
                            code_end: body_end,
                        });
                        s.pos = body_end;
                    }
                }
                11 => {
                    for _ in 0..s.u32()? {
                        let flags = s.u32()?;
                        if flags != 0 {
                            return Err(Error::Unsupported("segmento de datos pasivo"));
                        }
                        let (offset_ty, offset) = const_expr_typed(&mut s)?;
                        if offset_ty != ValType::I32 {
                            return Err(Error::Invalid("desplazamiento de datos no i32"));
                        }
                        let offset = offset as u32;
                        let len = s.u32()? as usize;
                        let start = s.pos;
                        s.bytes(len)?;
                        m.data.push(DataSegment { offset, start, len });
                    }
                }
                12 => {} // data count
                _ => return Err(Error::Malformed("seccion desconocida")),
            }
            r.pos = end;
        }
        if func_types.len() != m.functions.len() {
            return Err(Error::Malformed("faltan cuerpos"));
        }
        validate(&m)?;
        Ok(m)
    }

    pub fn export_name(&self, e: &Export) -> &'a [u8] {
        &self.bytes[e.name_start..e.name_start + e.name_len]
    }

    /// Índice de función exportada con ese nombre.
    pub fn exported_function(&self, name: &str) -> Option<u32> {
        self.exports
            .iter()
            .find(|e| e.kind == 0 && self.export_name(e) == name.as_bytes())
            .map(|e| e.index)
    }

    /// Tipo de la función `index` (importadas primero).
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

    /// Páginas iniciales de memoria declaradas.
    pub fn initial_pages(&self) -> u32 {
        self.memory_pages.map(|(min, _)| min).unwrap_or(0)
    }
}

/// Funciones que el anfitrión ofrece al módulo. `call(índice de importación,
/// argumentos, memoria)` devuelve el resultado (o 0 si no hay).
pub trait Host {
    fn call(&mut self, import: usize, args: &[Value], memory: &mut [u8]) -> Result<Value, Error>;
}

struct Label {
    /// Posición de la instrucción siguiente al `end` (o del `else`).
    end: usize,
    /// `loop`: saltar al inicio.
    loop_start: Option<usize>,
    /// Altura de la pila al entrar (sin los parámetros del bloque).
    height: usize,
    /// Valores que deja el bloque al salir por `br`.
    arity: usize,
    /// Parámetros del bloque: lo que conserva un `br` a un `loop`
    /// (auditoría 6ee19fb, observación).
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

/// Instancia: módulo más estado mutable (globales, memoria del anfitrión).
pub struct Instance<'m, 'a> {
    module: &'m Module<'a>,
    globals: Vec<Value>,
    stack: Vec<Value>,
    labels: Vec<Label>,
    /// Fin de cada bloque, calculado la primera vez que se entra.
    memory_pages: u32,
}

impl<'m, 'a> Instance<'m, 'a> {
    /// Crea la instancia y copia los segmentos de datos en `memory`.
    pub fn new(module: &'m Module<'a>, memory: &mut [u8]) -> Result<Self, Error> {
        let pages = module.initial_pages();
        let active = (pages as usize) * PAGE;
        if active > memory.len() {
            return Err(Error::Trap("memoria insuficiente para el modulo"));
        }
        // La memoria lineal nace a cero aunque el buffer del anfitrion traiga
        // restos; los segmentos de datos deben caer dentro de las paginas
        // iniciales, no del buffer entero.
        memory[..active].fill(0);
        for d in &module.data {
            let end = (d.offset as usize)
                .checked_add(d.len)
                .ok_or(Error::Trap("segmento de datos fuera de memoria"))?;
            let dst = memory[..active]
                .get_mut(d.offset as usize..end)
                .ok_or(Error::Trap("segmento de datos fuera de memoria"))?;
            dst.copy_from_slice(&module.bytes[d.start..d.start + d.len]);
        }
        Ok(Self {
            module,
            globals: module.globals.iter().map(|g| g.init).collect(),
            stack: Vec::new(),
            labels: Vec::new(),
            memory_pages: pages,
        })
    }

    pub fn global(&self, index: usize) -> Option<Value> {
        self.globals.get(index).copied()
    }

    /// Bytes de memoria lineal actualmente válidos (`memory_pages * PAGE`).
    pub fn memory_len(&self) -> usize {
        self.memory_pages as usize * PAGE
    }

    /// Ejecuta la función `start` del módulo si la declara. Debe llamarse una
    /// vez, tras `new` y antes de cualquier exportación; propaga sus traps.
    pub fn start(&mut self, memory: &mut [u8], host: &mut dyn Host) -> Result<(), Error> {
        match self.module.start {
            Some(index) => self.invoke(index, &[], memory, host).map(|_| ()),
            None => Ok(()),
        }
    }

    /// Llama a la función exportada `name` con `args`; devuelve su primer
    /// resultado (0 si no tiene).
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
        let ty = self.module.function_type(index)?;
        if ty.params.len() != args.len() {
            return Err(Error::OutOfRange);
        }
        let results = ty.results.len();
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
        self.stack.push(v);
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, Error> {
        self.stack.pop().ok_or(Error::Trap("pila vacia"))
    }

    fn pop32(&mut self) -> Result<u32, Error> {
        self.pop().map(|v| v as u32)
    }

    /// Salta al final del bloque que empieza en `pc` (tras su opcode y tipo)
    /// devolviendo (posición del `else` si lo hay, posición tras `end`).
    fn find_block_end(
        &self,
        func: &Function,
        mut pc: usize,
    ) -> Result<(Option<usize>, usize), Error> {
        let code = &self.module.bytes[..func.code_end];
        let mut depth = 0usize;
        let mut else_pos = None;
        while pc < code.len() {
            let op = code[pc];
            pc += 1;
            let mut r = Reader::new(code, pc);
            match op {
                0x02..=0x04 => {
                    skip_blocktype(&mut r)?;
                    depth += 1;
                }
                0x05 => {
                    if depth == 0 {
                        else_pos = Some(r.pos);
                    }
                }
                0x0B => {
                    if depth == 0 {
                        return Ok((else_pos, r.pos));
                    }
                    depth -= 1;
                }
                _ => skip_immediates(op, &mut r)?,
            }
            pc = r.pos;
        }
        Err(Error::Malformed("bloque sin fin"))
    }

    fn branch(&mut self, depth: u32, frame_labels_base: usize) -> Result<Option<usize>, Error> {
        let idx = self
            .labels
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(Error::Malformed("etiqueta"))?;
        if idx < frame_labels_base {
            return Err(Error::Malformed("etiqueta de otro marco"));
        }
        let (target, height, arity, is_loop, params) = {
            let l = &self.labels[idx];
            (
                l.loop_start.unwrap_or(l.end),
                l.height,
                l.arity,
                l.loop_start.is_some(),
                l.params,
            )
        };
        // Conservar los valores de salida del bloque (o los parámetros del
        // `loop`, que vuelven a entrar) y descartar el resto.
        let keep = if is_loop { params } else { arity };
        let len = self.stack.len();
        if len < height + keep {
            return Err(Error::Trap("pila insuficiente en br"));
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
        let mut locals: Vec<Value> = Vec::new();
        let mut steps = 0u64;

        // Entrar en una función: parámetros ya en la pila.
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
                return Err(Error::Trap("faltan argumentos"));
            }
            let locals_base = locals.len();
            let args_start = this.stack.len() - nparams;
            locals.extend_from_slice(&this.stack[args_start..]);
            this.stack.truncate(args_start);
            locals.resize(locals_base + nparams + f.locals as usize, 0);
            let labels_base = this.labels.len();
            // Etiqueta implícita de la función: `br` a ella es `return`.
            this.labels.push(Label {
                end: f.code_end,
                loop_start: None,
                height: this.stack.len(),
                arity: ty.results.len(),
                params: 0,
            });
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
                return Err(Error::Trap("faltan argumentos"));
            }
            let args: Vec<Value> = this.stack.split_off(this.stack.len() - n);
            let active = this.memory_len().min(memory.len());
            let result = host.call(import, &args, &mut memory[..active])?;
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
                // Fin de función (sin `end` explícito): retorno.
                let fr = frames.pop().unwrap();
                self.finish_frame(&fr, &mut locals)?;
                continue;
            }
            steps += 1;
            if steps > MAX_STEPS {
                return Err(Error::Fuel);
            }
            let op = code[frame.pc];
            let mut r = Reader::new(code, frame.pc + 1);
            let frame_labels_base = frame.labels_base;
            let locals_base = frame.locals_base;
            let mut next_pc: Option<usize> = None;
            match op {
                0x00 => return Err(Error::Trap("unreachable")),
                0x01 => {}
                0x02 | 0x03 => {
                    let arity = block_arity(&mut r, module)?;
                    let (_, end) = self.find_block_end(&func, r.pos)?;
                    let start = r.pos;
                    self.labels.push(Label {
                        end,
                        loop_start: (op == 0x03).then_some(start),
                        height: self.stack.len() - arity.0,
                        arity: arity.1,
                        params: arity.0,
                    });
                }
                0x04 => {
                    let arity = block_arity(&mut r, module)?;
                    let (else_pos, end) = self.find_block_end(&func, r.pos)?;
                    let cond = self.pop32()?;
                    self.labels.push(Label {
                        end,
                        loop_start: None,
                        height: self.stack.len() - arity.0,
                        arity: arity.1,
                        params: arity.0,
                    });
                    if cond == 0 {
                        match else_pos {
                            Some(p) => next_pc = Some(p),
                            None => {
                                self.labels.pop();
                                next_pc = Some(end);
                            }
                        }
                    }
                }
                0x05 => {
                    // Fin de la rama `then`: saltar al `end` del bloque.
                    let l = self
                        .labels
                        .pop()
                        .ok_or(Error::Malformed("else sin bloque"))?;
                    next_pc = Some(l.end);
                }
                0x0B => {
                    if self.labels.len() == frame_labels_base + 1 {
                        // `end` de la función: retorno.
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
                    let n = r.u32()? as usize;
                    let mut chosen = None;
                    let idx = self.pop32()? as usize;
                    for i in 0..n {
                        let d = r.u32()?;
                        if i == idx {
                            chosen = Some(d);
                        }
                    }
                    let default = r.u32()?;
                    let depth = chosen.unwrap_or(default);
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
                    r.u8()?; // tabla 0
                    let elem = self.pop32()? as usize;
                    let index = module
                        .table
                        .get(elem)
                        .copied()
                        .flatten()
                        .ok_or(Error::Trap("call_indirect: entrada vacia"))?;
                    let expected = module
                        .types
                        .get(type_index as usize)
                        .ok_or(Error::OutOfRange)?;
                    if module.function_type(index)? != expected {
                        return Err(Error::Trap("call_indirect: tipo distinto"));
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
                    let v = *self.stack.last().ok_or(Error::Trap("pila vacia"))?;
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
                        // Las paginas nuevas nacen a cero.
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
                0x43 | 0x44 => return Err(Error::Unsupported("coma flotante")),
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
                            return Err(Error::Trap("acceso fuera de memoria"));
                        }
                        memory.copy_within(src..end_src, dst);
                    }
                    11 => {
                        r.u8()?;
                        let n = self.pop32()? as usize;
                        let v = self.pop32()? as u8;
                        let dst = self.pop32()? as usize;
                        let end = dst.checked_add(n).ok_or(Error::Trap("memory.fill"))?;
                        if end > self.memory_len() {
                            return Err(Error::Trap("acceso fuera de memoria"));
                        }
                        memory[dst..end].fill(v);
                    }
                    _ => return Err(Error::Unsupported("prefijo 0xFC")),
                },
                _ => return Err(Error::Unsupported("opcode")),
            }
            let frame = frames.last_mut().unwrap();
            frame.pc = next_pc.unwrap_or(r.pos);
        }
    }

    /// `br` que puede ser un retorno de función (etiqueta implícita).
    fn branch_or_return(
        &mut self,
        depth: u32,
        frame_labels_base: usize,
    ) -> Result<Option<usize>, Error> {
        let idx = self
            .labels
            .len()
            .checked_sub(1 + depth as usize)
            .ok_or(Error::Malformed("etiqueta"))?;
        if idx == frame_labels_base {
            return Ok(None); // retorno: `finish_frame` recorta la pila
        }
        self.branch(depth, frame_labels_base)
    }

    /// Salida de una función: deja `results` valores sobre `stack_base` y
    /// libera locales y etiquetas del marco.
    fn finish_frame(&mut self, fr: &Frame, locals: &mut Vec<Value>) -> Result<(), Error> {
        let len = self.stack.len();
        if len < fr.stack_base + fr.results {
            return Err(Error::Trap("resultados insuficientes"));
        }
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
                    return Err(Error::Trap("division por cero"));
                }
                sa.checked_div(sb)
                    .ok_or(Error::Trap("desbordamiento en division"))? as u32 as u64
            }
            0x6E => {
                if b == 0 {
                    return Err(Error::Trap("division por cero"));
                }
                (a / b) as u64
            }
            0x6F => {
                if sb == 0 {
                    return Err(Error::Trap("division por cero"));
                }
                sa.wrapping_rem(sb) as u32 as u64
            }
            0x70 => {
                if b == 0 {
                    return Err(Error::Trap("division por cero"));
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
                    return Err(Error::Trap("division por cero"));
                }
                sa.checked_div(sb)
                    .ok_or(Error::Trap("desbordamiento en division"))? as u64
            }
            0x80 => {
                if b == 0 {
                    return Err(Error::Trap("division por cero"));
                }
                a / b
            }
            0x81 => {
                if sb == 0 {
                    return Err(Error::Trap("division por cero"));
                }
                sa.wrapping_rem(sb) as u64
            }
            0x82 => {
                if b == 0 {
                    return Err(Error::Trap("division por cero"));
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

/// Aridad (parámetros, resultados) de un tipo de bloque.
fn block_arity(r: &mut Reader, module: &Module) -> Result<(usize, usize), Error> {
    let b = *r.b.get(r.pos).ok_or(Error::Malformed("tipo de bloque"))?;
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
    let b = *r.b.get(r.pos).ok_or(Error::Malformed("tipo de bloque"))?;
    if b == 0x40 || matches!(b, 0x7C..=0x7F) {
        r.pos += 1;
    } else {
        r.i32()?;
    }
    Ok(())
}

/// Salta los inmediatos de una instrucción (para localizar fines de bloque).
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
            _ => return Err(Error::Unsupported("prefijo 0xFC")),
        },
        _ => {}
    }
    Ok(())
}

fn effective(base: u32, offset: u32, size: usize, memory: &[u8]) -> Result<usize, Error> {
    let addr = base as u64 + offset as u64;
    let end = addr + size as u64;
    if end > memory.len() as u64 {
        return Err(Error::Trap("acceso fuera de memoria"));
    }
    Ok(addr as usize)
}

fn load(memory: &[u8], base: u32, offset: u32, op: u8) -> Result<Value, Error> {
    let size = match op {
        0x28 | 0x34 | 0x35 => 4,
        0x29 => 8,
        0x2C | 0x2D | 0x30 | 0x31 => 1,
        0x2E | 0x2F | 0x32 | 0x33 => 2,
        _ => return Err(Error::Unsupported("carga en coma flotante")),
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
        _ => return Err(Error::Unsupported("almacenamiento en coma flotante")),
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

    /// Ensamblador mínimo para los tests: un módulo con tipos, importaciones,
    /// funciones, memoria de una página, exportaciones y datos.
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
            section(5, &[1, 0, 1], &mut out); // memoria: 1 pagina
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

    // Codigos: 0x20 local.get, 0x41 i32.const, 0x6A add, 0x6C mul, 0x6B sub

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
            Err(Error::Trap("acceso fuera de memoria"))
        );
        // Los datos iniciales llegan a memoria.
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
        let recurse = b.func(t, 0, &[0x20, 0, 0x10, 3]); // se llama a si misma (indice 3)
        b.export("recurse", recurse);
        let wasm = b.build();
        assert_eq!(
            run(&wasm, "div", &[0]),
            Err(Error::Trap("division por cero"))
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
        // indirecta: call_indirect tabla[a] con tipo t, argumento 1
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
            Err(Error::Trap("call_indirect: entrada vacia"))
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
        // Relleno fuera de la pagina: trap, sin panico.
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
            Err(Error::Trap("acceso fuera de memoria"))
        );
    }

    #[test]
    fn memory_is_bounded_by_current_pages_not_host_buffer() {
        // Modulo de 1 pagina (max 1) sobre un buffer de 4: leer en 65536 es trap.
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
            Err(Error::Trap("acceso fuera de memoria"))
        );
        assert_eq!(
            inst.call("st", &[PAGE as u64], &mut mem, &mut NoHost),
            Err(Error::Trap("acceso fuera de memoria"))
        );
        assert_eq!(
            inst.call("ld", &[(PAGE - 1) as u64], &mut mem, &mut NoHost),
            Ok(0)
        );
        // El anfitrion tampoco ve mas alla de la memoria activa.
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
        // grow 1 pagina y leer el primer byte de la nueva.
        let grow = b.func(t, 0, &[0x41, 1, 0x40, 0, 0x1A, 0x41, 0, 0x2D, 0, 0]);
        b.export("grow", grow);
        b.data.push((8, vec![1, 2, 3]));
        let wasm = b.build();
        // El constructor de pruebas declara memoria {min 1} sin maximo.
        let m = Module::parse(&wasm).unwrap();
        let mut mem = vec![0xAAu8; 4 * PAGE];
        let mut inst = Instance::new(&m, &mut mem).unwrap();
        assert_eq!(mem[0], 0, "memoria inicial a cero");
        assert_eq!(&mem[8..11], &[1, 2, 3], "datos aplicados");
        assert_eq!(inst.call("ld0", &[], &mut mem, &mut NoHost), Ok(0));
        // Tras crecer, el byte 65536 (antes 0xAA en el buffer) debe ser 0.
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
            "sin start aun"
        );
        inst.start(&mut mem, &mut NoHost).unwrap();
        assert_eq!(inst.call("get", &[], &mut mem, &mut NoHost), Ok(42));
        // Un start que hace trap se propaga.
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
            Some(Error::Malformed("cabecera"))
        );
        let mut b = Builder::new();
        let t = b.ty(&[], &[0x7F]);
        b.func(t, 0, &[0x43, 0, 0, 0, 0]); // f32.const
        b.export("f", 0);
        assert_eq!(
            run(&b.build(), "f", &[]),
            Err(Error::Unsupported("coma flotante"))
        );
        let mut truncated = b.build();
        truncated.truncate(truncated.len() - 3);
        assert!(Module::parse(&truncated).is_err());
    }

    /// Módulos mínimos de la auditoría 6ee19fb (hallazgo 6 y observación),
    /// contrastados con `WebAssembly.validate` de Node: tres inválidos y un
    /// `loop` con parámetro que debe devolver 0.
    #[test]
    fn audit_fixtures_structural_validation_and_loop_params() {
        // global i32 inicializada con i64.const
        let wrong_global = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x06, 0x06, 0x01, 0x7F, 0x00, 0x42,
            0x01, 0x0B,
        ];
        assert_eq!(
            Module::parse(&wrong_global).err(),
            Some(Error::Invalid("tipo del inicializador de global"))
        );
        // exportación de memoria 99 sin memoria
        let missing_memory = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x07, 0x05, 0x01, 0x01, 0x6D, 0x02,
            0x63,
        ];
        assert_eq!(
            Module::parse(&missing_memory).err(),
            Some(Error::Invalid("exportacion fuera de rango"))
        );
        // memoria con mínimo 2 y máximo 1
        let inverted_limits = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x05, 0x04, 0x01, 0x01, 0x02, 0x01,
        ];
        assert_eq!(
            Module::parse(&inverted_limits).err(),
            Some(Error::Invalid("limites de memoria"))
        );
        // loop (param i32) con br_if: cuenta atrás desde 2 y devuelve 0
        let loop_params = [
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, 0x01, 0x0A, 0x02, 0x60, 0x00, 0x01,
            0x7F, 0x60, 0x01, 0x7F, 0x01, 0x7F, 0x03, 0x02, 0x01, 0x00, 0x07, 0x08, 0x01, 0x04,
            0x6D, 0x61, 0x69, 0x6E, 0x00, 0x00, 0x0A, 0x16, 0x01, 0x14, 0x01, 0x01, 0x7F, 0x41,
            0x02, 0x03, 0x01, 0x22, 0x00, 0x41, 0x01, 0x6B, 0x22, 0x00, 0x20, 0x00, 0x0D, 0x00,
            0x0B, 0x0B,
        ];
        assert_eq!(run(&loop_params, "main", &[]), Ok(0));
    }

    /// El validador rechaza antes de ejecutar lo que el intérprete solo
    /// descubriría a mitad de camino (ADR 0019).
    #[test]
    fn validator_rejects_ill_typed_modules() {
        fn invalid(code: &[u8], params: &[u8], results: &[u8]) -> Option<Error> {
            let mut b = Builder::new();
            let t = b.ty(params, results);
            let f = b.func(t, 1, code);
            b.export("f", f);
            Module::parse(&b.build()).err()
        }
        // Pila vacia al sumar.
        assert_eq!(
            invalid(&[0x41, 1, 0x6A], &[], &[0x7F]),
            Some(Error::Invalid("pila vacia"))
        );
        // i64 donde se espera i32.
        assert_eq!(
            invalid(&[0x42, 1, 0x41, 2, 0x6A], &[], &[0x7F]),
            Some(Error::Invalid("tipo de operando"))
        );
        // Valor sobrante al terminar la funcion.
        assert_eq!(
            invalid(&[0x41, 1, 0x41, 2], &[], &[0x7F]),
            Some(Error::Invalid("valores sobrantes al cerrar el bloque"))
        );
        // Etiqueta inexistente.
        assert_eq!(
            invalid(&[0x0C, 5], &[], &[]),
            Some(Error::Invalid("etiqueta fuera de rango"))
        );
        // `if` con resultado y sin `else`.
        assert_eq!(
            invalid(&[0x41, 1, 0x04, 0x7F, 0x41, 7, 0x0B], &[], &[0x7F]),
            Some(Error::Invalid("if sin else con resultados"))
        );
        // Local y global fuera de rango; opcode desconocido.
        assert_eq!(
            invalid(&[0x20, 9], &[], &[0x7F]),
            Some(Error::Invalid("local fuera de rango"))
        );
        assert_eq!(
            invalid(&[0x23, 0], &[], &[0x7F]),
            Some(Error::Invalid("global fuera de rango"))
        );
        assert_eq!(
            invalid(&[0xD0], &[], &[]),
            Some(Error::Unsupported("opcode"))
        );
        // Codigo inalcanzable: la pila es polimorfica y el resto valida.
        assert_eq!(invalid(&[0x00, 0x6A], &[], &[0x7F]), None);
        // Un modulo correcto sigue pasando.
        assert_eq!(invalid(&[0x20, 0, 0x41, 1, 0x6A], &[0x7F], &[0x7F]), None);
    }
}
