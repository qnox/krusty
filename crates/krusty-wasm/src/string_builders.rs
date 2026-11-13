//! `kotlin.text.StringBuilder`, laid out by the runtime.
//!
//! The target stdlib's own `StringBuilder` is written over that target's `String` internals (a
//! `JsString` on wasm-js), which this backend's string struct does not share, so its KLIB bodies
//! cannot run here. The runtime lays the class out instead, as it lays out `String`: an identity
//! object holding growable UTF-16 storage and the count of units in use. Its members are the
//! runtime functions below; `super::codegen` realizes a call of a library member at that receiver.
//!
//! **Layout.** `struct { table, hash, chars: (mut (ref $chars)), length: (mut i32) }`, a subtype
//! of the identity object, so `hashCode` is the identity hash and `equals` is identity, as
//! Kotlin's `StringBuilder` inherits both from `Any`. Its `toString` slot copies the units in use
//! into a new string.

use super::encode::{BlockType, Code, CompositeType, FieldType, Function, HeapType, Module};
use super::encode::{StorageType, SubType, ValType};
use super::identity::IdentityObjects;
use super::objects::{immutable, ObjectRoot, REFERENCE};
use super::runtime::{ADD32, GE_S32, GT_S32, LT_S32, MUL32, OR32, SUB32};

/// `select`: the first of two values when the `i32` above them is nonzero, else the second.
const SELECT: u8 = 0x1b;
const CHARS_FIELD: u32 = super::identity::PREFIX_FIELDS;
const LENGTH_FIELD: u32 = CHARS_FIELD + 1;
/// The capacity of a builder that asked for less, and the least a full one grows by.
const MINIMUM_CAPACITY: i32 = 16;

/// The runtime's `StringBuilder` type and the functions realizing its members.
pub(super) struct StringBuilders {
    table: u32,
    /// `(capacity: Int) -> StringBuilder`.
    pub(super) new: u32,
    /// `(builder, value: String?) -> builder`: appends `value`, or `"null"`.
    pub(super) append_string: u32,
    /// `(builder, value: String?, start: Int, end: Int) -> builder`: appends that range of
    /// `value`, or of `"null"`, trapping on a range outside it.
    append_range: u32,
    /// `(builder, value: Char) -> builder`.
    pub(super) append_char: u32,
    /// `(builder) -> String`: the units in use, copied.
    pub(super) to_string: u32,
    /// `(builder) -> Int`.
    pub(super) length: u32,
    /// `(builder, index: Int) -> Char`, trapping outside the units in use.
    pub(super) get: u32,
}

impl StringBuilders {
    /// Declare the builder type and define its functions over `string`, the runtime's string
    /// struct of `chars` units.
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
        chars: u32,
        string: u32,
    ) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                mutable(ValType::reference(chars)),
                mutable(ValType::I32),
            ]),
            supertype: Some(identity.object),
            open: false,
        });
        let to_string = module.declare();
        let (_, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![object]);
        let mut init = Code::default();
        for function in [objects.identity_equals, identity.hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable);
        // No type information describes the runtime's builder yet: the field is null, as for any
        // runtime object no classifier record covers.
        objects.push_type_info(&mut init, None);
        init.struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        let builders = Self {
            table,
            new: module.declare(),
            append_string: module.declare(),
            append_range: module.declare(),
            append_char: module.declare(),
            to_string,
            length: module.declare(),
            get: module.declare(),
        };
        let layout = Layout {
            object,
            chars,
            string,
        };
        builders.define_new(module, identity, layout);
        let reserve = layout.define_reserve(module);
        builders.define_append_range(module, layout, reserve);
        builders.define_append_string(module, layout);
        builders.define_append_char(module, layout, reserve);
        builders.define_to_string(module, layout, objects);
        builders.define_length(module, layout);
        builders.define_get(module, layout);
        builders
    }

    fn define_new(&self, module: &mut Module, identity: &IdentityObjects, layout: Layout) {
        let ty = module.func_type(vec![ValType::I32], vec![REFERENCE]);
        let capacity = 0;
        let mut code = Code::default();
        code.global_get(self.table).call(identity.allocate);
        // A capacity below the minimum, a negative one included, asks for the minimum: the
        // capacity is a hint, and Kotlin's builders on this target do not reject one.
        code.local_get(capacity)
            .i32_const(MINIMUM_CAPACITY)
            .local_get(capacity)
            .i32_const(MINIMUM_CAPACITY)
            .op(GT_S32)
            .op(SELECT)
            .array_new_default(layout.chars)
            .i32_const(0)
            .struct_new(layout.object);
        module.define(
            self.new,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_append_range(&self, module: &mut Module, layout: Layout, reserve: u32) {
        let string = ValType::nullable(layout.string);
        let ty = module.func_type(
            vec![REFERENCE, string, ValType::I32, ValType::I32],
            vec![REFERENCE],
        );
        let (builder, value, start, end, source, at) = (0, 1, 2, 3, 4, 5);
        let mut code = Code::default();
        layout.units_or_null(&mut code, value);
        code.local_set(source);
        // `0 <= start <= end <= value.length`, as `append(CharSequence?, Int, Int)` requires.
        code.local_get(start)
            .i32_const(0)
            .op(LT_S32)
            .local_get(start)
            .local_get(end)
            .op(GT_S32)
            .op(OR32)
            .local_get(end)
            .local_get(source)
            .array_len()
            .op(GT_S32)
            .op(OR32)
            .if_(BlockType::Empty)
            .unreachable()
            .end();
        code.local_get(builder)
            .local_get(end)
            .local_get(start)
            .op(SUB32)
            .call(reserve)
            .local_set(at);
        // array.copy dst dst_offset src src_offset length
        layout.storage(&mut code, builder);
        code.local_get(at)
            .local_get(source)
            .local_get(start)
            .local_get(end)
            .local_get(start)
            .op(SUB32)
            .array_copy(layout.chars, layout.chars);
        code.local_get(builder);
        let locals = vec![ValType::reference(layout.chars), ValType::I32];
        module.define(self.append_range, Function { ty, locals, code });
    }

    fn define_append_string(&self, module: &mut Module, layout: Layout) {
        let string = ValType::nullable(layout.string);
        let ty = module.func_type(vec![REFERENCE, string], vec![REFERENCE]);
        let (builder, value) = (0, 1);
        let mut code = Code::default();
        code.local_get(builder).local_get(value).i32_const(0);
        layout.units_or_null(&mut code, value);
        code.array_len().call(self.append_range);
        module.define(
            self.append_string,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_append_char(&self, module: &mut Module, layout: Layout, reserve: u32) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![REFERENCE]);
        let (builder, value, at) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(builder)
            .i32_const(1)
            .call(reserve)
            .local_set(at);
        layout.storage(&mut code, builder);
        code.local_get(at)
            .local_get(value)
            .array_set(layout.chars)
            .local_get(builder);
        module.define(
            self.append_char,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
    }

    fn define_to_string(&self, module: &mut Module, layout: Layout, objects: &ObjectRoot) {
        let ty = objects.any_slot_types[2];
        let (builder, result) = (0, 1);
        let mut code = Code::default();
        layout.length(&mut code, builder);
        code.array_new_default(layout.chars).local_set(result);
        code.local_get(result).i32_const(0);
        layout.storage(&mut code, builder);
        code.i32_const(0);
        layout.length(&mut code, builder);
        code.array_copy(layout.chars, layout.chars)
            .local_get(result)
            .struct_new(layout.string);
        module.define(
            self.to_string,
            Function {
                ty,
                locals: vec![ValType::reference(layout.chars)],
                code,
            },
        );
    }

    fn define_length(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let mut code = Code::default();
        layout.length(&mut code, 0);
        module.define(
            self.length,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }

    fn define_get(&self, module: &mut Module, layout: Layout) {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![ValType::I32]);
        let (builder, index) = (0, 1);
        let mut code = Code::default();
        // Only the units in use are the builder's; the spare capacity past them is not.
        code.local_get(index)
            .i32_const(0)
            .op(LT_S32)
            .local_get(index);
        layout.length(&mut code, builder);
        code.op(GE_S32)
            .op(OR32)
            .if_(BlockType::Empty)
            .unreachable()
            .end();
        layout.storage(&mut code, builder);
        code.local_get(index).array_get_u(layout.chars);
        module.define(
            self.get,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
}

/// The types every builder function reads.
#[derive(Clone, Copy)]
struct Layout {
    object: u32,
    chars: u32,
    string: u32,
}

impl Layout {
    /// Push the storage of the builder in local `builder`.
    fn storage(self, code: &mut Code, builder: u32) {
        code.local_get(builder)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, CHARS_FIELD);
    }

    /// Push the count of units in use of the builder in local `builder`.
    fn length(self, code: &mut Code, builder: u32) {
        code.local_get(builder)
            .ref_cast(false, HeapType::Concrete(self.object))
            .struct_get(self.object, LENGTH_FIELD);
    }

    /// Push the units of the nullable string in local `value`, or those of `"null"`.
    fn units_or_null(self, code: &mut Code, value: u32) {
        let units = ValType::reference(self.chars);
        code.local_get(value)
            .ref_is_null()
            .if_(BlockType::Value(units));
        for unit in "null".encode_utf16() {
            code.i32_const(i32::from(unit));
        }
        code.array_new_fixed(self.chars, 4)
            .else_()
            .local_get(value)
            .struct_get(self.string, 0)
            .end();
    }

    /// `(builder, count: Int) -> Int`: make room for `count` more units, grow the count in use by
    /// it, and answer the index the first of them goes to. Full storage grows to twice its
    /// capacity plus the minimum, or to what is needed when that is more.
    fn define_reserve(self, module: &mut Module) -> u32 {
        let ty = module.func_type(vec![REFERENCE, ValType::I32], vec![ValType::I32]);
        let (builder, count, at, needed, storage, grown, grown_storage) = (0, 1, 2, 3, 4, 5, 6);
        let mut code = Code::default();
        self.length(&mut code, builder);
        code.local_tee(at)
            .local_get(count)
            .op(ADD32)
            .local_set(needed);
        self.storage(&mut code, builder);
        code.local_tee(storage)
            .array_len()
            .local_get(needed)
            .op(LT_S32)
            .if_(BlockType::Empty);
        code.local_get(storage)
            .array_len()
            .i32_const(2)
            .op(MUL32)
            .i32_const(MINIMUM_CAPACITY)
            .op(ADD32)
            .local_tee(grown)
            .local_get(needed)
            .local_get(grown)
            .local_get(needed)
            .op(GT_S32)
            .op(SELECT)
            .array_new_default(self.chars)
            .local_set(grown_storage);
        code.local_get(grown_storage)
            .i32_const(0)
            .local_get(storage)
            .i32_const(0)
            .local_get(at)
            .array_copy(self.chars, self.chars);
        code.local_get(builder)
            .ref_cast(false, HeapType::Concrete(self.object))
            .local_get(grown_storage)
            .struct_set(self.object, CHARS_FIELD);
        code.end();
        code.local_get(builder)
            .ref_cast(false, HeapType::Concrete(self.object))
            .local_get(needed)
            .struct_set(self.object, LENGTH_FIELD);
        code.local_get(at);
        let units = ValType::reference(self.chars);
        let locals = vec![ValType::I32, ValType::I32, units, ValType::I32, units];
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        function
    }
}

/// A mutable struct field holding `value`.
fn mutable(value: ValType) -> FieldType {
    FieldType {
        storage: StorageType::Val(value),
        mutable: true,
    }
}
