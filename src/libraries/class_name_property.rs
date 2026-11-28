//! The exact `kotlin.reflect.KClass` name getters a target answers itself.
//!
//! A target that builds the object a class literal evaluates to answers these from its own class
//! tokens. The JVM stdlib implements the same declarations as ordinary interface methods. Archive
//! authority is an input fact, never inferred from a package or manifest spelling.

use super::{
    ClassNameKind, CompilerIntrinsic, FnKind, FunctionInfo, MemberRealization, PropKind,
    PropertyInfo,
};
use crate::types::{wk, Ty, TypeName};

pub(crate) fn realize_class_name_property(
    owner: TypeName,
    compiler_builtin_owner: bool,
    property: &mut PropertyInfo,
) {
    let kind = match property.name.as_str() {
        "simpleName" => ClassNameKind::Simple,
        "qualifiedName" => ClassNameKind::Qualified,
        _ => return,
    };
    // A KLIB member records no dispatch receiver and no signature. The JVM provider records the
    // owner as the dispatch receiver and copies this property's parameter-less signature onto the
    // getter. Both are the same abstract interface declaration.
    let dispatch_receiver = match property.receiver {
        None => true,
        Some(receiver) => receiver == Ty::obj_name(owner),
    };
    let signature = match property.getter.generic_sig.as_deref() {
        None => true,
        Some(signature) => {
            signature.formals.is_empty()
                && signature.receiver.is_none()
                && signature.params.is_empty()
                && signature.ret == property.ty
        }
    };
    if compiler_builtin_owner
        && owner == wk::kclass()
        && property.owner == owner
        && property.kind == PropKind::Member
        && dispatch_receiver
        && property.formals.is_empty()
        && property.associated_classifier.is_none()
        && property.context_count == 0
        && property.setter.is_none()
        && !property.is_const
        && property.ty == Ty::nullable(Ty::String)
        && property.getter.params.is_empty()
        && property.getter.context_count == 0
        && signature
        && property.getter.source_receiver.is_none()
        && property.getter.owner_is_interface
        && property.getter.is_abstract
        && property.getter.ret == property.ty
        && !property.getter.suspend
    {
        property.getter.compiler_intrinsic = Some(CompilerIntrinsic::ClassName(kind));
    }
}

/// Authenticate the platform stdlib's `KClass.isInstance(value: Any?): Boolean`, the one abstract
/// interface member of that exact shape on `kotlin.reflect.KClass`.
pub(crate) fn realize_class_is_instance(
    owner: TypeName,
    compiler_builtin_owner: bool,
    function: &mut FunctionInfo,
) {
    if compiler_builtin_owner
        && owner == wk::kclass()
        && function.kind == FnKind::Member
        && function.callable.name == "isInstance"
        && function
            .receiver
            .is_none_or(|receiver| receiver == Ty::obj_name(owner))
        && function.semantic_params().as_ref() == [Ty::nullable(Ty::obj_name(wk::any()))]
        && function.callable.ret == Ty::Boolean
        && function.context_count == 0
        && function.flags.is_abstract
        && !function.flags.suspend
    {
        function.callable.compiler_intrinsic = Some(CompilerIntrinsic::ClassIsInstance);
        function.callable.member_realization =
            MemberRealization::Intrinsic(CompilerIntrinsic::ClassIsInstance);
    }
}

#[cfg(test)]
mod tests;
