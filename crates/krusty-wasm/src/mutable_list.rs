//! A packed `ArrayList`: the object `arrayListOf` and `mutableListOf` allocate.
//!
//! The elements are a copy of the vararg array, so a later write does not alias the caller's
//! storage. `equals`, `hashCode` and `toString` are the list's own `Any` slots. Erased `==`,
//! `hashCode()` and string templates reach those slots through `object_equals`, `hash_code` and
//! `any_to_string`, so the slots answer structurally: content equality, `31 * h + element.hash`,
//! and `[a, b]` rendering. An element that is this list renders as `(this Collection)`.

use super::encode::{
    BlockType, Code, CompositeType, FieldType, Function, HeapType, Module, StorageType, SubType,
    ValType,
};
use super::objects::immutable;
use super::runtime::{Runtime, ADD32, GE_U32, MUL32, NE32};

const ELEMENTS: u32 = 1;
const SIZE: u32 = 2;

pub(super) struct MutableList {
    pub(super) structure: u32,
    pub(super) table: u32,
    /// `(array (mut (ref null eq)))`, the same storage a reference array holds.
    pub(super) elements: u32,
}

impl Runtime {
    pub(super) fn declare_mutable_list(
        &mut self,
        module: &mut Module,
        references: u32,
    ) -> MutableList {
        let any_table = self.objects.any_table;
        let object = self.objects.object;
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(any_table)),
                FieldType {
                    storage: StorageType::Val(ValType::reference(references)),
                    mutable: true,
                },
                FieldType {
                    storage: StorageType::Val(ValType::I32),
                    mutable: true,
                },
            ]),
            supertype: Some(object),
            open: false,
        });
        let [equals_ty, hash_ty, to_string_ty] = self.objects.any_slot_types;
        let equals = self.list_equals(module, structure, references, equals_ty);
        let hash_code = self.list_hash_code(module, structure, references, hash_ty);
        let to_string = self.list_to_string(module, structure, references, to_string_ty);
        let itable = self.objects.itable;
        let (_, itable_global) = self
            .objects
            .interfaces
            .class(module, itable, vec![structure]);
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable_global);
        // No type information describes the runtime's list yet: the field is null, as for any
        // runtime object no classifier record covers.
        self.objects.push_type_info(&mut init, None);
        init.struct_new(any_table);
        let table = module.constant_global(ValType::reference(any_table), init);
        MutableList {
            structure,
            table,
            elements: references,
        }
    }

    /// `equals(this, other)`: same size and pairwise `==`, and false when `other` is not this
    /// list representation. A list compared with itself is true before its elements are read.
    fn list_equals(&self, module: &mut Module, structure: u32, references: u32, ty: u32) -> u32 {
        let heap = HeapType::Concrete(structure);
        let (this, other, index) = (0, 1, 2);
        let mut code = Code::default();
        code.local_get(other)
            .ref_test(false, heap)
            .if_(BlockType::Empty)
            .else_()
            .i32_const(0)
            .return_()
            .end();
        code.local_get(this)
            .local_get(other)
            .ref_eq()
            .if_(BlockType::Empty)
            .i32_const(1)
            .return_()
            .end();
        code.local_get(this)
            .ref_cast(false, heap)
            .struct_get(structure, SIZE);
        code.local_get(other)
            .ref_cast(false, heap)
            .struct_get(structure, SIZE)
            .op(NE32)
            .if_(BlockType::Empty)
            .i32_const(0)
            .return_()
            .end();
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index)
            .local_get(this)
            .ref_cast(false, heap)
            .struct_get(structure, SIZE)
            .op(GE_U32)
            .br_if(1);
        element(&mut code, structure, references, this, index);
        element(&mut code, structure, references, other, index);
        code.call(self.object_equals)
            .if_(BlockType::Empty)
            .else_()
            .i32_const(0)
            .return_()
            .end();
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().end().i32_const(1);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: vec![ValType::I32],
                code,
            },
        );
        function
    }

    /// `hashCode(this)`: `1`, then `31 * hash + element.hashCode()` for each element, a null
    /// element contributing `0`.
    fn list_hash_code(&self, module: &mut Module, structure: u32, references: u32, ty: u32) -> u32 {
        let (this, index, hash) = (0, 1, 2);
        let mut code = Code::default();
        code.i32_const(1).local_set(hash);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index)
            .local_get(this)
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, SIZE)
            .op(GE_U32)
            .br_if(1);
        code.local_get(hash).i32_const(31).op(MUL32);
        element(&mut code, structure, references, this, index);
        code.call(self.hashing.hash_code).op(ADD32).local_set(hash);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().end().local_get(hash);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: vec![ValType::I32, ValType::I32],
                code,
            },
        );
        function
    }

    /// `toString(this)`: `[a, b]`, each element through `any_to_string`, a self-element as
    /// `(this Collection)`.
    fn list_to_string(
        &mut self,
        module: &mut Module,
        structure: u32,
        references: u32,
        ty: u32,
    ) -> u32 {
        let (this, index, text, element_local, rendered_local) = (0, 1, 2, 3, 4);
        let rendered = self.string_type();
        let mut code = Code::default();
        self.text(module, &mut code, "[");
        code.local_set(text);
        code.block(BlockType::Empty).loop_(BlockType::Empty);
        code.local_get(index)
            .local_get(this)
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, SIZE)
            .op(GE_U32)
            .br_if(1);
        code.local_get(index).if_(BlockType::Empty);
        code.local_get(text).ref_as_non_null();
        self.text(module, &mut code, ", ");
        code.call(self.string_concat).local_set(text).end();
        element(&mut code, structure, references, this, index);
        code.local_tee(element_local)
            .local_get(this)
            .ref_eq()
            .if_(BlockType::Value(rendered));
        self.text(module, &mut code, "(this Collection)");
        code.else_()
            .local_get(element_local)
            .call(self.object_strings.any_to_string)
            .end();
        code.local_set(rendered_local);
        code.local_get(text).ref_as_non_null();
        code.local_get(rendered_local)
            .call(self.string_concat)
            .local_set(text);
        code.local_get(index)
            .i32_const(1)
            .op(ADD32)
            .local_set(index)
            .br(0);
        code.end().end();
        code.local_get(text).ref_as_non_null();
        self.text(module, &mut code, "]");
        code.call(self.string_concat);
        let function = module.declare();
        module.define(
            function,
            Function {
                ty,
                locals: vec![
                    ValType::I32,
                    self.nullable_string_type(),
                    super::objects::REFERENCE,
                    rendered,
                ],
                code,
            },
        );
        function
    }
}

/// Push `list[index]`. `list` may be nullable eq; the caller has already proved it is this list.
fn element(code: &mut Code, structure: u32, references: u32, list: u32, index: u32) {
    code.local_get(list)
        .ref_cast(false, HeapType::Concrete(structure))
        .struct_get(structure, ELEMENTS)
        .local_get(index)
        .array_get(references);
}
