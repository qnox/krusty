//! The JVM shape of a decoded `.kotlin_builtins` declaration: its semantic types, the erasure a
//! descriptor records, and the declared result a classpath member's metadata would publish.

use std::collections::HashMap;

use crate::jvm::names::type_descriptor;
use crate::libraries::GenericSig;
use crate::metadata::semantic::KotlinTypeParameterId;
use crate::types::Ty;

/// A builtin member's declared non-null result: its type parameter, or its classifier. A nullable
/// result publishes none, as the metadata of a classpath member does.
pub(super) fn builtin_declared_return(nullable: bool, ret: Ty) -> Option<Ty> {
    if nullable {
        None
    } else if ret.is_ty_param() {
        Some(ret)
    } else {
        ret.kotlin_class_internal().map(Ty::obj_name)
    }
}

/// The JVM erasure of a decoded builtin type: a type parameter erases to `Any` (`Object`), a class to
/// itself with its type arguments dropped — exactly what a JVM descriptor records.
pub(super) fn builtin_erased(ty: Ty) -> Ty {
    match ty {
        // JVM erasure follows the primary declared bound (`<T : CharSequence>` erases to
        // `CharSequence`), not unconditionally `Object`. Unbounded parameters already carry `Any?` as
        // their bound, so the same recursive rule covers both cases and stays aligned with
        // `names::type_descriptor` and bridge erasure.
        Ty::TyParam(_, bound) => builtin_erased(*bound),
        // Nullability is erased from reference classifiers, but not from scalar representation: a
        // nullable scalar occupies its wrapper reference in a JVM descriptor.
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
            inner.boxed_ref().unwrap_or_else(|| builtin_erased(*inner))
        }
        Ty::Obj(name, args) if !args.is_empty() => Ty::obj_name(name),
        other => other,
    }
}

/// The JVM descriptor a builtin member's declared signature erases to.
pub(super) fn builtin_descriptor(sig: &GenericSig) -> String {
    let params: String = sig
        .params
        .iter()
        .map(|p| type_descriptor(builtin_erased(*p)))
        .collect();
    format!("({params}){}", type_descriptor(builtin_erased(sig.ret)))
}

/// A decoded `.kotlin_builtins` type as a semantic [`Ty`]. `bounds` supplies each in-scope type
/// parameter's declared upper bound; an unlisted one is `Any?`, matching the `@Metadata`
/// generic-signature decoder. JVM erasure is derived separately by [`builtin_erased`].
pub(super) fn builtin_ty(
    t: &crate::jvm::metadata::BuiltinTy,
    bounds: &HashMap<KotlinTypeParameterId, Ty>,
) -> Ty {
    crate::metadata::semantic::semantic_ty(
        &crate::jvm::metadata::builtin_bridge::ty_to_common(t),
        bounds,
    )
}

/// The declared upper bound of each type parameter, keyed by its declaration identity. Bounds are
/// decoded with an EMPTY bound map so a recursive bound (`E : Comparable<E>`) terminates.
pub(super) fn builtin_bounds(
    params: &[crate::jvm::metadata::BuiltinTypeParam],
    inherited: &HashMap<KotlinTypeParameterId, Ty>,
) -> HashMap<KotlinTypeParameterId, Ty> {
    let mut out = inherited.clone();
    for p in params {
        let bound = p
            .bounds
            .first()
            .map(|b| builtin_ty(b, &HashMap::new()))
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        out.insert(p.id, bound);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_type_parameter_erasure_follows_its_primary_bound() {
        let bounded = Ty::ty_param("T", Ty::obj("kotlin/CharSequence"));
        assert_eq!(
            builtin_erased(bounded),
            Ty::obj("kotlin/CharSequence"),
            "a decoded builtins signature must use the same bound erasure as JVM descriptors"
        );
        let unbounded = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(builtin_erased(unbounded), Ty::obj("kotlin/Any"));
    }

    #[test]
    fn builtin_erasure_preserves_nullable_scalar_storage() {
        assert_eq!(
            builtin_erased(Ty::nullable(Ty::Int)),
            Ty::obj("java/lang/Integer")
        );
        assert_eq!(
            builtin_erased(Ty::nullable(Ty::UInt)),
            Ty::obj("kotlin/UInt")
        );
        assert_eq!(builtin_erased(Ty::nullable(Ty::String)), Ty::String);
    }
}
