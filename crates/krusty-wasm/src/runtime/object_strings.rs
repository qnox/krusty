//! `toString()` of a runtime reference, and the default bodies of `kotlin.Any.toString` and
//! `kotlin.Throwable.toString` that tables hold.
//!
//! A value read as `Any?` is `null`, a `String` (which is not an object of the class model, see
//! `super::super::objects`), or an object, which answers through the `toString` slot of its table.
//! The defaults are kotlinc-wasm's library bodies:
//!
//! - `Any.toString` is the class's qualified name, `@`, and the object's identity hash in decimal
//!   (`super::super::identity`).
//! - `Throwable.toString` is the class's name, then `: ` and the message when there is one. The
//!   name is the qualified one under `wasm-js` and the simple one under `wasm-wasi`, whose library
//!   body names the class by its `simpleName`.
//! - `Enum.toString` is the constant's `name`.
//!
//! Each body takes its class's name as data. One function per class name stands in the table, and
//! it hands that name to the shared rendering.

use super::*;
use crate::identity::HASH_FIELD;
use crate::objects::ENUM_NAME_FIELD;
use crate::objects::REFERENCE;
use crate::target::WasmTarget;
use krusty::backend::class_names::{member_class_names, ClassNames};
use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TypeName};

/// Function indices of the string rendering of objects.
pub(crate) struct ObjectStrings {
    /// `(Any?) -> String`: `value.toString()`, or `"null"`.
    pub(crate) any_to_string: u32,
    /// The `toString` slot body of every enum class: `kotlin.Enum`'s, the constant's `name`.
    pub(crate) enum_name: u32,
    /// `(name: String, this: $Identified) -> String`: `Any.toString`'s default.
    identity_string: u32,
    /// `(name: String, this: Throwable) -> String`: `Throwable.toString`'s default.
    throwable_string: u32,
    /// The `toString` slot body of each class name, by the default it renders.
    slots: HashMap<(TypeName, Rendering), u32>,
    /// The target, whose library chooses the name `Throwable.toString` prints.
    target: WasmTarget,
}

/// Which library default a `toString` slot body renders.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Rendering {
    Identity,
    Throwable,
}

impl ObjectStrings {
    pub(super) fn declare(module: &mut Module, target: WasmTarget) -> Self {
        Self {
            any_to_string: module.declare(),
            enum_name: module.declare(),
            identity_string: module.declare(),
            throwable_string: module.declare(),
            slots: HashMap::default(),
            target,
        }
    }
}

impl Runtime {
    pub(super) fn define_object_strings(&mut self, module: &mut Module) {
        self.define_any_to_string(module);
        self.define_enum_name(module);
        self.define_identity_string(module);
        self.define_throwable_string(module);
    }

    /// The `toString` slot body of `kotlin.Any`'s default for instances of `class`, which extend
    /// `$Identified` and which Kotlin names `names`.
    pub(crate) fn identity_to_string(
        &mut self,
        module: &mut Module,
        class: TypeName,
        names: &ClassNames,
    ) -> u32 {
        self.named_slot(module, class, &names.qualified, Rendering::Identity)
    }

    /// The `toString` slot body of `kotlin.Throwable`'s default for instances of the exception
    /// class `class`, which Kotlin names `names`.
    pub(crate) fn throwable_to_string(
        &mut self,
        module: &mut Module,
        class: TypeName,
        names: &ClassNames,
    ) -> u32 {
        let name = match self.object_strings.target {
            WasmTarget::Js => &names.qualified,
            WasmTarget::Wasi => &names.simple,
        };
        self.named_slot(module, class, name, Rendering::Throwable)
    }

    /// A `toString` slot body answering the constant `text`: the rendering of a class whose every
    /// instance prints alike (`kotlin.Unit`).
    pub(crate) fn constant_to_string(&mut self, module: &mut Module, text: &str) -> u32 {
        let mut code = Code::default();
        self.ascii_constant(module, &mut code, text);
        let function = module.declare();
        let ty = self.objects.any_slot_types[2];
        let locals = Vec::new();
        module.define(function, Function { ty, locals, code });
        function
    }

    /// Define the reference-array `toString` slot as `kotlin.Any`'s default for `kotlin.Array`.
    /// A primitive array's slot is defined when that classifier's object type is declared.
    pub(super) fn define_reference_array_to_string(&mut self, module: &mut Module, function: u32) {
        self.define_array_to_string(module, function, krusty::types::wk::array());
    }

    fn define_array_to_string(&mut self, module: &mut Module, function: u32, class: TypeName) {
        let name = member_class_names(class).qualified;
        self.define_named_slot(module, function, (class, &name), Rendering::Identity);
    }

    /// The slot body rendering `kind` for `class`, whose Kotlin name is `name`: one per class.
    fn named_slot(
        &mut self,
        module: &mut Module,
        class: TypeName,
        name: &str,
        kind: Rendering,
    ) -> u32 {
        if let Some(function) = self.object_strings.slots.get(&(class, kind)) {
            return *function;
        }
        let function = module.declare();
        self.define_named_slot(module, function, (class, name), kind);
        function
    }

    fn define_named_slot(
        &mut self,
        module: &mut Module,
        function: u32,
        (class, name): (TypeName, &str),
        kind: Rendering,
    ) {
        let rendering = match kind {
            Rendering::Identity => self.object_strings.identity_string,
            Rendering::Throwable => self.object_strings.throwable_string,
        };
        let mut code = Code::default();
        self.ascii_constant(module, &mut code, name);
        code.local_get(0).call(rendering);
        let ty = self.objects.any_slot_types[2];
        let locals = Vec::new();
        module.define(function, Function { ty, locals, code });
        self.object_strings.slots.insert((class, kind), function);
    }

    /// Define `function`, the `toString` slot of an unsigned array class: its simple name and its
    /// storage array's contents as the value class renders them, `UIntArray(storage=[1, 2])`,
    /// each element read as the signed number the storage array holds. `object` is the class's array object type and
    /// `storage` the signed array its elements live in.
    pub(crate) fn define_unsigned_array_to_string(
        &mut self,
        module: &mut Module,
        function: u32,
        (object, storage): (u32, u32),
        (name, element): (&str, Ty),
    ) {
        let ty = self.objects.any_slot_types[2];
        let concat = self.string_concat;
        let (elements, index, text) = (1, 2, 3);
        let mut code = Code::default();
        self.ascii_constant(module, &mut code, &format!("{name}(storage=["));
        code.local_set(text)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, crate::array_objects::STORAGE_FIELD)
            .local_set(elements)
            .i32_const(0)
            .local_set(index)
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(index)
            .local_get(elements)
            .ref_as_non_null()
            .array_len()
            .op(GE_S32)
            .br_if(1)
            .local_get(index)
            .if_(BlockType::Empty)
            .local_get(text)
            .ref_as_non_null();
        self.ascii_constant(module, &mut code, ", ");
        code.call(concat)
            .local_set(text)
            .end()
            .local_get(text)
            .ref_as_non_null()
            .local_get(elements)
            .ref_as_non_null()
            .local_get(index);
        // The storage is the signed array of the same width, so each element prints as the
        // signed number: `UIntArray(storage=[-1])` holds the maximum.
        match element {
            Ty::UByte | Ty::UShort => {
                code.array_get_s(storage).call(self.string_of_i32);
            }
            Ty::ULong => {
                code.array_get(storage).call(self.string_of_i64);
            }
            _ => {
                code.array_get(storage).call(self.string_of_i32);
            }
        }
        code.call(concat)
            .local_set(text)
            .local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0)
            .end()
            .end()
            .local_get(text)
            .ref_as_non_null();
        self.ascii_constant(module, &mut code, "])");
        code.call(concat);
        let locals = vec![
            ValType::nullable(storage),
            ValType::I32,
            self.nullable_string_type(),
        ];
        module.define(function, Function { ty, locals, code });
    }

    fn define_any_to_string(&mut self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![REFERENCE], vec![string]);
        let string_heap = HeapType::Concrete(self.string);
        let object = self.objects.object;
        let slot = krusty::backend::class_tables::AnyMember::ToString.slot();
        let mut code = Code::default();
        code.local_get(0)
            .ref_is_null()
            .if_(BlockType::Value(string));
        self.ascii_constant(module, &mut code, "null");
        code.else_()
            .local_get(0)
            .ref_test(false, string_heap)
            .if_(BlockType::Value(string))
            .local_get(0)
            .ref_cast(false, string_heap)
            .else_()
            .local_get(0)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .struct_get(self.objects.any_table, slot)
            .call_ref(self.objects.any_slot_types[slot as usize])
            .ref_as_non_null()
            .end()
            .end();
        let locals = Vec::new();
        module.define(
            self.object_strings.any_to_string,
            Function { ty, locals, code },
        );
    }

    fn define_enum_name(&mut self, module: &mut Module) {
        let base = self.objects.enum_base;
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(base))
            .struct_get(base, ENUM_NAME_FIELD);
        let ty = self.objects.any_slot_types[2];
        let locals = Vec::new();
        module.define(self.object_strings.enum_name, Function { ty, locals, code });
    }

    /// `name + "@" + identityHashCode().toString()`.
    fn define_identity_string(&mut self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![string, REFERENCE], vec![string]);
        let identified = self.objects.identified;
        let mut code = Code::default();
        code.local_get(0);
        self.ascii_constant(module, &mut code, "@");
        code.call(self.string_concat)
            .local_get(1)
            .ref_cast(false, HeapType::Concrete(identified))
            .struct_get(identified, HASH_FIELD)
            .call(self.string_of_i32)
            .call(self.string_concat);
        let locals = Vec::new();
        module.define(
            self.object_strings.identity_string,
            Function { ty, locals, code },
        );
    }

    /// `name`, then `": " + message` when the message is not `null`.
    fn define_throwable_string(&mut self, module: &mut Module) {
        let string = self.string_type();
        let ty = module.func_type(vec![string, REFERENCE], vec![string]);
        let message = 2;
        let mut code = Code::default();
        code.local_get(1);
        self.exceptions.message(&mut code);
        code.local_tee(message)
            .ref_is_null()
            .if_(BlockType::Value(string))
            .local_get(0)
            .else_()
            .local_get(0);
        self.ascii_constant(module, &mut code, ": ");
        code.call(self.string_concat)
            .local_get(message)
            .ref_as_non_null()
            .call(self.string_concat)
            .end();
        let locals = vec![self.nullable_string_type()];
        module.define(
            self.object_strings.throwable_string,
            Function { ty, locals, code },
        );
    }
}
