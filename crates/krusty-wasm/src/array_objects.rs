//! Kotlin arrays are objects whose mutable GC storage is separate from their identity.
//!
//! A reference array (`Array<T>`) stores references, a scalar element as its box. A primitive
//! array (`IntArray`, `CharArray`, …) stores its element's own value, a narrow one packed to its
//! width. Each classifier is its own object type, declared the first time a module uses it, so a
//! later type test can tell an `IntArray` from a `FloatArray`; tests and casts against array types
//! still decline today.

mod value_methods;

use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TyKind};

use super::encode::{Code, CompositeType, FieldType, Module, StorageType, SubType, ValType};
use super::identity::IdentityObjects;
use super::objects::{immutable, ObjectRoot};
use super::runtime::Runtime;

pub(super) const STORAGE_FIELD: u32 = super::identity::PREFIX_FIELDS;

/// The field of an unsigned array object holding the signed array object it wraps: the signed
/// array keeps its identity through the view, so unwrapping answers the one it was made from.
pub(super) const VIEW_FIELD: u32 = STORAGE_FIELD + 1;

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
    /// For an unsigned array class, the signed array object type its [`VIEW_FIELD`] refers to and
    /// the global holding that type's dispatch table.
    pub(super) view: Option<(u32, u32)>,
}

/// The array object types this module has declared.
pub(super) struct ArrayObjects {
    reference: ArrayForm,
    /// Primitive arrays by element type.
    primitives: HashMap<Ty, ArrayForm>,
    /// Unsigned array classes by element type.
    unsigned: HashMap<Ty, ArrayForm>,
}

impl ArrayObjects {
    pub(super) fn declare(
        module: &mut Module,
        objects: &mut ObjectRoot,
        identity: &IdentityObjects,
        storage: u32,
        to_string: u32,
    ) -> Self {
        let methods = [objects.identity_equals, identity.hash_code, to_string];
        let (object, table) = declare_object(
            module,
            objects,
            identity,
            storage,
            krusty::types::wk::array(),
            (methods, None),
        );
        Self {
            reference: ArrayForm {
                object,
                storage,
                table,
                read: ElementRead::Plain,
                view: None,
            },
            primitives: HashMap::default(),
            unsigned: HashMap::default(),
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
        if let Some(signed) = unsigned_carrier_element(element) {
            // An unsigned array class is a value class over the signed array: its own object
            // type, so `is UIntArray` and `is IntArray` tell them apart, wrapping the signed
            // array's storage and reading it back with its own element's width.
            if let Some(form) = self.arrays.unsigned.get(&element) {
                return Some((*form, element));
            }
            let (signed_form, _) = self.array_form(module, Ty::array(signed))?;
            let (_, read) = primitive_storage(element)?;
            let class = krusty::types::type_name(
                krusty::types::prim_array_name(element)
                    .expect("an unsigned array element names its class"),
            );
            let names = krusty::backend::class_names::member_class_names(class);
            let to_string = module.declare();
            let equals = module.declare();
            let hash_code = module.declare();
            let (object, table) = declare_object(
                module,
                &mut self.objects,
                &self.identity,
                signed_form.storage,
                class,
                ([equals, hash_code, to_string], Some(signed_form.object)),
            );
            self.define_unsigned_array_value_methods(
                module,
                (equals, hash_code),
                object,
                signed_form,
                signed,
            );
            self.define_unsigned_array_to_string(
                module,
                to_string,
                (object, signed_form.storage),
                (&names.simple, element),
            );
            let form = ArrayForm {
                object,
                storage: signed_form.storage,
                table,
                read,
                view: Some((signed_form.object, signed_form.table)),
            };
            self.arrays.unsigned.insert(element, form);
            return Some((form, element));
        }
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
        let methods = [
            self.objects.identity_equals,
            self.identity.hash_code,
            to_string,
        ];
        let (object, table) = declare_object(
            module,
            &mut self.objects,
            &self.identity,
            storage,
            class,
            (methods, None),
        );
        let form = ArrayForm {
            object,
            storage,
            table,
            read,
            view: None,
        };
        self.arrays.primitives.insert(element, form);
        Some((form, element))
    }
}

/// The signed element whose array an unsigned array class (`UIntArray`) wraps.
fn unsigned_carrier_element(element: Ty) -> Option<Ty> {
    Some(match element.kind() {
        TyKind::UBYTE => Ty::Byte,
        TyKind::USHORT => Ty::Short,
        TyKind::UINT => Ty::Int,
        TyKind::ULONG => Ty::Long,
        _ => return None,
    })
}

/// The storage of a primitive array's element, and how a read of it widens; `None` for an element
/// that has no storage of its own.
fn primitive_storage(element: Ty) -> Option<(StorageType, ElementRead)> {
    Some(match element.kind() {
        TyKind::UBYTE => (StorageType::I8, ElementRead::Unsigned),
        TyKind::USHORT => (StorageType::I16, ElementRead::Unsigned),
        TyKind::UINT => (StorageType::Val(ValType::I32), ElementRead::Plain),
        TyKind::ULONG => (StorageType::Val(ValType::I64), ElementRead::Plain),
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
/// equality, hash and rendering methods. Primitive arrays use identity methods; unsigned wrappers
/// compare backing identity and hash its signed contents.
fn declare_object(
    module: &mut Module,
    objects: &mut ObjectRoot,
    identity: &IdentityObjects,
    storage: u32,
    class: krusty::types::TypeName,
    (methods, view): ([u32; 3], Option<u32>),
) -> (u32, u32) {
    let mut fields = vec![
        immutable(ValType::reference(objects.any_table)),
        immutable(ValType::I32),
        immutable(ValType::reference(storage)),
    ];
    if let Some(view) = view {
        fields.push(immutable(ValType::reference(view)));
    }
    let object = module.subtype(SubType {
        composite: CompositeType::Struct(fields),
        supertype: Some(identity.object),
        open: false,
    });
    let (_, itable) = objects
        .interfaces
        .class(module, objects.itable, vec![object]);
    let info = objects.type_infos.global(module, class);
    let mut init = Code::default();
    for function in methods {
        module.reference_function(function);
        init.ref_func(function);
    }
    init.global_get(itable);
    objects.push_type_info(&mut init, Some(info));
    init.struct_new(objects.any_table);
    let table = module.constant_global(ValType::reference(objects.any_table), init);
    (object, table)
}
