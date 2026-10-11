//! The object model and the runtime functions every module carries.
//!
//! Native krusty links a runtime written in C. A Wasm module cannot link one: WasmGC objects are
//! typed by the module that allocates them, so the runtime has to be emitted INTO each module, in
//! that module's types. These functions are therefore written here, instruction by instruction,
//! and declared before any program function so their indices are fixed.
//!
//! They are the operations no Kotlin source in the program spells and every program needs: string
//! equality and concatenation, the decimal form of an integer, Kotlin's division (which wasm's own
//! `div_s` would trap on for `MIN_VALUE / -1`), and writing a string to the host. Kotlin/Wasm gets
//! most of these from its standard library's Kotlin bodies; krusty will too once the library's
//! klib IR is compiled by this backend, and each helper here is then replaced by the library
//! function it stands in for.
//!
//! **Strings** are `struct { chars: (ref (array (mut i16))) }`: UTF-16 code units, as Kotlin's
//! `String` is on every target. A string constant is copied out of the module's passive data
//! segment with `array.new_data`.

use std::collections::HashMap;

use super::encode::{
    BlockType, Code, CompositeType, FieldType, Function, HeapType, Module, StorageType, ValType,
};
use super::host::Host;
use super::objects::ObjectRoot;

/// Where a host write's bytes are staged in linear memory. The first 16 bytes are left for the
/// host's own use (a WASI iovec).
pub(super) const BUFFER: i32 = 16;
/// How many bytes are staged before they are flushed to the host.
const BUFFER_CAPACITY: i32 = 4096;

/// Type and function indices of the runtime, fixed when the module is started.
pub(super) struct Runtime {
    /// `(array (mut i16))`.
    pub(super) chars: u32,
    /// `(struct (field (ref $chars)))`.
    pub(super) string: u32,
    pub(super) string_equals: u32,
    pub(super) string_concat: u32,
    pub(super) string_of_i32: u32,
    pub(super) string_of_i64: u32,
    pub(super) string_of_char: u32,
    pub(super) string_of_boolean: u32,
    pub(super) string_or_null: u32,
    pub(super) write_string: u32,
    pub(super) i32_div: u32,
    pub(super) i64_div: u32,
    /// The object model's root types and `kotlin.Any`'s members.
    pub(super) objects: ObjectRoot,
    /// Offsets of string constants already in the data segment.
    constants: HashMap<Vec<u16>, u32>,
}

impl Runtime {
    pub(super) fn string_type(&self) -> ValType {
        ValType::reference(self.string)
    }

    pub(super) fn nullable_string_type(&self) -> ValType {
        ValType::nullable(self.string)
    }

    /// Declare the object model's types: `$chars` then `$String`, as types 0 and 1, which is what
    /// the code generator's carriers name. Runs before anything else declares a type.
    pub(super) fn declare_types(module: &mut Module) -> (u32, u32) {
        let chars = module.ty(CompositeType::Array(FieldType {
            storage: StorageType::I16,
            mutable: true,
        }));
        let string = module.ty(CompositeType::Struct(vec![FieldType {
            storage: StorageType::Val(ValType::reference(chars)),
            mutable: false,
        }]));
        assert_eq!(
            (chars, string),
            (0, 1),
            "the object model's types come first"
        );
        (chars, string)
    }

    /// Declare the object model's types and the runtime functions, and define their bodies. Must
    /// run after every import is declared and before any program function.
    pub(super) fn start(module: &mut Module, host: &Host) -> Self {
        let (chars, string) = Self::declare_types(module);
        let mut runtime = Self {
            chars,
            string,
            string_equals: module.declare(),
            string_concat: module.declare(),
            string_of_i32: module.declare(),
            string_of_i64: module.declare(),
            string_of_char: module.declare(),
            string_of_boolean: module.declare(),
            string_or_null: module.declare(),
            write_string: module.declare(),
            i32_div: module.declare(),
            i64_div: module.declare(),
            objects: ObjectRoot::declare(module, ValType::nullable(string)),
            constants: HashMap::new(),
        };
        runtime.define_string_equals(module);
        runtime.define_string_concat(module);
        runtime.define_string_of_i64(module);
        runtime.define_string_of_i32(module);
        runtime.define_string_of_char(module);
        runtime.define_string_of_boolean(module);
        runtime.define_string_or_null(module);
        runtime.define_write_string(module, host);
        runtime.define_division(module);
        runtime
    }

    /// Push a new `String` holding `units`.
    pub(super) fn string_constant(&mut self, module: &mut Module, code: &mut Code, units: &[u16]) {
        let offset = match self.constants.get(units) {
            Some(offset) => *offset,
            None => {
                let bytes = units
                    .iter()
                    .flat_map(|unit| unit.to_le_bytes())
                    .collect::<Vec<_>>();
                let offset = module.data(&bytes);
                self.constants.insert(units.to_vec(), offset);
                offset
            }
        };
        code.i32_const(offset as i32)
            .i32_const(units.len() as i32)
            .array_new_data(self.chars, 0)
            .struct_new(self.string);
    }

    fn ascii_constant(&mut self, module: &mut Module, code: &mut Code, text: &str) {
        let units = text.encode_utf16().collect::<Vec<_>>();
        self.string_constant(module, code, &units);
    }

    /// `(a: String?, b: String?) -> Boolean`: Kotlin's `==` on two strings.
    fn define_string_equals(&self, module: &mut Module) {
        let string = self.nullable_string_type();
        let ty = module.func_type(vec![string, string], vec![ValType::I32]);
        let (a, b, left, right, length, index) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        code.local_get(a).local_get(b).ref_eq();
        code.if_(BlockType::Empty).i32_const(1).return_().end();
        code.local_get(a)
            .ref_is_null()
            .local_get(b)
            .ref_is_null()
            .op(OR32);
        code.if_(BlockType::Empty).i32_const(0).return_().end();
        code.local_get(a)
            .struct_get(self.string, 0)
            .local_tee(left)
            .array_len()
            .local_tee(length);
        code.local_get(b)
            .struct_get(self.string, 0)
            .local_tee(right)
            .array_len()
            .op(NE32);
        code.if_(BlockType::Empty).i32_const(0).return_().end();
        code.loop_(BlockType::Empty);
        code.local_get(index).local_get(length).op(GE_U32);
        code.if_(BlockType::Empty).i32_const(1).return_().end();
        code.local_get(left)
            .local_get(index)
            .array_get_u(self.chars);
        code.local_get(right)
            .local_get(index)
            .array_get_u(self.chars);
        code.op(NE32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().unreachable();
        let chars = ValType::reference(self.chars);
        let locals = vec![chars, chars, ValType::I32, ValType::I32];
        module.define(self.string_equals, Function { ty, locals, code });
    }

    /// `(a: String, b: String) -> String`: `a + b`.
    fn define_string_concat(&self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![string, string], vec![string]);
        let (a, b, left, right, result) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(a).struct_get(self.string, 0).local_set(left);
        code.local_get(b)
            .struct_get(self.string, 0)
            .local_set(right);
        code.local_get(left)
            .array_len()
            .local_get(right)
            .array_len()
            .op(ADD32)
            .array_new_default(self.chars)
            .local_set(result);
        // array.copy dst dst_offset src src_offset length
        code.local_get(result)
            .i32_const(0)
            .local_get(left)
            .i32_const(0)
            .local_get(left)
            .array_len()
            .array_copy(self.chars, self.chars);
        code.local_get(result)
            .local_get(left)
            .array_len()
            .local_get(right)
            .i32_const(0)
            .local_get(right)
            .array_len()
            .array_copy(self.chars, self.chars);
        code.local_get(result).struct_new(self.string);
        let chars = ValType::reference(self.chars);
        let locals = vec![chars, chars, chars];
        module.define(self.string_concat, Function { ty, locals, code });
    }

    /// `(value: Long) -> String`: the decimal form, `-` first for a negative value. The magnitude
    /// is taken as UNSIGNED so that `Long.MIN_VALUE`, which has no positive counterpart, still has
    /// one: `0 - MIN_VALUE` wraps to itself, which read unsigned is exactly its magnitude.
    fn define_string_of_i64(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::I64], vec![self.string_type()]);
        let (value, magnitude, digits, position, result) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.i32_const(20)
            .array_new_default(self.chars)
            .local_set(digits);
        code.i32_const(20).local_set(position);
        code.local_get(value)
            .i64_const(0)
            .op(LT_S64)
            .if_(BlockType::Value(ValType::I64))
            .i64_const(0)
            .local_get(value)
            .op(SUB64)
            .else_()
            .local_get(value)
            .end()
            .local_set(magnitude);
        code.loop_(BlockType::Empty);
        code.local_get(position)
            .i32_const(1)
            .op(SUB32)
            .local_set(position);
        code.local_get(digits)
            .local_get(position)
            .local_get(magnitude)
            .i64_const(10)
            .op(REM_U64)
            .op(WRAP_I64)
            .i32_const(i32::from(b'0'))
            .op(ADD32)
            .array_set(self.chars);
        code.local_get(magnitude)
            .i64_const(10)
            .op(DIV_U64)
            .local_tee(magnitude)
            .i64_const(0)
            .op(NE64)
            .br_if(0);
        code.end();
        code.local_get(value).i64_const(0).op(LT_S64);
        code.if_(BlockType::Empty)
            .local_get(position)
            .i32_const(1)
            .op(SUB32)
            .local_set(position)
            .local_get(digits)
            .local_get(position)
            .i32_const(i32::from(b'-'))
            .array_set(self.chars)
            .end();
        code.i32_const(20)
            .local_get(position)
            .op(SUB32)
            .array_new_default(self.chars)
            .local_set(result);
        code.local_get(result)
            .i32_const(0)
            .local_get(digits)
            .local_get(position)
            .i32_const(20)
            .local_get(position)
            .op(SUB32)
            .array_copy(self.chars, self.chars);
        code.local_get(result).struct_new(self.string);
        let chars = ValType::reference(self.chars);
        let locals = vec![ValType::I64, chars, ValType::I32, chars];
        module.define(self.string_of_i64, Function { ty, locals, code });
    }

    /// `(value: Int) -> String`.
    fn define_string_of_i32(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::I32], vec![self.string_type()]);
        let mut code = Code::default();
        code.local_get(0).op(EXTEND_I32_S).call(self.string_of_i64);
        module.define(
            self.string_of_i32,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `(value: Char) -> String`: the one code unit.
    fn define_string_of_char(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::I32], vec![self.string_type()]);
        let mut code = Code::default();
        code.local_get(0)
            .array_new_fixed(self.chars, 1)
            .struct_new(self.string);
        module.define(
            self.string_of_char,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `(value: Boolean) -> String`.
    fn define_string_of_boolean(&mut self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![ValType::I32], vec![string]);
        let mut code = Code::default();
        code.local_get(0).if_(BlockType::Value(string));
        self.ascii_constant(module, &mut code, "true");
        code.else_();
        self.ascii_constant(module, &mut code, "false");
        code.end();
        module.define(
            self.string_of_boolean,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `(value: String?) -> String`: the value, or `"null"`.
    fn define_string_or_null(&mut self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![self.nullable_string_type()], vec![string]);
        let mut code = Code::default();
        code.local_get(0)
            .ref_is_null()
            .if_(BlockType::Value(string));
        self.ascii_constant(module, &mut code, "null");
        code.else_().local_get(0).ref_as_non_null().end();
        module.define(
            self.string_or_null,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// `(value: String) -> Unit`: write the string to the host's standard output as UTF-8.
    ///
    /// A surrogate pair is one code point and four bytes. A LONE surrogate is written as the
    /// three bytes its code unit would take (WTF-8): that is not valid UTF-8, but it loses
    /// nothing, and a program that prints one is asking for exactly that unit.
    fn define_write_string(&self, module: &mut Module, host: &Host) {
        let ty = module.func_type(vec![self.string_type()], Vec::new());
        let (value, chars, length, index, position, unit) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        let store = |code: &mut Code, byte: &dyn Fn(&mut Code)| {
            code.local_get(position);
            byte(code);
            code.i32_store8(0)
                .local_get(position)
                .i32_const(1)
                .op(ADD32)
                .local_set(position);
        };
        code.local_get(value)
            .struct_get(self.string, 0)
            .local_tee(chars)
            .array_len()
            .local_set(length);
        code.i32_const(BUFFER).local_set(position);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index).local_get(length).op(GE_U32).br_if(1);
        code.local_get(chars)
            .local_get(index)
            .array_get_u(self.chars)
            .local_set(unit);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index);
        // One byte.
        code.local_get(unit)
            .i32_const(0x80)
            .op(LT_U32)
            .if_(BlockType::Empty);
        store(&mut code, &|code| {
            code.local_get(unit);
        });
        code.else_();
        // Two bytes.
        code.local_get(unit)
            .i32_const(0x800)
            .op(LT_U32)
            .if_(BlockType::Empty);
        store(&mut code, &|code| {
            code.local_get(unit)
                .i32_const(6)
                .op(SHR_U32)
                .i32_const(0xc0)
                .op(OR32);
        });
        store(&mut code, &|code| continuation(code, unit, 0));
        code.else_();
        // A high surrogate followed by a low one: one code point, four bytes. The code point
        // replaces `unit`, and the low surrogate is consumed.
        code.local_get(unit)
            .i32_const(0xfc00)
            .op(AND32)
            .i32_const(0xd800)
            .op(EQ32);
        code.local_get(index).local_get(length).op(LT_U32).op(AND32);
        code.if_(BlockType::Value(ValType::I32))
            .local_get(chars)
            .local_get(index)
            .array_get_u(self.chars)
            .i32_const(0xfc00)
            .op(AND32)
            .i32_const(0xdc00)
            .op(EQ32)
            .else_()
            .i32_const(0)
            .end();
        code.if_(BlockType::Empty);
        code.local_get(unit)
            .i32_const(0xd800)
            .op(SUB32)
            .i32_const(10)
            .op(SHL32)
            .local_get(chars)
            .local_get(index)
            .array_get_u(self.chars)
            .i32_const(0xdc00)
            .op(SUB32)
            .op(OR32)
            .i32_const(0x10000)
            .op(ADD32)
            .local_set(unit);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index);
        store(&mut code, &|code| {
            code.local_get(unit)
                .i32_const(18)
                .op(SHR_U32)
                .i32_const(0xf0)
                .op(OR32);
        });
        store(&mut code, &|code| continuation(code, unit, 12));
        store(&mut code, &|code| continuation(code, unit, 6));
        store(&mut code, &|code| continuation(code, unit, 0));
        code.else_();
        // Three bytes, a lone surrogate included.
        store(&mut code, &|code| {
            code.local_get(unit)
                .i32_const(12)
                .op(SHR_U32)
                .i32_const(0xe0)
                .op(OR32);
        });
        store(&mut code, &|code| continuation(code, unit, 6));
        store(&mut code, &|code| continuation(code, unit, 0));
        code.end().end().end();
        // Flush before the next code point could overrun the buffer.
        code.local_get(position)
            .i32_const(BUFFER + BUFFER_CAPACITY - 4)
            .op(GE_U32)
            .if_(BlockType::Empty)
            .local_get(position)
            .i32_const(BUFFER)
            .op(SUB32)
            .call(host.flush)
            .i32_const(BUFFER)
            .local_set(position)
            .end();
        code.br(0).end().end();
        code.local_get(position)
            .i32_const(BUFFER)
            .op(SUB32)
            .call(host.flush);
        let locals = vec![
            ValType::reference(self.chars),
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
        ];
        module.define(self.write_string, Function { ty, locals, code });
    }

    /// Kotlin's integer division: as `div_s`, except that `MIN_VALUE / -1` answers `MIN_VALUE`
    /// (two's complement wraparound) where wasm traps. Division by zero still traps; Kotlin throws
    /// `ArithmeticException` there, which needs exceptions this backend does not have yet.
    fn define_division(&self, module: &mut Module) {
        for (index, value, minus_one, sub, div) in [
            (
                self.i32_div,
                ValType::I32,
                Code::i32_minus_one as fn(&mut Code),
                SUB32,
                DIV_S32,
            ),
            (
                self.i64_div,
                ValType::I64,
                Code::i64_minus_one,
                SUB64,
                DIV_S64,
            ),
        ] {
            let ty = module.func_type(vec![value, value], vec![value]);
            let mut code = Code::default();
            code.local_get(1);
            minus_one(&mut code);
            code.op(if value == ValType::I32 { EQ32 } else { EQ64 });
            code.if_(BlockType::Value(value));
            match value {
                ValType::I32 => code.i32_const(0),
                _ => code.i64_const(0),
            };
            code.local_get(0)
                .op(sub)
                .else_()
                .local_get(0)
                .local_get(1)
                .op(div)
                .end();
            module.define(
                index,
                Function {
                    ty,
                    locals: Vec::new(),
                    code,
                },
            );
        }
    }
}

/// Push the UTF-8 continuation byte carrying bits `shift..shift + 6` of `unit`.
fn continuation(code: &mut Code, unit: u32, shift: i32) {
    code.local_get(unit);
    if shift > 0 {
        code.i32_const(shift).op(SHR_U32);
    }
    code.i32_const(0x3f).op(AND32).i32_const(0x80).op(OR32);
}

impl Code {
    fn i32_minus_one(&mut self) {
        self.i32_const(-1);
    }

    fn i64_minus_one(&mut self) {
        self.i64_const(-1);
    }
}

/// The numeric opcodes this file and the code generator use, named as the text format names them.
pub(super) const EQZ32: u8 = 0x45;
pub(super) const EQ32: u8 = 0x46;
pub(super) const NE32: u8 = 0x47;
pub(super) const LT_S32: u8 = 0x48;
pub(super) const LT_U32: u8 = 0x49;
pub(super) const GT_S32: u8 = 0x4a;
pub(super) const LE_S32: u8 = 0x4c;
pub(super) const GE_S32: u8 = 0x4e;
pub(super) const GE_U32: u8 = 0x4f;
pub(super) const EQ64: u8 = 0x51;
pub(super) const NE64: u8 = 0x52;
pub(super) const LT_S64: u8 = 0x53;
pub(super) const GT_S64: u8 = 0x55;
pub(super) const LE_S64: u8 = 0x57;
pub(super) const GE_S64: u8 = 0x59;
pub(super) const ADD32: u8 = 0x6a;
pub(super) const SUB32: u8 = 0x6b;
pub(super) const MUL32: u8 = 0x6c;
pub(super) const DIV_S32: u8 = 0x6d;
pub(super) const REM_S32: u8 = 0x6f;
pub(super) const AND32: u8 = 0x71;
pub(super) const OR32: u8 = 0x72;
pub(super) const XOR32: u8 = 0x73;
pub(super) const SHL32: u8 = 0x74;
pub(super) const SHR_S32: u8 = 0x75;
pub(super) const SHR_U32: u8 = 0x76;
pub(super) const ADD64: u8 = 0x7c;
pub(super) const SUB64: u8 = 0x7d;
pub(super) const MUL64: u8 = 0x7e;
pub(super) const DIV_S64: u8 = 0x7f;
pub(super) const DIV_U64: u8 = 0x80;
pub(super) const REM_S64: u8 = 0x81;
pub(super) const REM_U64: u8 = 0x82;
pub(super) const AND64: u8 = 0x83;
pub(super) const OR64: u8 = 0x84;
pub(super) const XOR64: u8 = 0x85;
pub(super) const SHL64: u8 = 0x86;
pub(super) const SHR_S64: u8 = 0x87;
pub(super) const SHR_U64: u8 = 0x88;
pub(super) const WRAP_I64: u8 = 0xa7;
pub(super) const EXTEND_I32_S: u8 = 0xac;

/// The null of every struct and array type.
pub(super) const NULL: HeapType = HeapType::None;
