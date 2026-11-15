//! `kotlin.Pair`, laid out by the Wasm runtime.
//!
//! `Pair` is generic, so a support unit does not materialize its class. A dependency body that
//! destructures a pair (`component1`/`component2`, or the `first`/`second` properties) reads the
//! two reference fields of this struct. `Pair(first, second)` allocates the same struct, so a
//! pair a source `to` builds and a pair a stdlib body reads share one layout.

use super::encode::{Code, CompositeType, Module, SubType, ValType};
use super::identity::IdentityObjects;
use super::objects::{immutable, ObjectRoot, REFERENCE};

/// The first component, after the identity object's dispatch table and hash.
pub(super) const FIRST_FIELD: u32 = super::identity::PREFIX_FIELDS;
/// The second component.
pub(super) const SECOND_FIELD: u32 = FIRST_FIELD + 1;

/// The runtime's `Pair` type and the dispatch table its instances carry.
pub(super) struct Pairs {
    pub(super) object: u32,
    pub(super) table: u32,
    /// `kotlin.Pair`'s `toString` slot: `kotlin.Pair@` and the identity hash.
    pub(super) to_string: u32,
}

impl Pairs {
    /// Declare the pair struct, a subtype of the identity object, and the table its instances
    /// share. The `toString` slot is defined once `Any`'s identity rendering exists.
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
    ) -> Self {
        let to_string = module.declare();
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(ValType::I32),
                immutable(REFERENCE),
                immutable(REFERENCE),
            ]),
            supertype: Some(identity.object),
            open: false,
        });
        let (_, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![object]);
        let mut init = Code::default();
        for function in [objects.identity_equals, identity.hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable);
        objects.push_type_info(&mut init, None);
        init.struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        Self {
            object,
            table,
            to_string,
        }
    }
}
