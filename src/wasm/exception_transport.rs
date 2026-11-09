//! The module-owned Kotlin exception tag and base Throwable storage.
//!
//! A throw transports the very same GC reference; no host exception wrapper or replacement Kotlin
//! object crosses a catch boundary. The base carrier extends the ordinary object prefix so later
//! hierarchy support can extend its layout without changing exception transport.

use super::encode::{
    Code, CompositeType, FieldType, HeapType, Module, StorageType, SubType, ValType,
};
use super::objects::{immutable, ObjectRoot, REFERENCE};

mod class_registry;

pub(super) struct ExceptionTransport {
    pub(super) tag: u32,
    structure: u32,
    table: u32,
    classes: class_registry::RuntimeClasses,
}

impl ExceptionTransport {
    pub(super) fn declare(module: &mut Module, objects: &mut ObjectRoot, string: ValType) -> Self {
        let tag = module.exception_tag(vec![REFERENCE]);
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(
                std::iter::once(immutable(ValType::reference(objects.any_table)))
                    .chain(Self::base_fields(string))
                    .collect(),
            ),
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
            classes: class_registry::RuntimeClasses::new(structure, objects.any_table, string),
        }
    }

    /// The message and cause every exception struct carries after its dispatch table. A
    /// constructor of a class extending a runtime exception writes them once its object exists,
    /// so they are mutable.
    pub(super) fn base_fields(string: ValType) -> [FieldType; 2] {
        [string, REFERENCE].map(|value| FieldType {
            storage: StorageType::Val(value),
            mutable: true,
        })
    }

    /// Store the `message` and `cause` locals into the exception fields of the `object` local.
    pub(super) fn store_base(&self, code: &mut Code, object: u32, message: u32, cause: u32) {
        code.local_get(object)
            .ref_cast(false, HeapType::Concrete(self.structure))
            .local_get(message)
            .struct_set(self.structure, MESSAGE_FIELD)
            .local_get(object)
            .ref_cast(false, HeapType::Concrete(self.structure))
            .local_get(cause)
            .struct_set(self.structure, CAUSE_FIELD);
    }

    pub(super) fn ensure_class(
        &mut self,
        module: &mut Module,
        source: &dyn crate::backend::BackendClassifierSource,
        identity: crate::types::TypeName,
    ) -> Result<u32, String> {
        self.classes.ensure(module, source, identity)
    }

    /// The nullable message is already evaluated into a local. These constructors have no cause;
    /// preserve the shared cause slot and allocate the authenticated selected subtype.
    pub(super) fn construct_class(
        &self,
        code: &mut Code,
        structure: u32,
        message: Option<u32>,
        string: u32,
    ) {
        code.global_get(self.table);
        match message {
            Some(message) => code.local_get(message),
            None => code.ref_null(HeapType::Concrete(string)),
        };
        code.ref_null(HeapType::Eq).struct_new(structure);
    }

    pub(super) fn message(&self, code: &mut Code) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, MESSAGE_FIELD);
    }
}

/// The exception struct fields holding the message and the cause.
const MESSAGE_FIELD: u32 = 1;
const CAUSE_FIELD: u32 = 2;
