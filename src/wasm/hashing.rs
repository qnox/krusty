//! Kotlin's `hashCode()` rules, as the runtime functions and inline sequences every module uses.
//!
//! A program can print a hash, so every target must answer the same number kotlinc's output does.
//! The rules are the library's own declarations:
//!
//! - `Boolean` is `1231` for `true` and `1237` for `false`.
//! - `Byte`, `Short`, `Char` and `Int` are their value as an `Int`. `UByte` and `UShort` answer
//!   `data.toInt()` on the `Byte`/`Short` they wrap, which sign-extends; `UInt` is its `Int`.
//! - `Long` and `ULong` fold their 64 bits: `(value xor (value ushr 32)).toInt()`.
//! - `Float` is `toBits()` and `Double` is `toBits()` folded as a `Long` is. `toBits` maps every
//!   NaN to the one canonical NaN, so two NaNs hash, and compare in a data class, alike.
//! - `String` is `s[0]*31^(n-1) + … + s[n-1]` over its UTF-16 code units, and `null` is `0`.
//! - Every other value dispatches through the `hashCode` slot of its class's table.

use super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::objects::{ObjectRoot, REFERENCE};
use super::runtime::{ADD32, GE_U32, MUL32, SHR_U64, WRAP_I64, XOR64};
use crate::backend::class_tables::AnyMember;
use crate::types::{Ty, TyKind};

const I32_REINTERPRET_F32: u8 = 0xbc;
const I64_REINTERPRET_F64: u8 = 0xbd;
const F32_NE: u8 = 0x5c;
const F64_NE: u8 = 0x62;
const SELECT: u8 = 0x1b;
const I32_EXTEND8_S: u8 = 0xc0;
const I32_EXTEND16_S: u8 = 0xc1;

/// `Float.NaN.toBits()` and `Double.NaN.toBits()`.
const CANONICAL_NAN_F32: i32 = 0x7fc0_0000;
const CANONICAL_NAN_F64: i64 = 0x7ff8_0000_0000_0000;

/// The hash of `kotlin.Unit`. `Unit` inherits `Any`'s identity hash, and any fixed number is an
/// identity hash of a class with exactly one instance.
const UNIT_HASH: i32 = 0;

/// Function indices of the hashing runtime.
pub(super) struct Hashing {
    /// `(Float) -> Int`: `toBits()`, NaN canonical.
    float_bits: u32,
    /// `(Double) -> Long`: `toBits()`, NaN canonical.
    double_bits: u32,
    /// `(Long) -> Int`: the 64-bit fold.
    fold: u32,
    /// `(String?) -> Int`.
    pub(super) string_hash: u32,
    /// `(Any?) -> Int`: `value?.hashCode() ?: 0`, by the value's runtime representation.
    pub(super) hash_code: u32,
    /// `kotlin.Unit`'s `hashCode` slot body.
    pub(super) unit_hash_code: u32,
}

impl Hashing {
    pub(super) fn declare(module: &mut Module) -> Self {
        Self {
            float_bits: module.declare(),
            double_bits: module.declare(),
            fold: module.declare(),
            string_hash: module.declare(),
            hash_code: module.declare(),
            unit_hash_code: module.declare(),
        }
    }

    /// Define every function [`Self::declare`] declared, with `string` and `chars` the runtime's
    /// string types.
    pub(super) fn define(
        &self,
        module: &mut Module,
        objects: &ObjectRoot,
        string: u32,
        chars: u32,
    ) {
        self.define_float_bits(module);
        self.define_double_bits(module);
        self.define_fold(module);
        self.define_string_hash(module, string, chars);
        self.define_hash_code(module, objects, string);
        let mut code = Code::default();
        code.i32_const(UNIT_HASH);
        let ty = objects.any_slot_types[AnyMember::HashCode.slot() as usize];
        module.define(
            self.unit_hash_code,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    /// Replace the value of scalar type `ty` on the stack with its hash; `false`, emitting
    /// nothing, when `ty` is not carried as a scalar.
    pub(super) fn emit_scalar_hash(&self, code: &mut Code, ty: Ty) -> bool {
        match ty.kind() {
            // `1237 - 6 * value`: `1237` for `false`, `1231` for `true`.
            TyKind::BOOLEAN => {
                code.i32_const(-6).op(MUL32).i32_const(1237).op(ADD32);
            }
            // Carried as the `Int` they hash as: `Byte`/`Short` sign-extended, `Char` zero-extended.
            TyKind::BYTE | TyKind::SHORT | TyKind::CHAR | TyKind::INT | TyKind::UINT => {}
            // Carried zero-extended, hashed as the signed value they wrap.
            TyKind::UBYTE => {
                code.op(I32_EXTEND8_S);
            }
            TyKind::USHORT => {
                code.op(I32_EXTEND16_S);
            }
            TyKind::LONG | TyKind::ULONG => {
                code.call(self.fold);
            }
            TyKind::FLOAT => {
                code.call(self.float_bits);
            }
            TyKind::DOUBLE => {
                code.call(self.double_bits).call(self.fold);
            }
            _ => return false,
        }
        true
    }

    /// Replace the floating value of type `ty` on the stack with its canonical bits: what a data
    /// class compares a floating property by (`Float.compare(a, b) == 0` on the JVM).
    pub(super) fn emit_float_bits(&self, code: &mut Code, ty: Ty) -> bool {
        match ty.kind() {
            TyKind::FLOAT => code.call(self.float_bits),
            TyKind::DOUBLE => code.call(self.double_bits),
            _ => return false,
        };
        true
    }

    fn define_float_bits(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::F32], vec![ValType::I32]);
        let mut code = Code::default();
        code.i32_const(CANONICAL_NAN_F32)
            .local_get(0)
            .op(I32_REINTERPRET_F32)
            .local_get(0)
            .local_get(0)
            .op(F32_NE)
            .op(SELECT);
        let locals = Vec::new();
        module.define(self.float_bits, Function { ty, locals, code });
    }

    fn define_double_bits(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::F64], vec![ValType::I64]);
        let mut code = Code::default();
        code.i64_const(CANONICAL_NAN_F64)
            .local_get(0)
            .op(I64_REINTERPRET_F64)
            .local_get(0)
            .local_get(0)
            .op(F64_NE)
            .op(SELECT);
        let locals = Vec::new();
        module.define(self.double_bits, Function { ty, locals, code });
    }

    fn define_fold(&self, module: &mut Module) {
        let ty = module.func_type(vec![ValType::I64], vec![ValType::I32]);
        let mut code = Code::default();
        code.local_get(0)
            .local_get(0)
            .i64_const(32)
            .op(SHR_U64)
            .op(XOR64)
            .op(WRAP_I64);
        let locals = Vec::new();
        module.define(self.fold, Function { ty, locals, code });
    }

    fn define_string_hash(&self, module: &mut Module, string: u32, chars: u32) {
        let ty = module.func_type(vec![ValType::nullable(string)], vec![ValType::I32]);
        let (value, units, length, index, hash) = (0, 1, 2, 3, 4);
        let mut code = Code::default();
        code.local_get(value)
            .ref_is_null()
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        code.local_get(value)
            .struct_get(string, 0)
            .local_tee(units)
            .array_len()
            .local_set(length);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index).local_get(length).op(GE_U32).br_if(1);
        code.local_get(hash)
            .i32_const(31)
            .op(MUL32)
            .local_get(units)
            .local_get(index)
            .array_get_u(chars)
            .op(ADD32)
            .local_set(hash);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().end().local_get(hash);
        let locals = vec![
            ValType::reference(chars),
            ValType::I32,
            ValType::I32,
            ValType::I32,
        ];
        module.define(self.string_hash, Function { ty, locals, code });
    }

    /// `(Any?) -> Int`. A string is not an object of the class model (see `super::objects`), so it
    /// is hashed here; every object, including primitive boxes and array identity wrappers,
    /// answers through its table.
    fn define_hash_code(&self, module: &mut Module, objects: &ObjectRoot, string: u32) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let string = HeapType::Concrete(string);
        let slot = AnyMember::HashCode.slot();
        let mut code = Code::default();
        code.local_get(0)
            .ref_is_null()
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        code.local_get(0)
            .ref_test(false, string)
            .if_(BlockType::Empty)
            .local_get(0)
            .ref_cast(false, string)
            .call(self.string_hash)
            .return_()
            .end();
        code.local_get(0)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(objects.object))
            .struct_get(objects.object, 0)
            .struct_get(objects.any_table, slot)
            .call_ref(objects.any_slot_types[slot as usize]);
        let locals = Vec::new();
        module.define(self.hash_code, Function { ty, locals, code });
    }
}
