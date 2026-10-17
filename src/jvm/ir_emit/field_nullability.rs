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

/// The nullability annotation on field `index` of `class`. A field typed by a value class is
/// annotated by the property's declared Kotlin type, not by its erased storage: `val b: N` over
/// `value class N(val s: String?)` stores a `String?` yet is `@NotNull`, like kotlinc's. An explicit
/// backing field (`val x: Any? field: String`) declares its own type, which its field keeps. The
/// field's descriptor still decides whether the position is a reference at all, and the guard and
/// line-table classification stays [`field_nullability_kind`].
pub(super) fn declared_field_annotation(
    class: &crate::ir::IrClass,
    index: usize,
) -> Option<&'static str> {
    let field = &class.fields[index];
    let declared = backing_property(class, index)
        .filter(|property| property.storage_ty.is_none())
        .map_or(field.ty, |property| property.ty);
    declared_storage_annotation(declared, field.ty)
}

/// The nullability annotation on the accessors and data-class members field `index` of `class`
/// backs: the property's declared type answers, whatever the field stores.
pub(super) fn declared_property_annotation(
    class: &crate::ir::IrClass,
    index: usize,
) -> Option<&'static str> {
    let field = &class.fields[index];
    let declared = backing_property(class, index).map_or(field.ty, |property| property.ty);
    declared_storage_annotation(declared, field.ty)
}

/// The nullability annotation on a static's field, typed by the declaration a value-class erasure
/// replaced, as [`declared_field_annotation`] is for an instance field.
pub(super) fn declared_static_annotation(stat: &crate::ir::IrStatic) -> Option<&'static str> {
    declared_storage_annotation(stat.erased_declared_ty.unwrap_or(stat.ty), stat.ty)
}

fn backing_property(class: &crate::ir::IrClass, index: usize) -> Option<&crate::ir::IrProperty> {
    class
        .properties
        .iter()
        .find(|property| property.backing_field == Some(index as u32))
}

fn declared_storage_annotation(declared: Ty, stored: Ty) -> Option<&'static str> {
    nullability_annotation(field_nullability_kind(stored))
        .and_then(|_| crate::jvm::declared_type_nullability::declared_annotation(declared, stored))
}
