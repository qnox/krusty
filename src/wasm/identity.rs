//! Stable identity hashes on runtime-owned objects. The immutable hash travels with the object,
//! so hashing neither retains otherwise dead values nor depends on mutable object contents.

use super::encode::{Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::{immutable, ObjectRoot};

pub(super) const HASH_FIELD: u32 = 1;
pub(super) const PREFIX_FIELDS: u32 = 2;

pub(super) struct IdentityObjects {
    pub(super) object: u32,
    pub(super) allocate: u32,
    pub(super) hash_code: u32,
}

impl IdentityObjects {
    pub(super) fn declare(module: &mut Module, objects: &ObjectRoot) -> Self {
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
            ]),
            supertype: Some(objects.object),
            open: true,
        });
        let counter = module.global(ValType::I32);
        let allocate_type = module.func_type(Vec::new(), vec![ValType::I32]);
        let allocate = module.declare();
        let mut code = Code::default();
        code.global_get(counter)
            .i32_const(1)
            .op(super::runtime::ADD32)
            .global_set(counter)
            .global_get(counter);
        module.define(
            allocate,
            Function {
                ty: allocate_type,
                locals: Vec::new(),
                code,
            },
        );
        let hash_code = module.declare();
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(object))
            .struct_get(object, HASH_FIELD);
        module.define(
            hash_code,
            Function {
                ty: objects.any_slot_types[1],
                locals: Vec::new(),
                code,
            },
        );
        Self {
            object,
            allocate,
            hash_code,
        }
    }
}
