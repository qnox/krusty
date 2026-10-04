//! Normalized semantic declarations read from Kotlin builtin metadata.
//!
//! Providers may obtain these declarations from different containers, but realization is allowed
//! to inspect only this common semantic record: the qualified owner identity, the complete declared
//! signature, and declaration modifiers. JVM descriptors, KLIB table ordinals, and provider-local
//! handles are deliberately absent.

use crate::libraries::{FnKind, PropKind};
use crate::types::{SemanticCallRole, Ty, TypeName};

/// One exact builtin member declaration at a provider boundary.
///
/// The textual callable name is one component of the declaration, never its identity by itself.
/// Consumers must match the complete record; a later KLIB provider can construct the same record
/// only after its linkdata declaration has been joined to IR by the KLIB's stable `IdSignature`.
pub(crate) struct BuiltinMemberDeclaration<'a> {
    pub owner: TypeName,
    pub name: &'a str,
    pub params: &'a [Ty],
    pub ret: Ty,
    pub is_property: bool,
    pub is_operator: bool,
    pub is_infix: bool,
    pub annotations: &'a [TypeName],
}

/// One exact package-level Kotlin function after a provider has normalized its declaration.
///
/// The package is the source namespace, never a JVM file facade. Receivers and parameters are
/// semantic Kotlin types, and the receiver is not repeated in `params`.
pub(crate) struct BuiltinFunctionDeclaration<'a> {
    pub package: TypeName,
    pub name: &'a str,
    pub kind: FnKind,
    pub receiver: Option<Ty>,
    pub params: &'a [Ty],
    pub ret: Ty,
    pub context_count: usize,
    pub type_parameter_count: usize,
    pub vararg: Option<usize>,
    pub is_suspend: bool,
    pub is_operator: bool,
    pub is_infix: bool,
}

/// One exact package-level Kotlin property after provider normalization.
pub(crate) struct BuiltinPropertyDeclaration<'a> {
    pub package: TypeName,
    pub name: &'a str,
    pub kind: PropKind,
    pub receiver: Option<Ty>,
    pub ty: Ty,
    pub context_count: usize,
    pub type_parameter_count: usize,
    pub mutable: bool,
}

/// Backend-relevant language role of one exact builtin declaration.
///
/// Providers call this only after decoding the declaration's qualified owner and complete source
/// signature. Consumers receive the role beside stable declaration identity and never recognize a
/// builtin again from a call-site spelling or platform descriptor.
pub(crate) fn semantic_call_role(
    declaration: BuiltinMemberDeclaration<'_>,
) -> Option<SemanticCallRole> {
    if declaration.is_property {
        return None;
    }
    if declaration.owner == crate::types::wk::any() {
        return match (declaration.name, declaration.params, declaration.ret) {
            ("equals", [parameter], Ty::Boolean)
                if *parameter == Ty::nullable(Ty::obj_name(crate::types::wk::any())) =>
            {
                Some(SemanticCallRole::KotlinAnyEquals)
            }
            ("hashCode", [], Ty::Int) => Some(SemanticCallRole::KotlinAnyHashCode),
            ("toString", [], Ty::String) => Some(SemanticCallRole::KotlinAnyToString),
            _ => None,
        };
    }
    (declaration.owner == crate::types::wk::comparable()
        && declaration.name == "compareTo"
        && declaration.params.len() == 1
        && declaration.ret == Ty::Int)
        .then_some(SemanticCallRole::KotlinComparableCompareTo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_roles_require_the_complete_builtin_declaration() {
        let any = crate::types::wk::any();
        let nullable_any = Ty::nullable(Ty::obj_name(any));
        let declaration = |owner, name, params: &[Ty], ret| BuiltinMemberDeclaration {
            owner,
            name,
            params,
            ret,
            is_property: false,
            is_operator: false,
            is_infix: false,
            annotations: &[],
        };
        assert_eq!(
            semantic_call_role(declaration(any, "equals", &[nullable_any], Ty::Boolean)),
            Some(SemanticCallRole::KotlinAnyEquals)
        );
        assert_eq!(
            semantic_call_role(declaration(any, "equals", &[Ty::String], Ty::Boolean)),
            None,
            "a familiar spelling with a different declaration shape has no role"
        );
        assert_eq!(
            semantic_call_role(declaration(
                crate::types::type_name("sample/Unrelated"),
                "equals",
                &[nullable_any],
                Ty::Boolean,
            )),
            None,
            "the signature without the exact builtin owner has no role"
        );
    }
}
