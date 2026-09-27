//! How a value class named by `@Metadata` erases in a JVM descriptor, and the value class a
//! descriptor position recovers from it: the underlying for `X`, and for `X?` too when the
//! underlying's own null can stand for the absent value.

use super::{meta_descriptor_position, meta_ids};
use crate::types::{Ty, TypeName};

/// The JVM representation recovered for a metadata-named value class. Keep unsigned normalization in
/// this single semantic adapter so top-level, member, exact, and compatible alignment cannot drift.
///
/// A NULLABLE value class erases to its underlying too when that underlying's own null can stand for
/// the absent value (`Tag?` over `String` is a nullable `String`); otherwise it stays the boxed class
/// in the descriptor and there is nothing to recover.
pub(super) fn metadata_value_class_underlying(
    name: TypeName,
    nullable: bool,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    if nullable && !underlying_carries_null(name, value_underlying) {
        return None;
    }
    value_underlying(name).map(|underlying| {
        underlying
            .scalar_value_repr()
            .filter(|_| underlying.is_unsigned())
            .unwrap_or(underlying)
    })
}

/// Whether `name?` is its underlying on the JVM, as kotlinc erases it: the chain of underlyings
/// ends in a non-null reference, with no nullable link (whose null would be ambiguous) and no
/// primitive or unsigned scalar (which cannot hold null) on the way.
fn underlying_carries_null(
    name: TypeName,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut classifier = name;
    while seen.insert(classifier) {
        let Some(underlying) = value_underlying(classifier) else {
            return false;
        };
        if underlying.is_nullable() || !underlying.is_reference() || underlying.is_unsigned() {
            return false;
        }
        match underlying.obj_internal() {
            Some(inner) if value_underlying(inner).is_some() => classifier = inner,
            _ => return true,
        }
    }
    false
}

/// The VALUE CLASS each descriptor parameter position really has, per `@Metadata`.
///
/// A `@JvmInline value class` erases to its underlying in the descriptor, so the parsed parameter is
/// `J`/`Ljava/lang/String;` while the source type is `Duration`/`Tag`. Overload selection compares the
/// ARGUMENT's Kotlin type against the parameter, so without this a call passing the value class matches
/// nothing. Only a position whose metadata names a value class AND whose descriptor carries exactly
/// that value class's underlying is reported — anything else stays `None` and the erased type stands.
///
/// A NULLABLE value class whose descriptor carries the class itself (boxed) needs no recovery;
/// `metadata_value_class_underlying` already returns `None` for it. One erased to its underlying
/// (`Tag?` as `String`) is recovered as the nullable class.
pub(super) fn value_class_param_types(
    callable: &crate::jvm::metadata::MetaFn,
    desc_params: &[Ty],
    extension: bool,
    kept: usize,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Vec<Option<Ty>> {
    let mut out = vec![None; desc_params.len()];
    // Descriptor layout: [context params] [extension receiver] [value params].
    let context_count = callable.context_count();
    for (index, parameter) in callable.parameters().enumerate() {
        let position = meta_descriptor_position(index, context_count, extension);
        if position >= kept {
            break;
        }
        let (Some(name), Some(declared)) = (parameter.ty, desc_params.get(position)) else {
            continue;
        };
        let Some(underlying) =
            metadata_value_class_underlying(name, parameter.nullable(), value_underlying)
        else {
            continue;
        };
        if underlying.non_null() == declared.non_null() {
            // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never
            // as the boxed class. `UInt`/`ULong` ride in the JVM primitive slot of their carrier, so
            // a REFERENCE spelling here makes the lowerer box the argument (`kotlin/UInt.box-impl`)
            // into the erased descriptor slot (`I`/`J`) that takes it unboxed — a class file that
            // fails verification at load time. The scalar spelling is also the sharper one for the
            // overload selection this recovery exists for: it is exactly the type the checker gives
            // an unsigned argument. Every other value class (`Duration`, a user `Tag`) keeps the
            // class name: it is a reference on both sides, and the value-classes pass erases it at
            // the call.
            let recovered = meta_ids()
                .prim
                .get(&name)
                .copied()
                .unwrap_or_else(|| Ty::obj_name(name));
            out[position] = Some(if parameter.nullable() {
                Ty::nullable(recovered)
            } else {
                recovered
            });
        }
    }
    out
}

/// The VALUE CLASS a descriptor RETURN really has, per `@Metadata` — the return counterpart of
/// [`value_class_param_types`].
///
/// A value-class return erases exactly like a value-class parameter: the JVM method hands back the
/// UNDERLYING (`fun make(): K` → `make-<hash>()Ljava/lang/String;`) while `@Metadata` names `K`. The
/// call site needs BOTH halves. Knowing only the Kotlin return makes it treat the result as a BOXED
/// `K` and emit kotlinc's `checkcast K; K.unbox-impl()` over a `String` that is already the carrier —
/// a ClassCastException. Reporting the value class HERE is what marks the physical result as the
/// already-erased form, so the representation analysis leaves it alone.
///
/// Only a return whose metadata names a value class AND whose descriptor carries exactly that value
/// class's underlying is reported; anything else stays `None` and the erased type stands. A boxed
/// NULLABLE value class (the descriptor carries the class itself) gets `None` from
/// `metadata_value_class_underlying` and keeps its boxed handling; one erased to its underlying
/// (`Tag?` as `String`) is reported as the nullable class.
pub(super) fn value_class_return_type(
    callable: &crate::jvm::metadata::MetaFn,
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    let name = callable.ret_class?;
    let underlying =
        metadata_value_class_underlying(name, callable.ret_nullable(), value_underlying)?;
    if underlying.non_null() != desc_ret.non_null() {
        return None;
    }
    // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never as the
    // boxed class — the same rule the parameter side applies, and for the same reason: `UInt`/`ULong`
    // ride in the JVM primitive slot of their carrier, so a REFERENCE spelling would make the lowerer
    // box a value the descriptor takes unboxed.
    let recovered = meta_ids()
        .prim
        .get(&name)
        .copied()
        .unwrap_or_else(|| Ty::obj_name(name));
    Some(if callable.ret_nullable() {
        Ty::nullable(recovered)
    } else {
        recovered
    })
}
