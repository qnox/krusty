//! The root of the object model: what every object carries, and `kotlin.Any`'s three members.
//!
//! **An object** is a struct whose first field is its class's dispatch table. That field is typed
//! as `$AnyTable` in every class, so it is one field of `$Object`, the struct every class's type is
//! declared a subtype of: a dispatch reads it off any object before knowing the object's class.
//! `$AnyTable` holds `kotlin.Any`'s three slots — `equals`, `hashCode`, `toString`, in the order
//! every class table keeps them — then the class's interface table (`super::interface_dispatch`)
//! and its instances' type information (`super::class_tokens`), and every table type is declared a
//! subtype of it. A class's own slots follow, so class slot `n` from the fourth on is table field
//! `n + 2` ([`slot_field`]).
//!
//! **References** of class, interface and `Any` types are carried as `(ref null eq)`, the common
//! supertype of an object and a `String`. A slot's function takes its receiver that way too, so an
//! override never needs a different signature from the member it overrides.
//!
//! `kotlin.Any`'s `equals` defaults to identity here. Its `hashCode` and `toString` read the
//! identity hash every object of a declared class carries (`$Identified`, see `super::identity`);
//! `super::runtime` writes those defaults, and a slot with no body gets the trap every abstract
//! slot gets.

use krusty::fx_hash::HashMap;

use super::class_tokens::TypeInfos;
use super::encode::{
    CompositeType, FieldType, Function, HeapType, Module, StorageType, SubType, ValType,
};
use super::interface_dispatch::InterfaceDispatch;

/// Any object, interface value or `Any` value: `(ref null eq)`.
pub(super) const REFERENCE: ValType = ValType::Ref {
    nullable: true,
    heap: HeapType::Eq,
};

/// The field of every dispatch table holding the class's interface table: after `kotlin.Any`'s
/// three slots.
pub(super) const ITABLE_FIELD: u32 = 3;

/// The field of every dispatch table holding the type information record of its instances' class,
/// or null for a runtime object no classifier describes.
pub(super) const TYPE_INFO_FIELD: u32 = ITABLE_FIELD + 1;

/// The fixed erased `FunctionN.invoke` entry, after the common object table prefix.
pub(super) const FUNCTION_FIELD: u32 = TYPE_INFO_FIELD + 1;

/// The field of `$Enum` holding `kotlin.Enum`'s `name`, after the identity prefix; `ordinal`
/// follows it.
pub(super) const ENUM_NAME_FIELD: u32 = super::identity::PREFIX_FIELDS;

/// The dispatch table field holding class slot `slot`.
pub(super) fn slot_field(slot: u32) -> u32 {
    if slot < ITABLE_FIELD {
        slot
    } else {
        slot + 2
    }
}

/// Type and function indices of the object model, fixed when the module is started, and the
/// interface dispatch every file records into.
pub(super) struct ObjectRoot {
    /// `$Itable`: `(array funcref)`, a class's interface table.
    pub(super) itable: u32,
    /// `$AnyTable`: `kotlin.Any`'s three slots, the interface table, then the type information.
    pub(super) any_table: u32,
    /// `$FunctionTable`: `$AnyTable` plus an erased `FunctionN.invoke` entry.
    pub(super) function_table: u32,
    /// `$Object`: the dispatch table, and nothing else yet.
    pub(super) object: u32,
    /// `$Identified`: `$Object`, then the object's immutable identity hash
    /// (`super::identity`), which every object with `kotlin.Any`'s identity semantics extends.
    pub(super) identified: u32,
    /// `$Enum`: `$Identified`, then `kotlin.Enum`'s `name` and `ordinal`, which every enum class's
    /// struct extends.
    pub(super) enum_base: u32,
    /// The function types of `kotlin.Any`'s three slots, in slot order.
    pub(super) any_slot_types: [u32; 3],
    /// `kotlin.Any.equals`'s default: identity.
    pub(super) identity_equals: u32,
    /// A function that traps, per function type: what an abstract slot holds.
    traps: HashMap<u32, u32>,
    pub(super) interfaces: InterfaceDispatch,
    /// One type information record per classifier.
    pub(super) type_infos: TypeInfos,
}

impl ObjectRoot {
    /// Declare the root types over the runtime's `$chars` and `$String` types.
    pub(super) fn declare(module: &mut Module, chars: u32, string_type: u32) -> Self {
        let string = ValType::nullable(string_type);
        let type_infos = TypeInfos::declare(module, chars, string_type);
        let equals = module.func_type(vec![REFERENCE, REFERENCE], vec![ValType::I32]);
        let hash_code = module.func_type(vec![REFERENCE], vec![ValType::I32]);
        let to_string = module.func_type(vec![REFERENCE], vec![string]);
        let any_slot_types = [equals, hash_code, to_string];
        let itable = module.ty(CompositeType::Array(immutable(ValType::Ref {
            nullable: true,
            heap: HeapType::Func,
        })));
        let any_table = module.subtype(SubType {
            composite: CompositeType::Struct(
                any_slot_types
                    .iter()
                    .map(|ty| immutable(ValType::reference(*ty)))
                    .chain([
                        immutable(ValType::nullable(itable)),
                        immutable(ValType::nullable(type_infos.ty)),
                    ])
                    .collect(),
            ),
            supertype: None,
            open: true,
        });
        let function_table = module.subtype(SubType {
            composite: CompositeType::Struct(
                any_slot_types
                    .iter()
                    .map(|ty| immutable(ValType::reference(*ty)))
                    .chain([
                        immutable(ValType::nullable(itable)),
                        immutable(ValType::nullable(type_infos.ty)),
                        immutable(ValType::Ref {
                            nullable: true,
                            heap: HeapType::Func,
                        }),
                    ])
                    .collect(),
            ),
            supertype: Some(any_table),
            open: true,
        });
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![immutable(ValType::reference(any_table))]),
            supertype: None,
            open: true,
        });
        let identified = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(any_table)),
                immutable(ValType::I32),
            ]),
            supertype: Some(object),
            open: true,
        });
        let enum_base = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(any_table)),
                immutable(ValType::I32),
                immutable(string),
                immutable(ValType::I32),
            ]),
            supertype: Some(identified),
            open: true,
        });
        let identity_equals = module.declare();
        let mut code = super::encode::Code::default();
        code.local_get(0).local_get(1).ref_eq();
        module.define(
            identity_equals,
            Function {
                ty: equals,
                locals: Vec::new(),
                code,
            },
        );
        Self {
            itable,
            any_table,
            function_table,
            object,
            identified,
            enum_base,
            any_slot_types,
            identity_equals,
            traps: HashMap::default(),
            interfaces: InterfaceDispatch::default(),
            type_infos,
        }
    }

    /// Push a table's type information field: the record held by the global `info`, or null for
    /// a runtime object no classifier describes.
    pub(super) fn push_type_info(&self, code: &mut super::encode::Code, info: Option<u32>) {
        match info {
            Some(global) => code.global_get(global),
            None => code.ref_null(HeapType::Concrete(self.type_infos.ty)),
        };
    }

    /// Write what the interface dispatch of the whole program needs: see
    /// `super::interface_dispatch`.
    pub(super) fn finalize_interfaces(&mut self, module: &mut Module) {
        std::mem::take(&mut self.interfaces).finalize(module, self);
    }

    /// A function of type `ty` that traps: the body of an abstract slot, and of a `kotlin.Any`
    /// member this module has no body for.
    pub(super) fn trap(&mut self, module: &mut Module, ty: u32) -> u32 {
        if let Some(function) = self.traps.get(&ty) {
            return *function;
        }
        let function = module.declare();
        let mut code = super::encode::Code::default();
        code.unreachable();
        module.define(
            function,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        self.traps.insert(ty, function);
        function
    }
}

/// An immutable struct field holding `value`.
pub(super) fn immutable(value: ValType) -> FieldType {
    FieldType {
        storage: StorageType::Val(value),
        mutable: false,
    }
}
