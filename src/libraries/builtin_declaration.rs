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
        if declaration.owner == crate::types::type_name("kotlin/reflect/KCallable")
            && declaration.name == "name"
            && declaration.params.is_empty()
            && declaration.ret == Ty::String
            && !declaration.is_operator
            && !declaration.is_infix
        {
            return Some(SemanticCallRole::KotlinCallableReferenceName);
        }
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
    if declaration.owner == crate::types::wk::comparable()
        && declaration.name == "compareTo"
        && matches!(declaration.params, [Ty::TyParam(..)])
        && declaration.ret == Ty::Int
        && declaration.is_operator
        && !declaration.is_infix
    {
        return Some(SemanticCallRole::KotlinComparableCompareTo);
    }

    let property_arity = [
        ("kotlin/reflect/KProperty0", 0),
        ("kotlin/reflect/KProperty1", 1),
        ("kotlin/reflect/KProperty2", 2),
    ]
    .into_iter()
    .find_map(|(owner, arity)| {
        (declaration.owner == crate::types::type_name(owner)).then_some(arity)
    });
    if let Some(arity) = property_arity {
        let signature_matches = declaration.params.len() == arity as usize
            && declaration
                .params
                .iter()
                .all(|parameter| matches!(parameter, Ty::TyParam(..)))
            && matches!(declaration.ret, Ty::TyParam(..))
            && !declaration.is_infix;
        if signature_matches
            && ((declaration.name == "get" && !declaration.is_operator)
                || (declaration.name == "invoke" && declaration.is_operator))
        {
            return Some(SemanticCallRole::KotlinPropertyReferenceGet(arity));
        }
    }

    let mutable_property_arity = [
        ("kotlin/reflect/KMutableProperty0", 0),
        ("kotlin/reflect/KMutableProperty1", 1),
        ("kotlin/reflect/KMutableProperty2", 2),
    ]
    .into_iter()
    .find_map(|(owner, arity)| {
        (declaration.owner == crate::types::type_name(owner)).then_some(arity)
    });
    if let Some(arity) = mutable_property_arity {
        let signature_matches = declaration.params.len() == arity as usize + 1
            && declaration
                .params
                .iter()
                .all(|parameter| matches!(parameter, Ty::TyParam(..)))
            && declaration.ret == Ty::Unit
            && !declaration.is_operator
            && !declaration.is_infix;
        if signature_matches && declaration.name == "set" {
            return Some(SemanticCallRole::KotlinPropertyReferenceSet(arity));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_roles_require_the_complete_builtin_declaration() {
        fn declaration<'a>(
            owner: TypeName,
            name: &'a str,
            params: &'a [Ty],
            ret: Ty,
        ) -> BuiltinMemberDeclaration<'a> {
            BuiltinMemberDeclaration {
                owner,
                name,
                params,
                ret,
                is_property: false,
                is_operator: false,
                is_infix: false,
                annotations: &[],
            }
        }

        let any = crate::types::wk::any();
        let nullable_any = Ty::nullable(Ty::obj_name(any));
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

        let parameter = Ty::ty_param("T", Ty::obj_name(any));
        let parameters = [parameter];
        let mut comparable = declaration(
            crate::types::wk::comparable(),
            "compareTo",
            &parameters,
            Ty::Int,
        );
        comparable.is_operator = true;
        assert_eq!(
            semantic_call_role(comparable),
            Some(SemanticCallRole::KotlinComparableCompareTo)
        );
        assert_eq!(
            semantic_call_role(declaration(
                crate::types::wk::comparable(),
                "compareTo",
                &[Ty::String],
                Ty::Int,
            )),
            None,
            "the Comparable owner and spelling do not replace its generic declaration signature"
        );

        let mut callable_name = declaration(
            crate::types::type_name("kotlin/reflect/KCallable"),
            "name",
            &[],
            Ty::String,
        );
        callable_name.is_property = true;
        assert_eq!(
            semantic_call_role(callable_name),
            Some(SemanticCallRole::KotlinCallableReferenceName)
        );

        let receiver = Ty::ty_param("T", Ty::obj_name(any));
        let value = Ty::ty_param("V", Ty::obj_name(any));
        let get_parameters = [receiver];
        assert_eq!(
            semantic_call_role(declaration(
                crate::types::type_name("kotlin/reflect/KProperty1"),
                "get",
                &get_parameters,
                value,
            )),
            Some(SemanticCallRole::KotlinPropertyReferenceGet(1))
        );
        assert_eq!(
            semantic_call_role(declaration(
                crate::types::type_name("fixture/KProperty1"),
                "get",
                &get_parameters,
                value,
            )),
            None,
            "a reflection-shaped lookalike has no language role"
        );
    }
}
