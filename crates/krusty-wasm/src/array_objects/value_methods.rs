//! The value-class methods of unsigned array views, over signed array storage.

use super::{ArrayForm, STORAGE_FIELD, VIEW_FIELD};
use crate::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use crate::hashing::ArrayHash;
use crate::runtime::Runtime;
use krusty::backend::class_tables::AnyMember;
use krusty::types::Ty;

impl Runtime {
    pub(super) fn define_unsigned_array_value_methods(
        &self,
        module: &mut Module,
        (equals, hash_code): (u32, u32),
        object: u32,
        signed: ArrayForm,
        element: Ty,
    ) {
        let mut code = Code::default();
        code.local_get(1)
            .ref_test(false, HeapType::Concrete(object))
            .if_(BlockType::Value(ValType::I32))
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, VIEW_FIELD)
            .local_get(1)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, VIEW_FIELD)
            .ref_eq()
            .else_()
            .i32_const(0)
            .end();
        module.define(
            equals,
            Function {
                ty: self.objects.any_slot_types[AnyMember::Equals.slot() as usize],
                locals: Vec::new(),
                code,
            },
        );

        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, STORAGE_FIELD)
            .local_set(1);
        assert!(self.hashing.emit_array_content_hash(
            &mut code,
            ArrayHash {
                storage: signed.storage,
                read: signed.read,
                element,
                reference: false,
                values: 1,
                index: 2,
                hash: 3,
            },
        ));
        module.define(
            hash_code,
            Function {
                ty: self.objects.any_slot_types[AnyMember::HashCode.slot() as usize],
                locals: vec![
                    ValType::reference(signed.storage),
                    ValType::I32,
                    ValType::I32,
                ],
                code,
            },
        );
    }
}
