//! Which value classes the JVM carries through its value-class box and carrier machinery.
//!
//! The IR's value-class table is semantic: it holds every checked value-class declaration,
//! including the ones the type model carries as a native scalar (the unsigned integers). The JVM
//! gives a native scalar a primitive slot and boxes it through its wrapper like any other scalar,
//! so the questions "box this through `box-impl`" and "unbox to the declared carrier" exclude it.
//! Callable naming does not ask these questions: kotlinc mangles by the semantic identity.

use crate::ir::IrFile;
use crate::types::{Ty, TypeName};

/// Whether the type model carries `classifier` as a native scalar rather than a class reference.
pub(super) fn has_native_carrier(classifier: TypeName) -> bool {
    Ty::obj_name(classifier).is_jvm_scalar()
}

/// Whether the JVM represents `classifier` as a value class with its own box and carrier.
pub(crate) fn is_boxed_value_class(ir: &IrFile, classifier: TypeName) -> bool {
    ir.is_value_class_name(classifier) && !has_native_carrier(classifier)
}

/// The declared underlying type of a value class the JVM boxes through its own `box-impl`.
pub(crate) fn boxed_value_class_underlying(ir: &IrFile, classifier: TypeName) -> Option<Ty> {
    if has_native_carrier(classifier) {
        return None;
    }
    ir.value_class_underlying_name(classifier)
}

/// Internal name → declared underlying of each value class this file declares, before recursive
/// value-class erasure. A generic underlying property carries its type parameter's declared upper
/// bound, read from the checked occurrence itself: `S<T : String>` carries `String`, while
/// `V<T : Int>` carries `int`. Keeping the unbound `TyParam` here would force an Object slot while
/// descriptor code specializes the same bound, leaving boxing and null guards inconsistent with the
/// emitted method descriptor. A nullable occurrence (`val x: T?`) keeps its `?` on that bound, as
/// kotlinc's `getUnderlyingType` keeps a nullable type parameter: `X<T : Any>(val x: T?)` carries
/// `Any?`, so `X?` is boxed.
pub(super) fn declared_underlyings(ir: &IrFile) -> crate::value_classes::UnderlyingTypes {
    ir.classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            let field = c.fields.first()?;
            Some((
                c.fq_name,
                parameter_bound_underlying(field.ty).canonical_semantic(),
            ))
        })
        .collect()
}

/// `declared` with each type-parameter occurrence replaced by its upper bound, through a chain of
/// parameters bounded by parameters, keeping any `?` met on the way.
fn parameter_bound_underlying(declared: Ty) -> Ty {
    let mut nullable = declared.is_nullable();
    let mut current = declared.non_null();
    let mut seen = std::collections::HashSet::new();
    while let Ty::TyParam(name, bound) = current {
        assert!(
            seen.insert(name),
            "a checked type-parameter bound chain is acyclic"
        );
        nullable |= bound.is_nullable();
        current = bound.non_null();
    }
    if nullable {
        Ty::nullable(current)
    } else {
        current
    }
}

/// The carrier of a value class the JVM boxes through its own `box-impl`: its underlying erased as
/// the JVM erases it, through nested value classes but stopping at a nullable one the JVM keeps
/// boxed (`NZ1(val nz: Z?)` over `Z(val x: Int)` carries `Z`, not `int`).
pub(crate) fn boxed_value_class_carrier(ir: &IrFile, classifier: TypeName) -> Option<Ty> {
    if has_native_carrier(classifier) {
        return None;
    }
    let mut declarations = declared_underlyings(ir);
    for name in ir.value_class_names() {
        if let (std::collections::hash_map::Entry::Vacant(entry), Some(declared)) = (
            declarations.entry(name),
            ir.value_class_underlying_name(name),
        ) {
            entry.insert(declared);
        }
    }
    let underlying = *declarations.get(&classifier)?;
    Some(super::erase(&underlying, &declarations))
}

/// Every value class this IR knows that the JVM boxes through its own `box-impl`.
pub(crate) fn boxed_value_class_names(ir: &IrFile) -> impl Iterator<Item = TypeName> + '_ {
    ir.value_class_names()
        .filter(|&classifier| !has_native_carrier(classifier))
}

/// A value class whose own name is a primitive-array classifier and whose declared carrier is an
/// array. The semantic name and the carrier are different JVM classes that share one descriptor
/// (`kotlin/UIntArray` and `int[]` are both `[I`). The rewrite map is the authority: a signed
/// primitive array is not in it, and a user value class over an array has a name that is not itself
/// a primitive-array classifier.
pub(super) fn primitive_array_value_class(
    classifier: TypeName,
    under: &crate::value_classes::UnderlyingTypes,
) -> Option<(TypeName, Ty)> {
    crate::types::prim_array_element(classifier)?;
    let carrier = under.get(&classifier).copied()?.non_null();
    carrier.is_array().then_some((classifier, carrier))
}

/// The JVM slot published for `ty` when erasure keeps the semantic name so element reads stay
/// unsigned. Publishing that name is the box (`is_boxed_vc` treats a physical stamp of the value
/// class as the box). `None` leaves the caller's existing publication in place.
pub(super) fn carrier_slot(ty: Ty, under: &crate::value_classes::UnderlyingTypes) -> Option<Ty> {
    let name = ty.non_null().obj_internal()?;
    let (_, carrier) = primitive_array_value_class(name, under)?;
    Some(if ty.is_nullable() {
        Ty::nullable(carrier)
    } else {
        carrier
    })
}

/// Erase `t` through the value-class table. A primitive-array value class keeps its semantic name
/// so element reads stay unsigned; its JVM descriptor is already the carrier. Physical slots use
/// [`carrier_slot`] instead of this name.
pub(super) fn erase(t: &Ty, under: &crate::value_classes::UnderlyingTypes) -> Ty {
    if let Some(name) = t.non_null().obj_internal() {
        if primitive_array_value_class(name, under).is_some() {
            return *t;
        }
    }
    crate::value_classes::project_underlying(
        super::member_names::value_class_bound_occurrence(*t, under),
        under,
        &super::JvmUnderlyingProjection,
    )
}

/// The classfile name a recorded type operation asks `instanceof` or `checkcast` to use.
pub(crate) fn type_operation_internal_name(
    operation: crate::ir::IrValueClassTypeOperation,
) -> String {
    match operation.role {
        crate::ir::IrValueClassTypeRole::Box => {
            crate::jvm::names::classfile_internal_name_of(operation.boxed_owner).to_string()
        }
        crate::ir::IrValueClassTypeRole::Carrier => {
            crate::jvm::names::type_descriptor(operation.carrier)
        }
    }
}

/// The JVM type an instance of `classifier` is held as in its own right: a value class the JVM
/// boxes is held as its carrier (an inner class's outer instance of `Z(val x: Int)` is an `int`),
/// any other class as a reference to it.
pub(crate) fn instance_representation(ir: &IrFile, classifier: TypeName) -> Ty {
    boxed_value_class_carrier(ir, classifier).unwrap_or_else(|| Ty::obj_name(classifier))
}

/// Whether the erased type occupies a JVM *reference* slot. A non-null Kotlin primitive class
/// (`kotlin/Int`, `kotlin/Boolean`, …) emits as a JVM primitive (`I`, `Z`, …), so it is NOT a
/// reference; its NULLABLE form is the boxed wrapper (`Integer`), which is. Everything else that is a
/// `Class` is a reference.
pub(super) fn is_ref(t: &Ty) -> bool {
    if t.is_nullable() {
        return true;
    }
    // A Kotlin type parameter always occupies an erased JVM reference slot, even when its upper
    // bound names a primitive-like Kotlin class. Treating `T` as non-reference loses the boxing
    // boundary in `Holder<T>(value: T)` and stores an unboxed value-class carrier as `Integer`
    // instead of the value class's boxed wrapper.
    if matches!(t.non_null(), Ty::TyParam(..)) {
        return true;
    }
    // A JVM scalar (`Int`/`Long`/… AND the unsigned `UInt`/`ULong`, which are unboxed primitives) is NOT a
    // reference. Check this FIRST — `kotlin_class_internal(UInt)` is "kotlin/UInt" but `unboxed_primitive`
    // only knows the signed wrappers, so the descriptor check below would misclassify it as a reference.
    if t.is_jvm_scalar() {
        return false;
    }
    // A FUNCTION type realizes as a `FunctionN` object and an array as its array class — both are
    // references with no `kotlin_class_internal`, and the `None => false` fallback below silently
    // stripped their `checkNotNullParameter` guards (kotlinc guards a `block: () -> Unit` like any
    // other non-null reference parameter).
    if matches!(t, Ty::Fun(_)) || t.is_array() {
        return true;
    }
    // `kotlin_class_internal` (not `obj_internal`): a bare `Ty::String` variant is a REFERENCE but has no
    // `obj_internal()` — treating it as a non-reference makes `nullable_is_boxed` think a `String`-backed
    // value class is primitive-like (`Str?` wrongly boxed instead of unboxed to `String?`).
    match t.kotlin_class_internal() {
        Some(fq_name) => Ty::obj_name(fq_name).unboxed_primitive().is_none(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_scalar_value_class_is_semantic_but_not_boxed() {
        let uint = crate::types::type_name("kotlin/UInt");
        let count = crate::types::type_name("fixture/Count");
        let mut ir = IrFile::default();
        ir.insert_external_value_class_name(uint, Ty::Int);
        ir.insert_external_value_class_name(count, Ty::Int);

        assert_eq!(ir.value_class_underlying_name(uint), Some(Ty::Int));
        assert!(!is_boxed_value_class(&ir, uint));
        assert_eq!(boxed_value_class_underlying(&ir, uint), None);
        assert!(is_boxed_value_class(&ir, count));
        assert_eq!(boxed_value_class_underlying(&ir, count), Some(Ty::Int));
        assert_eq!(
            boxed_value_class_names(&ir).collect::<Vec<_>>(),
            vec![count]
        );
    }

    #[test]
    fn an_unsigned_array_publishes_its_carrier_and_keeps_two_classfile_names() {
        let array = crate::types::type_name("kotlin/UIntArray");
        let carrier = Ty::obj("kotlin/IntArray");
        let mut under = crate::value_classes::UnderlyingTypes::new();
        under.insert(array, carrier);
        assert_eq!(carrier_slot(Ty::obj_name(array), &under), Some(carrier));
        assert_eq!(
            carrier_slot(Ty::nullable(Ty::obj_name(array)), &under),
            Some(Ty::nullable(carrier))
        );
        assert_eq!(
            crate::jvm::names::type_descriptor(Ty::obj_name(array)),
            crate::jvm::names::type_descriptor(carrier)
        );
        let shared = (array, carrier);
        let operation = |role| crate::ir::IrValueClassTypeOperation {
            boxed_owner: shared.0,
            carrier: shared.1,
            role,
        };
        assert_eq!(
            type_operation_internal_name(operation(crate::ir::IrValueClassTypeRole::Box)),
            "kotlin/UIntArray"
        );
        assert_eq!(
            type_operation_internal_name(operation(crate::ir::IrValueClassTypeRole::Carrier)),
            "[I"
        );
        assert!(
            primitive_array_value_class(crate::types::type_name("kotlin/IntArray"), &under)
                .is_none()
        );
    }
}
