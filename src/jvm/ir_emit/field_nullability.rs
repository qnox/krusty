//! kotlinc's nullability classification of a class field or primary-constructor parameter, which
//! its annotations, parameter guards and constructor line table must agree on.

use crate::ir::IrFile;
use crate::types::Ty;

/// The TYPE PARAMETER a field is declared as (`class Pair<A, B>(val a: A)` → `a` is `A`), or `None` when
/// the field has a type of its own. `field_signatures` already tracks these — it is what drives their
/// `Signature` attribute. Callers ask so they can consult that parameter's BOUND: the erased descriptor
/// says nothing about whether the field can hold null.
fn type_parameter_field_name<'a>(ir: &'a IrFile, fq_name: &str, field: &str) -> Option<&'a str> {
    ir.field_signatures(fq_name)
        .and_then(|signatures| {
            signatures
                .iter()
                .find(|(name, _)| name == field)
                .map(|(_, parameter)| parameter.as_str())
        })
        .or_else(|| {
            let class = ir.class_id_by_name(crate::types::type_name(fq_name))?;
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
/// A field declared as a BARE type parameter answers from that parameter's BOUND, not from the
/// erased descriptor: `<T : Cargo>`/`<T : Any>` cannot hold null and is `@NotNull`, while an
/// unbounded `<T>` (= `Any?`) or a `<T : Cargo?>` is left UNANNOTATED — kotlinc does not mark it
/// `@Nullable`. A nullable OCCURRENCE (`var c: T?`) is `@Nullable` even when the bound is not:
/// the `?` is what admits null, and treating the bound as the field's nullability emits a setter
/// line entry at the width of a null check the setter does not have (`pc == code_length`).
///
/// One predicate for the pool seeder, the field/accessor/parameter annotations, the `var` setter guard,
/// and the constructor's `LineNumberTable` start pc, because those must agree: classify a field as
/// guarded in one and unguarded in another and the line entry lands at the wrong offset.
pub(super) fn field_nullability_kind(ir: &IrFile, fq_name: &str, name: &str, t: Ty) -> u8 {
    let d = crate::jvm::names::type_descriptor(t);
    if !(d.starts_with('L') || d.starts_with('[')) {
        return 0;
    }
    if matches!(t, Ty::PlatformNullable(_)) {
        0
    } else if t.is_nullable() {
        2
    } else if let Some(parameter) = type_parameter_field_name(ir, fq_name, name) {
        u8::from(!ir.class_type_param_admits_null(fq_name, parameter))
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
pub(super) fn is_nonnull_reference_field(ir: &IrFile, fq_name: &str, name: &str, t: Ty) -> bool {
    field_nullability_kind(ir, fq_name, name, t) == 1
}
