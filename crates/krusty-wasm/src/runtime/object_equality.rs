//! Kotlin structural equality for runtime references, including callable-reference receivers.

use super::*;
use crate::objects::REFERENCE;

impl Runtime {
    pub(super) fn define_object_equals(&self, module: &mut Module) {
        let string = HeapType::Concrete(self.string);
        let object = self.objects.object;
        let mut code = Code::default();
        code.local_get(0)
            .ref_is_null()
            .if_(BlockType::Value(ValType::I32))
            .local_get(1)
            .ref_is_null()
            .else_()
            .local_get(0)
            .ref_test(false, string)
            .if_(BlockType::Value(ValType::I32))
            .local_get(1)
            .ref_test(false, string)
            .if_(BlockType::Value(ValType::I32))
            .local_get(0)
            .ref_cast(false, string)
            .local_get(1)
            .ref_cast(false, string)
            .call(self.string_equals)
            .else_()
            .i32_const(0)
            .end()
            .else_()
            .local_get(0)
            .ref_test(false, HeapType::Concrete(object))
            .if_(BlockType::Value(ValType::I32))
            .local_get(0)
            .local_get(1)
            .local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, 0)
            .struct_get(self.objects.any_table, 0)
            .call_ref(self.objects.any_slot_types[0])
            .else_()
            .local_get(0)
            .local_get(1)
            .ref_eq()
            .end()
            .end()
            .end();
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        module.define(
            self.object_equals,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
    }
}
