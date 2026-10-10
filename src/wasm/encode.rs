//! The WebAssembly binary format: a module builder and an instruction writer.
//!
//! This knows nothing about Kotlin. It writes the type forms Kotlin/Wasm needs — the GC proposal's
//! structs, arrays, packed fields and recursion groups — beside the MVP sections, and leaves every
//! decision about what a Kotlin value is to the code generator.
//!
//! Every type the module declares lives in ONE recursion group. That is what kotlinc emits too: a
//! class may name a type declared after it (a field of its own subclass's type, a method taking a
//! later class), and only a recursion group lets a type section refer forward. The price is that the
//! group's types are canonicalized together, which nothing here relies on.

/// Unsigned LEB128.
pub(super) fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Signed LEB128.
pub(super) fn sleb(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn name(out: &mut Vec<u8>, text: &str) {
    uleb(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

/// A heap type: a type this module declares, or one of the abstract ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum HeapType {
    Concrete(u32),
    /// `none`, the bottom of the internal (struct/array/i31) hierarchy: what `null` of any struct
    /// or array type is.
    None,
}

impl HeapType {
    fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::Concrete(index) => sleb(out, i64::from(index)),
            Self::None => out.push(0x71),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ValType {
    I32,
    I64,
    F32,
    F64,
    Ref { nullable: bool, heap: HeapType },
}

impl ValType {
    pub(super) fn reference(index: u32) -> Self {
        Self::Ref {
            nullable: false,
            heap: HeapType::Concrete(index),
        }
    }

    pub(super) fn nullable(index: u32) -> Self {
        Self::Ref {
            nullable: true,
            heap: HeapType::Concrete(index),
        }
    }

    fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::I32 => out.push(0x7f),
            Self::I64 => out.push(0x7e),
            Self::F32 => out.push(0x7d),
            Self::F64 => out.push(0x7c),
            Self::Ref { nullable, heap } => {
                out.push(if nullable { 0x63 } else { 0x64 });
                heap.encode(out);
            }
        }
    }
}

/// What a struct field or array element holds: a value, or one of the packed integer widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum StorageType {
    Val(ValType),
    I16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct FieldType {
    pub(super) storage: StorageType,
    pub(super) mutable: bool,
}

impl FieldType {
    fn encode(self, out: &mut Vec<u8>) {
        match self.storage {
            StorageType::Val(value) => value.encode(out),
            StorageType::I16 => out.push(0x77),
        }
        out.push(u8::from(self.mutable));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum CompositeType {
    Func {
        params: Vec<ValType>,
        results: Vec<ValType>,
    },
    Struct(Vec<FieldType>),
    Array(FieldType),
}

impl CompositeType {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Func { params, results } => {
                out.push(0x60);
                uleb(out, params.len() as u64);
                params.iter().for_each(|param| param.encode(out));
                uleb(out, results.len() as u64);
                results.iter().for_each(|result| result.encode(out));
            }
            Self::Struct(fields) => {
                out.push(0x5f);
                uleb(out, fields.len() as u64);
                fields.iter().for_each(|field| field.encode(out));
            }
            Self::Array(element) => {
                out.push(0x5e);
                element.encode(out);
            }
        }
    }
}

/// An import of a function; the only kind of import the hosts krusty targets need.
pub(super) struct FunctionImport {
    pub(super) module: &'static str,
    pub(super) name: &'static str,
    pub(super) ty: u32,
}

/// A function body under construction: its declared locals and its instruction bytes.
pub(super) struct Function {
    pub(super) ty: u32,
    pub(super) locals: Vec<ValType>,
    pub(super) code: Code,
}

/// A module, assembled in index order: the function index space is the imports followed by the
/// defined functions, in the order they were added.
#[derive(Default)]
pub(super) struct Module {
    types: Vec<CompositeType>,
    imports: Vec<FunctionImport>,
    functions: Vec<Option<Function>>,
    /// One page-granular linear memory, exported as `memory`, when the host ABI needs one.
    memory_pages: Option<u32>,
    exports: Vec<(String, u32)>,
    /// Mutable globals, each starting at its type's default value.
    globals: Vec<ValType>,
    /// One passive data segment: the bytes `array.new_data` copies constants out of.
    data: Vec<u8>,
}

impl Module {
    /// The index of `ty`, declaring it if it is new. Function types are deduplicated so that two
    /// functions of one signature share an index, which `call_ref` would need them to.
    pub(super) fn ty(&mut self, ty: CompositeType) -> u32 {
        if let Some(index) = self.types.iter().position(|known| *known == ty) {
            return index as u32;
        }
        self.types.push(ty);
        (self.types.len() - 1) as u32
    }

    pub(super) fn func_type(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        self.ty(CompositeType::Func { params, results })
    }

    /// Declare an imported function. Imports precede every defined function in the index space,
    /// so all of them are declared before the first [`Module::declare`].
    pub(super) fn import(&mut self, import: FunctionImport) -> u32 {
        assert!(
            self.functions.is_empty(),
            "a function import after a defined function would renumber it"
        );
        self.imports.push(import);
        (self.imports.len() - 1) as u32
    }

    /// Reserve the index of a function whose body is supplied later by [`Module::define`].
    pub(super) fn declare(&mut self) -> u32 {
        self.functions.push(None);
        (self.imports.len() + self.functions.len() - 1) as u32
    }

    pub(super) fn define(&mut self, index: u32, function: Function) {
        let slot = &mut self.functions[index as usize - self.imports.len()];
        assert!(slot.is_none(), "function {index} defined twice");
        *slot = Some(function);
    }

    pub(super) fn memory(&mut self, pages: u32) {
        self.memory_pages = Some(pages);
    }

    /// Declare a mutable global holding `ty`'s default value: zero, or null.
    pub(super) fn global(&mut self, ty: ValType) -> u32 {
        self.globals.push(ty);
        (self.globals.len() - 1) as u32
    }

    pub(super) fn export_function(&mut self, name: &str, index: u32) {
        self.exports.push((name.to_string(), index));
    }

    /// Append `bytes` to the passive data segment; the offset they start at.
    pub(super) fn data(&mut self, bytes: &[u8]) -> u32 {
        let offset = self.data.len() as u32;
        self.data.extend_from_slice(bytes);
        offset
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = b"\0asm".to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        section(&mut out, 1, |body| {
            uleb(body, 1);
            body.push(0x4e);
            uleb(body, self.types.len() as u64);
            for ty in &self.types {
                // `sub final` with no supertypes: the same type the short form would name, spelled
                // out so a class hierarchy can later declare supertypes without changing form.
                body.push(0x4f);
                uleb(body, 0);
                ty.encode(body);
            }
        });
        section(&mut out, 2, |body| {
            uleb(body, self.imports.len() as u64);
            for import in &self.imports {
                name(body, import.module);
                name(body, import.name);
                body.push(0x00);
                uleb(body, u64::from(import.ty));
            }
        });
        section(&mut out, 3, |body| {
            uleb(body, self.functions.len() as u64);
            for function in &self.functions {
                let function = function
                    .as_ref()
                    .expect("every declared function is defined");
                uleb(body, u64::from(function.ty));
            }
        });
        if let Some(pages) = self.memory_pages {
            section(&mut out, 5, |body| {
                uleb(body, 1);
                body.push(0x00);
                uleb(body, u64::from(pages));
            });
        }
        if !self.globals.is_empty() {
            section(&mut out, 6, |body| {
                uleb(body, self.globals.len() as u64);
                for ty in &self.globals {
                    ty.encode(body);
                    body.push(0x01);
                    let mut init = Code::default();
                    match ty {
                        ValType::I32 => init.i32_const(0),
                        ValType::I64 => init.i64_const(0),
                        ValType::F32 => init.f32_const(0.0),
                        ValType::F64 => init.f64_const(0.0),
                        ValType::Ref { heap, .. } => init.ref_null(*heap),
                    };
                    body.extend_from_slice(&init.bytes);
                    body.push(0x0b);
                }
            });
        }
        section(&mut out, 7, |body| {
            let memory = usize::from(self.memory_pages.is_some());
            uleb(body, (self.exports.len() + memory) as u64);
            for (export, index) in &self.exports {
                name(body, export);
                body.push(0x00);
                uleb(body, u64::from(*index));
            }
            if memory == 1 {
                name(body, "memory");
                body.push(0x02);
                uleb(body, 0);
            }
        });
        // `array.new_data` names a data segment from the code section, which the format only
        // permits once the data count section has announced how many there are.
        section(&mut out, 12, |body| uleb(body, 1));
        section(&mut out, 10, |body| {
            uleb(body, self.functions.len() as u64);
            for function in self.functions.iter().flatten() {
                let mut entry = Vec::new();
                let mut runs: Vec<(u32, ValType)> = Vec::new();
                for local in &function.locals {
                    match runs.last_mut() {
                        Some((count, ty)) if ty == local => *count += 1,
                        _ => runs.push((1, *local)),
                    }
                }
                uleb(&mut entry, runs.len() as u64);
                for (count, ty) in runs {
                    uleb(&mut entry, u64::from(count));
                    ty.encode(&mut entry);
                }
                entry.extend_from_slice(&function.code.bytes);
                entry.push(0x0b);
                uleb(body, entry.len() as u64);
                body.extend_from_slice(&entry);
            }
        });
        section(&mut out, 11, |body| {
            uleb(body, 1);
            body.push(0x01);
            uleb(body, self.data.len() as u64);
            body.extend_from_slice(&self.data);
        });
        out
    }
}

fn section(out: &mut Vec<u8>, id: u8, write: impl FnOnce(&mut Vec<u8>)) {
    let mut body = Vec::new();
    write(&mut body);
    out.push(id);
    uleb(out, body.len() as u64);
    out.extend_from_slice(&body);
}

/// The result of a structured control instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockType {
    Empty,
    Value(ValType),
}

/// One function's instruction stream. Each method appends one instruction; the names are the
/// text format's, so a reader can check a sequence against the specification directly.
#[derive(Default)]
pub(super) struct Code {
    bytes: Vec<u8>,
}

impl Code {
    pub(super) fn op(&mut self, opcode: u8) -> &mut Self {
        self.bytes.push(opcode);
        self
    }

    fn gc(&mut self, opcode: u8) -> &mut Self {
        self.bytes.push(0xfb);
        uleb(&mut self.bytes, u64::from(opcode));
        self
    }

    fn index(&mut self, index: u32) -> &mut Self {
        uleb(&mut self.bytes, u64::from(index));
        self
    }

    fn block_type(&mut self, ty: BlockType) -> &mut Self {
        match ty {
            BlockType::Empty => self.bytes.push(0x40),
            BlockType::Value(value) => value.encode(&mut self.bytes),
        }
        self
    }

    pub(super) fn block(&mut self, ty: BlockType) -> &mut Self {
        self.op(0x02).block_type(ty)
    }

    pub(super) fn loop_(&mut self, ty: BlockType) -> &mut Self {
        self.op(0x03).block_type(ty)
    }

    pub(super) fn if_(&mut self, ty: BlockType) -> &mut Self {
        self.op(0x04).block_type(ty)
    }

    pub(super) fn else_(&mut self) -> &mut Self {
        self.op(0x05)
    }

    pub(super) fn end(&mut self) -> &mut Self {
        self.op(0x0b)
    }

    pub(super) fn br(&mut self, depth: u32) -> &mut Self {
        self.op(0x0c).index(depth)
    }

    pub(super) fn br_if(&mut self, depth: u32) -> &mut Self {
        self.op(0x0d).index(depth)
    }

    pub(super) fn unreachable(&mut self) -> &mut Self {
        self.op(0x00)
    }

    pub(super) fn return_(&mut self) -> &mut Self {
        self.op(0x0f)
    }

    pub(super) fn call(&mut self, function: u32) -> &mut Self {
        self.op(0x10).index(function)
    }

    pub(super) fn drop_(&mut self) -> &mut Self {
        self.op(0x1a)
    }

    pub(super) fn local_get(&mut self, local: u32) -> &mut Self {
        self.op(0x20).index(local)
    }

    pub(super) fn local_set(&mut self, local: u32) -> &mut Self {
        self.op(0x21).index(local)
    }

    pub(super) fn local_tee(&mut self, local: u32) -> &mut Self {
        self.op(0x22).index(local)
    }

    pub(super) fn global_get(&mut self, global: u32) -> &mut Self {
        self.op(0x23).index(global)
    }

    pub(super) fn global_set(&mut self, global: u32) -> &mut Self {
        self.op(0x24).index(global)
    }

    /// `i32.store8` with alignment 0 at `offset`.
    pub(super) fn i32_store8(&mut self, offset: u32) -> &mut Self {
        self.op(0x3a).index(0).index(offset)
    }

    /// `i32.store` with natural alignment at `offset`.
    pub(super) fn i32_store(&mut self, offset: u32) -> &mut Self {
        self.op(0x36).index(2).index(offset)
    }

    pub(super) fn i32_const(&mut self, value: i32) -> &mut Self {
        self.op(0x41);
        sleb(&mut self.bytes, i64::from(value));
        self
    }

    pub(super) fn i64_const(&mut self, value: i64) -> &mut Self {
        self.op(0x42);
        sleb(&mut self.bytes, value);
        self
    }

    pub(super) fn f32_const(&mut self, value: f32) -> &mut Self {
        self.op(0x43);
        self.bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        self
    }

    pub(super) fn f64_const(&mut self, value: f64) -> &mut Self {
        self.op(0x44);
        self.bytes.extend_from_slice(&value.to_bits().to_le_bytes());
        self
    }

    pub(super) fn ref_null(&mut self, heap: HeapType) -> &mut Self {
        self.op(0xd0);
        heap.encode(&mut self.bytes);
        self
    }

    pub(super) fn ref_is_null(&mut self) -> &mut Self {
        self.op(0xd1)
    }

    pub(super) fn ref_eq(&mut self) -> &mut Self {
        self.op(0xd3)
    }

    pub(super) fn ref_as_non_null(&mut self) -> &mut Self {
        self.op(0xd4)
    }

    pub(super) fn struct_new(&mut self, ty: u32) -> &mut Self {
        self.gc(0x00).index(ty)
    }

    pub(super) fn struct_get(&mut self, ty: u32, field: u32) -> &mut Self {
        self.gc(0x02).index(ty).index(field)
    }

    pub(super) fn array_new_default(&mut self, ty: u32) -> &mut Self {
        self.gc(0x07).index(ty)
    }

    pub(super) fn array_new_fixed(&mut self, ty: u32, length: u32) -> &mut Self {
        self.gc(0x08).index(ty).index(length)
    }

    pub(super) fn array_new_data(&mut self, ty: u32, segment: u32) -> &mut Self {
        self.gc(0x09).index(ty).index(segment)
    }

    pub(super) fn array_get_u(&mut self, ty: u32) -> &mut Self {
        self.gc(0x0d).index(ty)
    }

    pub(super) fn array_set(&mut self, ty: u32) -> &mut Self {
        self.gc(0x0e).index(ty)
    }

    pub(super) fn array_len(&mut self) -> &mut Self {
        self.gc(0x0f)
    }

    pub(super) fn array_copy(&mut self, destination: u32, source: u32) -> &mut Self {
        self.gc(0x11).index(destination).index(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_matches_the_specification_examples() {
        let mut out = Vec::new();
        uleb(&mut out, 624_485);
        assert_eq!(out, [0xe5, 0x8e, 0x26]);
        out.clear();
        sleb(&mut out, -123_456);
        assert_eq!(out, [0xc0, 0xbb, 0x78]);
        out.clear();
        sleb(&mut out, 64);
        assert_eq!(out, [0xc0, 0x00]);
        out.clear();
        sleb(&mut out, -64);
        assert_eq!(out, [0x40]);
    }

    #[test]
    fn an_empty_module_is_the_header_and_its_fixed_sections() {
        let bytes = Module::default().encode();
        assert_eq!(
            bytes,
            [
                0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
                0x01, 0x03, 0x01, 0x4e, 0x00, // one empty recursion group
                0x02, 0x01, 0x00, // no imports
                0x03, 0x01, 0x00, // no functions
                0x07, 0x01, 0x00, // no exports
                0x0c, 0x01, 0x01, // one data segment
                0x0a, 0x01, 0x00, // no code
                0x0b, 0x03, 0x01, 0x01, 0x00, // an empty passive segment
            ]
        );
    }

    #[test]
    fn locals_of_one_type_are_declared_as_one_run() {
        let mut module = Module::default();
        let ty = module.func_type(Vec::new(), Vec::new());
        let index = module.declare();
        module.define(
            index,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::I32, ValType::I64],
                code: Code::default(),
            },
        );
        let bytes = module.encode();
        let code = [0x0a, 0x08, 0x01, 0x06, 0x02, 0x02, 0x7f, 0x01, 0x7e, 0x0b];
        assert!(
            bytes.windows(code.len()).any(|window| window == code),
            "{bytes:02x?}"
        );
    }
}
