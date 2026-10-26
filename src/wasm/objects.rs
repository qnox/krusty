//! The root of the object model: what every object carries, and `kotlin.Any`'s three members.
//!
//! **An object** is a struct whose first field is its class's dispatch table. That field is typed
//! as `$AnyTable` in every class, so it is one field of `$Object`, the struct every class's type is
//! declared a subtype of: a dispatch reads it off any object before knowing the object's class.
//! `$AnyTable` holds `kotlin.Any`'s three slots — `equals`, `hashCode`, `toString`, in the order
//! every class table keeps them — then the class's interface table (`super::interface_dispatch`),
//! and every table type is declared a subtype of it. A class's own slots follow, so class slot `n`
//! from the fourth on is table field `n + 1` ([`slot_field`]).
//!
//! **References** of class, interface and `Any` types are carried as `(ref null eq)`, the common
//! supertype of an object and a `String`. A slot's function takes its receiver that way too, so an
//! override never needs a different signature from the member it overrides.
//!
//! Of `kotlin.Any`'s members only `equals` has a default body here, identity; `hashCode` and
//! `toString` have none until the library's own bodies are compiled into the module, and a class
//! that does not override them gets the trap every abstract slot gets.

use crate::fx_hash::HashMap;

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

/// The dispatch table field holding class slot `slot`.
pub(super) fn slot_field(slot: u32) -> u32 {
    if slot < ITABLE_FIELD {
        slot
    } else {
        slot + 1
    }
}

/// Type and function indices of the object model, fixed when the module is started, and the
/// interface dispatch every file records into.
pub(super) struct ObjectRoot {
    /// `$Itable`: `(array funcref)`, a class's interface table.
    pub(super) itable: u32,
    /// `$AnyTable`: `kotlin.Any`'s three slots, then the interface table.
    pub(super) any_table: u32,
    /// `$Object`: the dispatch table, and nothing else yet.
    pub(super) object: u32,
    /// The function types of `kotlin.Any`'s three slots, in slot order.
    pub(super) any_slot_types: [u32; 3],
    /// `kotlin.Any.equals`'s default: identity.
    pub(super) identity_equals: u32,
    /// A function that traps, per function type: what an abstract slot holds.
    traps: HashMap<u32, u32>,
    pub(super) interfaces: InterfaceDispatch,
}

impl ObjectRoot {
    /// Declare the root types, with `string` the runtime's string type.
    pub(super) fn declare(module: &mut Module, string: ValType) -> Self {
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
                    .chain([immutable(ValType::nullable(itable))])
                    .collect(),
            ),
            supertype: None,
            open: true,
        });
        let object = module.subtype(SubType {
            composite: CompositeType::Struct(vec![immutable(ValType::reference(any_table))]),
            supertype: None,
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
            object,
            any_slot_types,
            identity_equals,
            traps: HashMap::default(),
            interfaces: InterfaceDispatch::default(),
        }
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
