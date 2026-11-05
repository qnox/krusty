//! Reference arrays are objects whose mutable GC storage is separate from their identity.

use super::encode::{Code, CompositeType, Module, SubType, ValType};
use super::identity::IdentityObjects;
use super::objects::{immutable, ObjectRoot};

pub(super) const STORAGE_FIELD: u32 = super::identity::PREFIX_FIELDS;

pub(super) struct ReferenceArrays {
    pub(super) object: u32,
    pub(super) table: u32,
}

impl ReferenceArrays {
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
        storage: u32,
    ) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(ValType::reference(storage)),
            ]),
            supertype: Some(identity.object),
            open: false,
        });
        let to_string = objects.trap(module, objects.any_slot_types[2]);
        let (_, itable) = objects.interfaces.class(module, objects.itable, vec![object]);
        let mut init = Code::default();
        for function in [objects.identity_equals, identity.hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        Self { object, table }
    }
}
