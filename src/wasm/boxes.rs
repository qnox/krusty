//! Boxes: a primitive value carried where a reference is expected.
//!
//! A Kotlin primitive read as `Any`, as a type parameter's value or through a supertype that is a
//! class (`Number`, `Comparable`) is an object, as it is on every Kotlin target. Each primitive
//! type has one box type: a struct of the dispatch table every object starts with, then the value
//! itself. Its table holds the primitive's own `equals`, as the JVM's box defines it, which is what
//! kotlinc's output observes. A box type and its table are declared the first time a
//! file boxes that primitive.
//!
//! The box type is the primitive's identity: a `Long` box never equals an `Int` box holding the
//! same number, and unboxing casts to exactly the type the frontend checked the value is.

use crate::fx_hash::HashMap;
use crate::types::{Ty, TyKind};

use super::encode::{BlockType, Code, CompositeType, Function, HeapType, Module, SubType, ValType};
use super::objects::immutable;
use super::runtime::{Runtime, AND32, EQ32, EQ64, OR32};

/// A primitive that has a box.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Primitive {
    Boolean,
    Byte,
    Short,
    Char,
    Int,
    Long,
    Float,
    Double,
}

impl Primitive {
    fn of(ty: Ty) -> Option<Self> {
        Some(match ty.kind() {
            TyKind::BOOLEAN => Self::Boolean,
            TyKind::BYTE => Self::Byte,
            TyKind::SHORT => Self::Short,
            TyKind::CHAR => Self::Char,
            TyKind::INT => Self::Int,
            TyKind::LONG => Self::Long,
            TyKind::FLOAT => Self::Float,
            TyKind::DOUBLE => Self::Double,
            _ => return None,
        })
    }

    fn value(self) -> ValType {
        match self {
            Self::Long => ValType::I64,
            Self::Float => ValType::F32,
            Self::Double => ValType::F64,
            _ => ValType::I32,
        }
    }
}

/// A primitive's box struct and the global holding its dispatch table.
#[derive(Clone, Copy)]
pub(super) struct BoxForm {
    pub(super) structure: u32,
    pub(super) table: u32,
    /// The globals holding the only two `Boolean` boxes, `false` then `true`: the JVM's
    /// `Boolean.valueOf` answers one instance per value, which `===` observes.
    pub(super) booleans: Option<[u32; 2]>,
}

/// The value field of a box struct, after the dispatch table.
pub(super) const BOX_VALUE_FIELD: u32 = 1;

/// The box types this module has declared.
#[derive(Default)]
pub(super) struct Boxes {
    forms: HashMap<Primitive, BoxForm>,
}

impl Runtime {
    /// The box of `ty`'s values, declared on first use; `None` for a type that is not boxed as a
    /// primitive (an unsigned number is a value class, whose box is its own).
    pub(super) fn box_form(&mut self, module: &mut Module, ty: Ty) -> Option<BoxForm> {
        let primitive = Primitive::of(ty)?;
        if let Some(form) = self.boxes.forms.get(&primitive) {
            return Some(*form);
        }
        let objects = &self.objects;
        let structure = module.subtype(SubType {
            composite: CompositeType::Struct(vec![
                immutable(ValType::reference(objects.any_table)),
                immutable(primitive.value()),
            ]),
            supertype: Some(objects.object),
            open: false,
        });
        let [equals_ty, hash_code_ty, to_string_ty] = objects.any_slot_types;
        let equals = define(module, equals_ty, equals(primitive, structure));
        // A box's `hashCode` and `toString` are reached only by a call of `kotlin.Any`'s members
        // on an `Any` receiver, which this backend does not lower yet.
        let hash_code = self.objects.trap(module, hash_code_ty);
        let to_string = self.objects.trap(module, to_string_ty);
        let objects = &mut self.objects;
        let (_, itable) = objects
            .interfaces
            .class(module, objects.itable, vec![structure]);
        let mut init = Code::default();
        for function in [equals, hash_code, to_string] {
            module.reference_function(function);
            init.ref_func(function);
        }
        init.global_get(itable).struct_new(objects.any_table);
        let table = module.constant_global(ValType::reference(objects.any_table), init);
        let booleans = (primitive == Primitive::Boolean).then(|| {
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
            booleans,
        };
        self.boxes.forms.insert(primitive, form);
        Some(form)
    }
}

fn define(module: &mut Module, ty: u32, (locals, code): (Vec<ValType>, Code)) -> u32 {
    let function = module.declare();
    module.define(function, Function { ty, locals, code });
    function
}

/// `equals(this, other)`: whether `other` is a box of the same primitive holding the same value,
/// a floating value compared by its bits with every NaN equal to every other (the JVM's
/// `Double.equals`).
fn equals(primitive: Primitive, structure: u32) -> (Vec<ValType>, Code) {
    let value = primitive.value();
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

/// The NaN test, the reinterpretation as bits and the bits' equality of a floating carrier.
fn floating(value: ValType) -> (u8, u8, u8) {
    match value {
        ValType::F32 => (0x5c, 0xbc, EQ32),
        _ => (0x62, 0xbd, EQ64),
    }
}
