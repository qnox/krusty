//! kotlinc's nullability annotation for one declared Kotlin type.
//!
//! `AnnotationCodegen` classifies the declared type of a field, accessor, parameter or result:
//! a bare type parameter whose upper-bound chain admits null (`<T>`, `<T : Cargo?>`,
//! `<A : Cargo?, B : A>`) and a platform type carry no annotation, a marked `T?` is `@Nullable`,
//! and every other reference type is `@NotNull` — a non-null bound `<T : Cargo>` and a
//! definitely-non-null `T & Any` included. The occurrence answers: source `T & Any` tightens its
//! own bound, so the declaring class's bound for `T` is not the question.
//!
//! Every emitter that annotates a declared position asks here; whether the position is a JVM
//! reference at all is its physical descriptor's question, not this one.

use crate::types::Ty;

const NOT_NULL: &str = "Lorg/jetbrains/annotations/NotNull;";
const NULLABLE: &str = "Lorg/jetbrains/annotations/Nullable;";

/// The classification of a declared reference type: `0` = unannotated, `1` = `@NotNull`,
/// `2` = `@Nullable`.
pub(super) fn declared_reference_nullability(declared: Ty) -> u8 {
    match declared {
        Ty::PlatformNullable(_) => 0,
        Ty::Nullable(_) => 2,
        Ty::TyParam(..) if declared.upper_bound_admits_null() => 0,
        _ => 1,
    }
}

/// The annotation descriptor a [`declared_reference_nullability`] kind selects.
pub(super) fn nullability_annotation(kind: u8) -> Option<&'static str> {
    match kind {
        1 => Some(NOT_NULL),
        2 => Some(NULLABLE),
        _ => None,
    }
}

/// The annotation on a position declared `declared` whose physical JVM type is `physical`: none
/// unless `physical` is a reference.
pub(super) fn declared_annotation(declared: Ty, physical: Ty) -> Option<&'static str> {
    let descriptor = crate::jvm::names::type_descriptor(physical);
    if !(descriptor.starts_with('L') || descriptor.starts_with('[')) {
        return None;
    }
    nullability_annotation(declared_reference_nullability(declared))
}

/// Whether `declared` is a bare type parameter left unannotated because it admits null.
pub(super) fn is_unannotated_type_parameter(declared: Ty) -> bool {
    matches!(declared, Ty::TyParam(..)) && declared_reference_nullability(declared) == 0
}
