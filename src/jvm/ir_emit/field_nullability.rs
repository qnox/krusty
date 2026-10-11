//! kotlinc's nullability classification of a declared Kotlin type, which a field's, accessor's or
//! primary-constructor parameter's annotations, parameter guards and constructor line table must
//! agree on.

use crate::types::Ty;

/// kotlinc's nullability classification for a declared reference position: `0` = no annotation (a
/// primitive, a platform type, or a type-parameter occurrence that admits null), `1` = a non-null
/// reference (`@NotNull`, plus an `Intrinsics.checkNotNullParameter` guard wherever one applies),
/// `2` = a nullable reference (`@Nullable`, never guarded).
///
/// `t` is the DECLARED (semantic) type of the position; its erased descriptor decides only whether
/// the position is a reference at all. This is kotlinc's `AnnotationCodegen` rule
/// (`isBareTypeParameterWithNullableUpperBound`, then `TypeUtils.isNullableType`) read off the
/// occurrence itself:
/// - a marked `T?` is `@Nullable`, whatever `T`'s bound;
/// - a bare type parameter whose upper-bound chain admits null (`<T>`, `<T : Cargo?>`,
///   `<A : Cargo?, B : A>`) is left UNANNOTATED, not `@Nullable`;
/// - a non-null-bounded parameter (`<T : Cargo>`) and a definitely-non-null `T & Any` are
///   `@NotNull`. Source `T & Any` tightens the occurrence's own bound, so the class's declared bound
///   for `T` is not the answer.
///
/// One predicate for the pool seeder, the field/accessor/parameter annotations, the `var` setter
/// guard, and the constructor's `LineNumberTable` start pc, because those must agree: classify a
/// field as guarded in one and unguarded in another and the line entry lands at the wrong offset.
pub(super) fn field_nullability_kind(t: Ty) -> u8 {
    let d = crate::jvm::names::type_descriptor(t);
    if !(d.starts_with('L') || d.starts_with('[')) {
        return 0;
    }
    declared_reference_nullability(t)
}

/// [`field_nullability_kind`] for a position already known to be a JVM reference.
pub(super) fn declared_reference_nullability(t: Ty) -> u8 {
    match t {
        Ty::PlatformNullable(_) => 0,
        Ty::Nullable(_) => 2,
        Ty::TyParam(..) if t.upper_bound_admits_null() => 0,
        _ => 1,
    }
}

/// The annotation descriptor a [`field_nullability_kind`] selects: `@NotNull`, `@Nullable` or none.
pub(super) fn nullability_annotation(kind: u8) -> Option<&'static str> {
    match kind {
        1 => Some("Lorg/jetbrains/annotations/NotNull;"),
        2 => Some("Lorg/jetbrains/annotations/Nullable;"),
        _ => None,
    }
}

/// Whether a field/constructor parameter is a NON-NULL reference — [`field_nullability_kind`] `== 1`.
pub(super) fn is_nonnull_reference_field(t: Ty) -> bool {
    field_nullability_kind(t) == 1
}
