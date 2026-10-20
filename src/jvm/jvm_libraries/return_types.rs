//! Provider-boundary normalization of declared return types.

use super::*;
use crate::types::TyKind;

/// The declaration-level return fact shared by both classpath member construction loops.
pub(super) fn metadata_declared_nonnull_return(
    function: &super::super::metadata::MetaFn,
) -> Option<Ty> {
    if function.ret_nullable() {
        return None;
    }
    function.ret_class.map(Ty::obj_name).or_else(|| {
        function
            .generic_sig
            .as_ref()
            .map(|signature| signature.ret)
            .filter(|ret| ret.is_ty_param())
    })
}

/// The mutable lower bound of a Java collection's flexible mutability interval.
///
/// A Java `List<T>` or `Iterator<T>`, in a parameter or a result, is Kotlin's `(Mutable)List<T>!`
/// / `(Mutable)Iterator<T>!`. Publishing the mutable face supplies that lower bound; its ordinary
/// Kotlin supertypes include the read-only face, and a platform type's upper bound is that
/// read-only face. A Kotlin-declared `MutableList` parameter is not a Java type and never passes
/// through here, so a read-only Kotlin `List` still does not satisfy it.
pub(super) fn java_collection_return_lower_bound(ty: Ty) -> Ty {
    match ty.kind() {
        TyKind::Obj(owner, arguments) => {
            let arguments = arguments
                .iter()
                .map(|argument| java_collection_return_lower_bound(*argument))
                .collect::<Vec<_>>();
            let physical = crate::jvm::jvm_class_map::to_jvm_type_name(owner);
            let owner =
                crate::jvm::jvm_class_map::jvm_collection_to_kotlin_mutable_type_name(physical)
                    .unwrap_or(owner);
            Ty::obj_args_name(owner, &arguments)
        }
        TyKind::Nullable(inner) => Ty::nullable(java_collection_return_lower_bound(inner)),
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(java_collection_return_lower_bound(inner))
        }
        TyKind::InProjection(inner) => Ty::in_projection(java_collection_return_lower_bound(inner)),
        TyKind::OutProjection(inner) => {
            Ty::out_projection(java_collection_return_lower_bound(inner))
        }
        TyKind::StarProjection(inner) => {
            Ty::star_projection(java_collection_return_lower_bound(inner))
        }
        TyKind::TyParam(name, bound) => {
            Ty::ty_param(name.as_str(), java_collection_return_lower_bound(bound))
        }
        TyKind::Fun(signature) => Ty::fun_with_shape(
            signature.params.clone(),
            java_collection_return_lower_bound(signature.ret),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        _ => ty,
    }
}
