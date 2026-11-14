//! Positive declaration capabilities shared by dependency planning and Native emission.

use krusty::libraries::{CompilerIntrinsic, ListStorageConstruction};
use krusty::types::{Ty, TypeName};

/// Precompute support-body runtime calls from an exact checked declaration and serialized body.
/// The returned plan drives both declaration admission and body emission for the same FunId.
pub(super) fn runtime_companion_functions(
    ir: &krusty::ir::IrFile,
    callables: &krusty::backend::CheckedBackendCallables,
    unit_kind: krusty::backend::CheckedIrUnitKind,
) -> Result<krusty::fx_hash::HashMap<krusty::ir::FunId, &'static str>, ()> {
    let mut runtime_functions = krusty::fx_hash::HashMap::default();
    if unit_kind != krusty::backend::CheckedIrUnitKind::DependencySupport {
        return Ok(runtime_functions);
    }
    for (&identity, &function_id) in &ir.dependency_methods {
        let Some(function) = ir.functions.get(function_id as usize) else {
            continue;
        };
        if function.body.is_none()
            || ir.inline_only_fns.contains(&function_id)
            || function.is_static
        {
            continue;
        }
        let Some(owner) = function.dispatch_receiver else {
            continue;
        };
        let Some(realization) = callables.callable(identity) else {
            continue;
        };
        let Some(dependency) = realization.dependency_callable.as_ref() else {
            continue;
        };
        let Some(class_contract) = realization.klib_body_class_contract.as_ref() else {
            continue;
        };
        if !class_contract.matches_owner(&dependency.module, owner) {
            continue;
        }
        let Some(modifiers) = realization.klib_body_declaration_modifiers else {
            continue;
        };
        let Some(generic_signature) = realization.generic_sig.as_deref() else {
            // A physical or applied signature cannot stand in for the declaration contract.
            continue;
        };
        let params = generic_signature.params.as_slice();
        let name = realization
            .reflection_name
            .as_deref()
            .unwrap_or(&realization.name);
        let declaration = RuntimeCompanionDeclaration {
            owner,
            contract_matches_owner: class_contract.matches_owner(&dependency.module, owner),
            is_object: class_contract.is_object(),
            is_member: realization.is_member_function(),
            declaration_owner: realization.declaration_owner,
            physical_owner: realization.physical_owner,
            has_reified_parameters: !realization.reified_type_parameter_ordinals.is_empty(),
            has_extension_receiver: realization.source_receiver.is_some()
                || generic_signature.receiver.is_some(),
            context_count: realization.context_count,
            is_suspend: modifiers.is_suspend(),
            params,
            ret: generic_signature.ret,
            name,
        };
        let Some(symbol) = runtime_companion_symbol(unit_kind, function, declaration) else {
            continue;
        };
        if runtime_functions.insert(function_id, symbol).is_some() {
            return Err(());
        }
    }
    Ok(runtime_functions)
}

struct RuntimeCompanionDeclaration<'a> {
    owner: TypeName,
    contract_matches_owner: bool,
    is_object: bool,
    is_member: bool,
    declaration_owner: Option<krusty::types::SemanticCallableOwner>,
    physical_owner: TypeName,
    has_reified_parameters: bool,
    has_extension_receiver: bool,
    context_count: usize,
    is_suspend: bool,
    params: &'a [Ty],
    ret: Ty,
    name: &'a str,
}

fn runtime_companion_symbol(
    unit_kind: krusty::backend::CheckedIrUnitKind,
    function: &krusty::ir::IrFunction,
    declaration: RuntimeCompanionDeclaration<'_>,
) -> Option<&'static str> {
    if unit_kind != krusty::backend::CheckedIrUnitKind::DependencySupport
        || function.body.is_none()
        || function.is_static
        || function.name != declaration.name
        || function.dispatch_receiver != Some(declaration.owner)
        || function.params.as_slice() != declaration.params
        || function.ret != declaration.ret
        || !function.ret.is_reference()
        || !declaration.ret.is_reference()
        || !declaration.contract_matches_owner
        || !declaration.is_object
        || !declaration.is_member
        || declaration.declaration_owner
            != Some(krusty::types::SemanticCallableOwner::Classifier(
                declaration.owner,
            ))
        || declaration.physical_owner != declaration.owner
        || declaration.has_reified_parameters
        || declaration.has_extension_receiver
        || declaration.context_count != 0
        || declaration.is_suspend
    {
        return None;
    }
    let owner = super::intrinsics::DeclarationOwner::callable(
        declaration.physical_owner,
        declaration.declaration_owner,
    );
    super::intrinsics::runtime_companion_member(owner, declaration.name, declaration.params)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DependencyOperation {
    ListStorage(ListStorageConstruction),
    StringPlus,
    /// `String.compareTo(String)`, the lexicographic order of the two strings' UTF-16 units.
    StringCompare,
    /// A builtin scalar's own `hashCode()`, which Kotlin fixes per scalar type.
    PrimitiveHashCode,
    /// A primitive iterator's own `next()` (`IntIterator.next(): Int`). The runtime's iteration
    /// protocol answers it through the receiver's descriptor, as it answers `Iterator.next`.
    PrimitiveIteratorNext,
}

pub(super) fn operation_plan(operation: CompilerIntrinsic) -> Option<DependencyOperation> {
    match operation {
        CompilerIntrinsic::ListStorageConstruction(plan) => {
            Some(DependencyOperation::ListStorage(plan))
        }
        CompilerIntrinsic::StringPlus => Some(DependencyOperation::StringPlus),
        CompilerIntrinsic::StringCompare => Some(DependencyOperation::StringCompare),
        CompilerIntrinsic::PrimitiveHashCode => Some(DependencyOperation::PrimitiveHashCode),
        CompilerIntrinsic::PrimitiveIteratorNext => {
            Some(DependencyOperation::PrimitiveIteratorNext)
        }
        _ => None,
    }
}

/// The operation of a selected dependency declaration. Platform providers can preserve an
/// ordinary physical call while publishing its language role for targets with their own values.
pub(super) fn declaration_operation(
    intrinsic: Option<CompilerIntrinsic>,
    role: Option<krusty::types::SemanticCallRole>,
) -> Option<DependencyOperation> {
    match intrinsic {
        Some(intrinsic) => operation_plan(intrinsic),
        None => match role {
            Some(krusty::types::SemanticCallRole::KotlinStringCompareTo) => {
                Some(DependencyOperation::StringCompare)
            }
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CraneliftBackend, NativeTarget};
    use krusty::backend::Backend;

    fn not_null_function(ret: Ty) -> krusty::ir::IrFunction {
        krusty::ir::IrFunction {
            name: "notNull".to_owned(),
            params: Vec::new(),
            ret,
            body: Some(0),
            is_static: false,
            dispatch_receiver: Some(krusty::types::type_name("kotlin/properties/Delegates")),
            param_checks: Vec::new(),
        }
    }

    fn not_null_declaration(ret: Ty) -> RuntimeCompanionDeclaration<'static> {
        let owner = krusty::types::type_name("kotlin/properties/Delegates");
        RuntimeCompanionDeclaration {
            owner,
            contract_matches_owner: true,
            is_object: true,
            is_member: true,
            declaration_owner: Some(krusty::types::SemanticCallableOwner::Classifier(owner)),
            physical_owner: owner,
            has_reified_parameters: false,
            has_extension_receiver: false,
            context_count: 0,
            is_suspend: false,
            params: &[],
            ret,
            name: "notNull",
        }
    }

    #[test]
    fn runtime_companion_plan_requires_an_exact_support_declaration_frame() {
        let formal = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let ret = Ty::obj_args(
            "kotlin/properties/ReadWriteProperty",
            &[Ty::nullable(Ty::obj("kotlin/Any")), formal],
        );
        let function = not_null_function(ret);
        let declaration = not_null_declaration(ret);

        assert_eq!(
            runtime_companion_symbol(
                krusty::backend::CheckedIrUnitKind::DependencySupport,
                &function,
                declaration,
            ),
            Some("kt_not_null_var")
        );
        assert_eq!(
            runtime_companion_symbol(
                krusty::backend::CheckedIrUnitKind::Source,
                &function,
                not_null_declaration(ret),
            ),
            None,
            "source-unit functions do not receive support wrappers"
        );

        let other_formal = Ty::ty_param("U", Ty::obj("kotlin/Any"));
        let other_ret = Ty::obj_args(
            "kotlin/properties/ReadWriteProperty",
            &[Ty::nullable(Ty::obj("kotlin/Any")), other_formal],
        );
        assert_eq!(
            runtime_companion_symbol(
                krusty::backend::CheckedIrUnitKind::DependencySupport,
                &function,
                not_null_declaration(other_ret),
            ),
            None,
            "a same-carrier result with a different generic formal is not the same frame"
        );
        assert_eq!(
            runtime_companion_symbol(
                krusty::backend::CheckedIrUnitKind::DependencySupport,
                &function,
                not_null_declaration(Ty::String),
            ),
            None,
            "a different declared result is rejected even when both are references"
        );
    }

    #[test]
    fn runtime_companion_plan_requires_the_exact_object_member_owner_and_role() {
        let ret = Ty::obj_args(
            "kotlin/properties/ReadWriteProperty",
            &[Ty::nullable(Ty::obj("kotlin/Any")), Ty::String],
        );
        let function = not_null_function(ret);
        for declaration in [
            RuntimeCompanionDeclaration {
                contract_matches_owner: false,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                is_object: false,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                is_member: false,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                declaration_owner: None,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                physical_owner: krusty::types::type_name("kotlin/Any"),
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                has_extension_receiver: true,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                context_count: 1,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                is_suspend: true,
                ..not_null_declaration(ret)
            },
            RuntimeCompanionDeclaration {
                name: "observable",
                ..not_null_declaration(ret)
            },
        ] {
            assert_eq!(
                runtime_companion_symbol(
                    krusty::backend::CheckedIrUnitKind::DependencySupport,
                    &function,
                    declaration,
                ),
                None
            );
        }
        let mut wrong_owner = not_null_declaration(ret);
        wrong_owner.owner = krusty::types::type_name("kotlin/Any");
        assert_eq!(
            runtime_companion_symbol(
                krusty::backend::CheckedIrUnitKind::DependencySupport,
                &function,
                wrong_owner,
            ),
            None
        );
    }

    #[test]
    fn klib_target_string_plus_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::StringPlus));
        }
    }

    #[test]
    fn klib_string_compare_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::StringCompare));
        }
    }

    #[test]
    fn a_platform_string_comparison_role_selects_the_same_native_operation() {
        use krusty::types::SemanticCallRole;
        assert_eq!(
            declaration_operation(None, Some(SemanticCallRole::KotlinStringCompareTo)),
            operation_plan(CompilerIntrinsic::StringCompare)
        );
        assert_eq!(
            declaration_operation(None, Some(SemanticCallRole::KotlinComparableCompareTo)),
            None,
            "an arbitrary Comparable implementation is not string comparison"
        );
        assert_eq!(declaration_operation(None, None), None);
    }

    #[test]
    fn klib_primitive_hash_code_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::PrimitiveHashCode));
        }
    }

    #[test]
    fn klib_primitive_iterator_next_is_a_native_dependency_capability() {
        for &target in NativeTarget::ALL {
            assert!(CraneliftBackend::new(target)
                .realizes_dependency_intrinsic(CompilerIntrinsic::PrimitiveIteratorNext));
        }
    }

    #[test]
    fn library_algorithm_adapters_are_not_native_dependency_capabilities() {
        for &target in NativeTarget::ALL {
            let backend = CraneliftBackend::new(target);
            for operation in [
                CompilerIntrinsic::TextTrim,
                CompilerIntrinsic::CollectionSizeOrDefault,
                CompilerIntrinsic::ResultFailure(krusty::libraries::ResultFailureIntrinsic::Create),
                CompilerIntrinsic::UnsignedDivide {
                    operand: krusty::types::Ty::Int,
                },
                CompilerIntrinsic::UnsignedRemainder {
                    operand: krusty::types::Ty::Int,
                },
            ] {
                assert!(
                    !backend.realizes_dependency_intrinsic(operation),
                    "{target:?}: {operation:?}"
                );
            }
        }
    }
}
