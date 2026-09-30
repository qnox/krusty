//! kotlinc's nullability classification of a class field or primary-constructor parameter, which
//! its annotations, parameter guards and constructor line table must agree on.

use crate::ir::IrFile;
use crate::types::{Ty, TypeName};

/// The TYPE PARAMETER a field is declared as (`class Pair<A, B>(val a: A)` → `a` is `A`), or `None` when
/// the field has a type of its own. `field_signatures` already tracks these — it is what drives their
/// `Signature` attribute. Callers ask so they can consult that parameter's BOUND: the erased descriptor
/// says nothing about whether the field can hold null.
fn type_parameter_field_name<'a>(ir: &'a IrFile, owner: TypeName, field: &str) -> Option<&'a str> {
    ir.field_signatures(owner)
        .and_then(|signatures| {
            signatures
                .iter()
                .find(|(name, _)| name == field)
                .map(|(_, parameter)| parameter.as_str())
        })
        .or_else(|| {
            let class = ir.class_id_by_name(owner)?;
            ir.classes[class as usize]
                .fields
                .iter()
                .find(|candidate| candidate.name == field)
                .and_then(|candidate| candidate.type_param.as_deref())
        })
}

/// kotlinc's nullability classification for a class field / primary-constructor parameter: `0` = no
/// annotation (a primitive, or a type parameter that admits null), `1` = a non-null reference
/// (`@NotNull`, plus an `Intrinsics.checkNotNullParameter` guard wherever one applies), `2` = a nullable
/// reference (`@Nullable`, never guarded).
///
/// A field declared as a TYPE PARAMETER answers from that parameter's BOUND, not from the erased
/// descriptor: `<T : Cargo>`/`<T : Any>` cannot hold null and is `@NotNull`, while an unbounded `<T>`
/// (= `Any?`) or a `<T : Cargo?>` is left UNANNOTATED — kotlinc does not mark it `@Nullable`.
///
/// One predicate for the pool seeder, the field/accessor/parameter annotations, the `var` setter guard,
/// and the constructor's `LineNumberTable` start pc, because those must agree: classify a field as
/// guarded in one and unguarded in another and the line entry lands at the wrong offset.
pub(super) fn field_nullability_kind(ir: &IrFile, owner: TypeName, name: &str, t: Ty) -> u8 {
    let d = crate::jvm::names::type_descriptor(t);
    if !(d.starts_with('L') || d.starts_with('[')) {
        return 0;
    }
    if matches!(t, Ty::PlatformNullable(_)) {
        0
    } else if let Some(parameter) = type_parameter_field_name(ir, owner, name) {
        u8::from(!ir.class_type_param_admits_null(owner, parameter))
    } else if matches!(t, Ty::Nullable(_)) {
        2
    } else {
        1
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
pub(super) fn is_nonnull_reference_field(ir: &IrFile, owner: TypeName, name: &str, t: Ty) -> bool {
    field_nullability_kind(ir, owner, name, t) == 1
}
