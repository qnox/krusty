//! Kotlin arrays are objects whose mutable GC storage is separate from their identity.
//!
//! A reference array (`Array<T>`) stores references, a scalar element as its box. A primitive
//! array (`IntArray`, `CharArray`, …) stores its element's own value, a narrow one packed to its
//! width. Each classifier is its own object type, declared the first time a module uses it, so a
//! later type test can tell an `IntArray` from a `FloatArray`; tests and casts against array types
//! still decline today.

use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TyKind};

use super::encode::{Code, CompositeType, FieldType, Module, StorageType, SubType, ValType};
use super::identity::IdentityObjects;
use super::objects::{immutable, ObjectRoot};
use super::runtime::Runtime;

pub(super) const STORAGE_FIELD: u32 = super::identity::PREFIX_FIELDS;

/// How an element is read out of its storage: as stored, or widened from a packed width with or
/// without its sign.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ElementRead {
    Plain,
    Signed,
    Unsigned,
}

/// One array classifier's object type, its storage array type and its dispatch table's global.
#[derive(Clone, Copy)]
pub(super) struct ArrayForm {
    pub(super) object: u32,
    pub(super) storage: u32,
    pub(super) table: u32,
    pub(super) read: ElementRead,
}

/// The array object types this module has declared.
pub(super) struct ArrayObjects {
    reference: ArrayForm,
    /// Primitive arrays by element type.
    primitives: HashMap<Ty, ArrayForm>,
}

impl ArrayObjects {
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
        storage: u32,
        to_string: u32,
    ) -> Self {
        let (object, table) = declare_object(module, objects, identity, storage, to_string);
        Self {
            reference: ArrayForm {
                object,
                storage,
                table,
                read: ElementRead::Plain,
            },
            primitives: HashMap::default(),
        }
    }
}

impl Runtime {
    /// The form of `array`'s values and the type of the element its storage holds: `Any?` for a
    /// reference array, the primitive itself for a primitive one.
    pub(super) fn array_form(&mut self, module: &mut Module, array: Ty) -> Option<(ArrayForm, Ty)> {
        let array = array.non_null();
        if array.is_reference_array() {
            let any = Ty::nullable(Ty::obj_name(krusty::types::wk::any()));
            return Some((self.arrays.reference, any));
        }
        if !array.is_array() {
            return None;
        }
        let element = array.array_elem()?;
        let (packed, read) = primitive_storage(element)?;
        if let Some(form) = self.arrays.primitives.get(&element) {
            return Some((*form, element));
        }
        let storage = module.ty(CompositeType::Array(FieldType {
            storage: packed,
            mutable: true,
        }));
        let class = krusty::types::type_name(
            krusty::types::prim_array_name(element)
                .expect("a primitive array element names its class"),
        );
        let names = krusty::backend::class_names::member_class_names(class);
        let to_string = self.identity_to_string(module, class, &names);
        let (object, table) = declare_object(
            module,
            &mut self.objects,
            &self.identity,
            storage,
            to_string,
        );
        let form = ArrayForm {
            object,
            storage,
            table,
            read,
        };
        self.arrays.primitives.insert(element, form);
        Some((form, element))
    }
}

/// The storage of a primitive array's element; `None` for an element that is not a primitive (an
/// unsigned array is a value class over a signed one).
fn primitive_storage(element: Ty) -> Option<(StorageType, ElementRead)> {
    Some(match element.kind() {
        TyKind::BOOLEAN => (StorageType::I8, ElementRead::Unsigned),
        TyKind::BYTE => (StorageType::I8, ElementRead::Signed),
        TyKind::SHORT => (StorageType::I16, ElementRead::Signed),
        TyKind::CHAR => (StorageType::I16, ElementRead::Unsigned),
        TyKind::INT => (StorageType::Val(ValType::I32), ElementRead::Plain),
        TyKind::LONG => (StorageType::Val(ValType::I64), ElementRead::Plain),
        TyKind::FLOAT => (StorageType::Val(ValType::F32), ElementRead::Plain),
        TyKind::DOUBLE => (StorageType::Val(ValType::F64), ElementRead::Plain),
        _ => return None,
    })
}

/// Declare an array object type over `storage` and the global holding its dispatch table: identity
/// equality and hash, and `to_string` (`kotlin.Any`'s default for that array class).
fn declare_object(
    module: &mut Module,
    objects: &mut ObjectRoot,
    identity: &IdentityObjects,
    storage: u32,
    to_string: u32,
) -> (u32, u32) {
    let object = module.subtype(SubType {
        composite: CompositeType::Struct(vec![
            immutable(ValType::reference(objects.any_table)),
            immutable(ValType::I32),
            immutable(ValType::reference(storage)),
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
    init.global_get(itable).struct_new(objects.any_table);
    let table = module.constant_global(ValType::reference(objects.any_table), init);
    (object, table)
}
