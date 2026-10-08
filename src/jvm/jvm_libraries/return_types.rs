//! Provider-boundary normalization of declared return types.

use super::*;

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
            .filter(|ret| matches!(ret, Ty::TyParam(..)))
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
    match ty {
        Ty::Obj(owner, arguments) => {
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
        Ty::Nullable(inner) => Ty::nullable(java_collection_return_lower_bound(*inner)),
        Ty::PlatformNullable(inner) => {
            Ty::platform_nullable(java_collection_return_lower_bound(*inner))
        }
        Ty::InProjection(inner) => Ty::in_projection(java_collection_return_lower_bound(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(java_collection_return_lower_bound(*inner)),
        Ty::StarProjection(inner) => {
            Ty::star_projection(java_collection_return_lower_bound(*inner))
        }
        Ty::TyParam(name, bound) => Ty::ty_param(name, java_collection_return_lower_bound(*bound)),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature.params.clone(),
            java_collection_return_lower_bound(signature.ret),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        _ => ty,
    }
}
