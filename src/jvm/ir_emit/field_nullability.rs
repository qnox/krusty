//! kotlinc's nullability classification of a field, accessor or primary-constructor parameter,
//! which its annotations, parameter guards and constructor line table must agree on.

use crate::jvm::declared_type_nullability::declared_reference_nullability;
pub(super) use crate::jvm::declared_type_nullability::nullability_annotation;
use crate::types::Ty;

/// kotlinc's nullability classification for a declared position typed `t`: `0` = no annotation (a
/// primitive, a platform type, or a type-parameter occurrence that admits null), `1` = a non-null
/// reference (`@NotNull`, plus an `Intrinsics.checkNotNullParameter` guard wherever one applies),
/// `2` = a nullable reference (`@Nullable`, never guarded).
///
/// `t` is the DECLARED type of the position; its descriptor decides only whether the position is a
/// reference at all. The occurrence answers (see `declared_type_nullability`): `val v: T & Any` is
/// `@NotNull` and guarded even when the class declares `<T>`, and an unbounded `<T>` or a
/// `<T : Cargo?>` is left UNANNOTATED, not `@Nullable`.
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

/// Whether a field/constructor parameter is a NON-NULL reference — [`field_nullability_kind`] `== 1`.
pub(super) fn is_nonnull_reference_field(t: Ty) -> bool {
    field_nullability_kind(t) == 1
}
