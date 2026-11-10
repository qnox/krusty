//! The exact `kotlin.reflect.KClass` name getters a target answers from its own class tokens.
//!
//! A target that builds the object a class literal evaluates to (rather than calling a library
//! factory) is the only implementation of these getters it ever reaches, so the platform stdlib's
//! abstract declarations carry the operation. Archive authority is an input fact, never inferred
//! from a package or manifest spelling.

use crate::libraries::{ClassNameKind, CompilerIntrinsic, PropKind, PropertyInfo};
use crate::types::{wk, Ty, TypeName};

pub(in super::super) fn realize_class_name_property(
    owner: TypeName,
    compiler_builtin_owner: bool,
    property: &mut PropertyInfo,
) {
    let kind = match property.name.as_str() {
        "simpleName" => ClassNameKind::Simple,
        "qualifiedName" => ClassNameKind::Qualified,
        _ => return,
    };
    if compiler_builtin_owner
        && owner == wk::kclass()
        && property.owner == owner
        && property.kind == PropKind::Member
        && property.receiver.is_none()
        && property.formals.is_empty()
        && property.associated_classifier.is_none()
        && property.context_count == 0
        && property.setter.is_none()
        && !property.is_const
        && property.ty == Ty::nullable(Ty::String)
        && property.getter.params.is_empty()
        && property.getter.context_count == 0
        && property.getter.generic_sig.is_none()
        && property.getter.source_receiver.is_none()
        && property.getter.owner_is_interface
        && property.getter.is_abstract
        && property.getter.ret == property.ty
        && !property.getter.suspend
    {
        property.getter.compiler_intrinsic = Some(CompilerIntrinsic::ClassName(kind));
    }
}

#[cfg(test)]
mod tests;
