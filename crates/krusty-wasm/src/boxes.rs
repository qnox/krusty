//! Boxes: a scalar value carried where a reference is expected.
//!
//! A value this backend carries as a scalar (a primitive, or an unsigned number, a value class
//! carried as its underlying bits) is an object wherever it is read as `Any`, as a nullable
//! value, as a type parameter's value or through a supertype that is a class (`Number`,
//! `Comparable`), as it is on every Kotlin target. Each such classifier has one box type: a
//! struct of the dispatch table every object starts with, then the value itself. Its table holds
//! that type's own `equals` and `hashCode`, which is what kotlinc's output observes. A box type
//! and its table are declared the first time a file boxes a value of that classifier. A box also
//! implements the members of the library classes above its classifier that a call goes through
//! (`Number.toLong`, see [`members`]).
//!
//! The box type is keyed by the classifier's identity, never by the carrier: a `Long` box never
//! equals an `Int` box holding the same number, a `UInt` box never equals an `Int` box holding the
//! same bits, and unboxing casts to exactly the type the frontend checked the value is.

use krusty::fx_hash::HashMap;
use krusty::types::{Ty, TypeName};

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::interface_dispatch::ClassRef;
use super::objects::immutable;
use super::runtime::{Runtime, AND32, EQ32, EQ64, OR32};

mod members;

pub(super) use members::BoxMembers;

/// A scalar type's box struct and the global holding its dispatch table.
#[derive(Clone, Copy)]
pub(super) struct BoxForm {
    pub(super) structure: u32,
    pub(super) table: u32,
    /// The boxed type, and the class its box is in the program's dispatch.
    boxed: Ty,
    class: ClassRef,
    /// The globals holding the only two `Boolean` boxes, `false` then `true`: the JVM's
    /// `Boolean.valueOf` answers one instance per value, which `===` observes.
    pub(super) booleans: Option<[u32; 2]>,
}

/// The value field of a box struct, after the dispatch table.
const BOX_VALUE_FIELD: u32 = 1;

impl BoxForm {
    /// Whether boxing needs a scratch local of the value's carrier: every box but a `Boolean`'s,
    /// which is one of two existing instances.
    pub(super) fn needs_scratch(&self) -> bool {
        self.booleans.is_none()
    }

    /// Replace the value on the stack with its box, through `scratch` when
    /// [`Self::needs_scratch`].
    pub(super) fn emit_box(&self, code: &mut Code, scratch: Option<u32>) {
        if let Some([no, yes]) = self.booleans {
            code.if_(BlockType::Value(super::objects::REFERENCE))
                .global_get(yes)
                .else_()
                .global_get(no)
                .end();
            return;
        }
        let scratch = scratch.expect("a box built through a scratch local");
        code.local_set(scratch)
            .global_get(self.table)
            .local_get(scratch)
            .struct_new(self.structure);
    }

    /// Replace the reference on the stack, which the frontend checked is this box, with its value.
    pub(super) fn emit_unbox(&self, code: &mut Code) {
        code.ref_cast(false, HeapType::Concrete(self.structure))
            .struct_get(self.structure, BOX_VALUE_FIELD);
    }
}

/// The box types this module has declared.
#[derive(Default)]
pub(super) struct Boxes {
    /// By the boxed classifier's identity.
    forms: HashMap<TypeName, BoxForm>,
    /// The library class members called on a value that may be a box.
    members: BoxMembers,
    /// The global holding `kotlin.Unit`, once something reads it as a reference.
    unit: Option<u32>,
}

impl Runtime {
    /// The box of `ty`'s values, which are carried as `value`, declared on first use; `None` for a
    /// type that names no classifier or has no scalar hash, which is not boxed here. Declaring one
    /// declines when a library class member called on such values has an implementation for the
    /// classifier this generator cannot write.
    pub(super) fn box_form(
        &mut self,
        module: &mut Module,
        ty: Ty,
        value: ValType,
    ) -> Result<Option<BoxForm>, String> {
        let Some(identity) = ty.obj_internal() else {
            return Ok(None);
        };
        if let Some(form) = self.boxes.forms.get(&identity) {
            return Ok(Some(*form));
        }
        if !self.hashing.emit_scalar_hash(&mut Code::default(), ty) {
            return Ok(None);
        }
        let objects = &self.objects;
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(value),
            ]),
            supertype: Some(objects.object),
            open: false,
        });
        let [equals_ty, hash_code_ty, to_string_ty] = objects.any_slot_types;
        let equals = define(module, equals_ty, equals(value, structure));
        let hash_code = define(module, hash_code_ty, self.hash_code(ty, structure));
        // A box's `toString` is reached only by a call of `kotlin.Any`'s members on an `Any`
        // receiver, which this backend does not lower yet.
        let to_string = self.objects.trap(module, to_string_ty);
        let objects = &mut self.objects;
        let (class, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![structure]);
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        let booleans = (ty == Ty::Boolean).then(|| {
            [0, 1].map(|value| {
                let mut init = Code::default();
                init.global_get(table)
                    .i32_const(value)
                    .struct_new(structure);
                module.constant_global(ValType::reference(structure), init)
            })
        });
        let form = BoxForm {
            structure,
            table,
            boxed: ty,
            class,
            booleans,
        };
        self.boxes.forms.insert(identity, form);
        self.implement_box_members(module, form)?;
        Ok(Some(form))
    }
}

impl Runtime {
    /// The global holding `kotlin.Unit`: what a `Unit` expression is where a reference is expected
    /// (a lambda's result, a type parameter's value). `Unit` has no carrier of its own, so it is
    /// boxed like a primitive is, into the one instance it has.
    pub(super) fn unit_object(&mut self, module: &mut Module) -> u32 {
        if let Some(unit) = self.boxes.unit {
            return unit;
        }
        let objects = &mut self.objects;
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![immutable(ValType::reference(
                objects.any_table,
            ))]),
            supertype: Some(objects.object),
            open: false,
        });
        let [_, _, to_string_ty] = objects.any_slot_types;
        let hash_code = self.hashing.unit_hash_code;
        let to_string = objects.trap(module, to_string_ty);
        let (_, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![structure]);
        let mut init = Code::default();
        for function in [objects.identity_equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        let mut init = Code::default();
        init.global_get(table).struct_new(structure);
        let unit = module.constant_global(ValType::reference(structure), init);
        self.boxes.unit = Some(unit);
        unit
    }
}

fn define(module: &mut Module, ty: u32, (locals, code): (Vec<ValType>, Code)) -> u32 {
    let function = module.declare();
    module.define(function, Function { ty, locals, code });
    function
}

/// `equals(this, other)`: whether `other` is a box of the same classifier holding the same value,
/// a floating value compared by its bits with every NaN equal to every other (the JVM's
/// `Double.equals`).
fn equals(value: ValType, structure: u32) -> (Vec<ValType>, Code) {
    let heap = HeapType::Concrete(structure);
    let mut code = Code::default();
    code.local_get(1)
        .ref_test(false, heap)
        .if_(BlockType::Value(ValType::I32));
    for local in [0, 1] {
        code.local_get(local)
            .ref_cast(false, heap)
            .struct_get(structure, BOX_VALUE_FIELD)
            .local_set(local + 2);
    }
    match value {
        ValType::I32 => {
            code.local_get(2).local_get(3).op(EQ32);
        }
        ValType::I64 => {
            code.local_get(2).local_get(3).op(EQ64);
        }
        _ => {
            let (ne, bits, bits_eq) = floating(value);
            // Both NaN, or the same bits.
            code.local_get(2).local_get(2).op(ne);
            code.local_get(3).local_get(3).op(ne);
            code.op(AND32);
            code.local_get(2).op(bits).local_get(3).op(bits).op(bits_eq);
            code.op(OR32);
        }
    }
    code.else_().i32_const(0).end();
    (vec![value, value], code)
}

impl Runtime {
    /// `hashCode(this)`: the hash of the `ty` value the box holds.
    fn hash_code(&self, ty: Ty, structure: u32) -> (Vec<ValType>, Code) {
        let mut code = Code::default();
        code.local_get(0)
            .ref_cast(false, HeapType::Concrete(structure))
            .struct_get(structure, BOX_VALUE_FIELD);
        let hashed = self.hashing.emit_scalar_hash(&mut code, ty);
        debug_assert!(
            hashed,
            "a box is declared only for a type with a scalar hash"
        );
        (Vec::new(), code)
    }
}

/// The NaN test, the reinterpretation as bits and the bits' equality of a floating carrier.
fn floating(value: ValType) -> (u8, u8, u8) {
    match value {
        ValType::F32 => (0x5c, 0xbc, EQ32),
        _ => (0x62, 0xbd, EQ64),
    }
}
