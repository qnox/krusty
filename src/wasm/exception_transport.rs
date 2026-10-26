//! The module-owned Kotlin exception tag and base Throwable storage.
//!
//! A throw transports the very same GC reference; no host exception wrapper or replacement Kotlin
//! object crosses a catch boundary. The base carrier extends the ordinary object prefix so later
//! hierarchy support can extend its layout without changing exception transport.

use super::encode::{Code, CompositeType, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot, REFERENCE};

pub(super) struct ExceptionTransport {
    pub(super) tag: u32,
    structure: u32,
    table: u32,
}

impl ExceptionTransport {
    pub(super) fn declare(module: &mut Module, objects: &mut ObjectRoot, string: ValType) -> Self {
        let tag = module.exception_tag(vec![REFERENCE]);
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(string),
                immutable(REFERENCE),
            ]),
            supertype: Some(objects.object),
            open: true,
        });
        let mut table = Code::default();
        for (slot, ty) in objects.any_slot_types.into_iter().enumerate() {
            let function = if slot == 0 {
                objects.identity_equals
            } else {
                objects.trap(module, ty)
            };
            module.reference_function(function);
            table.ref_func(function);
        }
        table
            .ref_null(HeapType::Concrete(objects.itable))
            .struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), table);
        Self {
            tag,
            structure,
            table,
        }
    }

    /// The nullable message is already evaluated into a local. Both supported constructors have
    /// no cause; retain the null cause slot rather than inventing a different base layout later.
    pub(super) fn construct(&self, code: &mut Code, message: Option<u32>, string: u32) {
        code.global_get(self.table);
        match message {
            Some(message) => code.local_get(message),
            None => code.ref_null(HeapType::Concrete(string)),
        };
        code.ref_null(HeapType::Eq).struct_new(self.structure);
    }

    pub(super) fn message(&self, code: &mut Code) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, 1);
    }
}
