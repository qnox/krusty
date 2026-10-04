//! JVM call-site realizations published by selected dependency declarations.
//!
//! Providers attach physical candidates while source declarations and target policy are joined.
//! This pass selects one only through the checked callable identity, then records the exact plan on
//! the expression. Emission consumes that plan without name lookup.

use crate::backend::CheckedBackendCallables;
use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{Callee, IrExpr, IrFile, IrVirtualTarget};
use crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal;
use crate::libraries::OverriddenCallRealization;
use crate::types::TypeName;

use super::member_dispatch::{CheckedDispatchClassifiers, MissingClassifier};

fn callable_candidates(
    callables: &CheckedBackendCallables,
    target: crate::fir::ExternalCallableId,
) -> Vec<OverriddenCallRealization> {
    callables
        .callable(target)
        .map(|fact| fact.overridden_call_realizations.to_vec())
        .unwrap_or_default()
}

fn function_candidates(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    target: ResolvedFunctionOverrideTarget,
) -> Vec<OverriddenCallRealization> {
    let mut candidates = Vec::new();
    let mut pending = vec![target];
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        match current {
            ResolvedFunctionOverrideTarget::External(target) => {
                candidates.extend(callable_candidates(callables, target));
            }
            ResolvedFunctionOverrideTarget::Module(_) => {
                pending.extend(
                    ir.function_overrides
                        .values()
                        .flatten()
                        .filter(|edge| edge.implementation == current)
                        .map(|edge| edge.overridden),
                );
            }
        }
    }
    candidates
}

fn property_getter_candidates(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    target: crate::fir::PropertyId,
) -> Vec<OverriddenCallRealization> {
    let mut candidates = Vec::new();
    let mut pending = vec![crate::fir::ResolvedPropertyOverrideTarget::Module(target)];
    let mut seen = std::collections::HashSet::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        match current {
            crate::fir::ResolvedPropertyOverrideTarget::External(getter) => {
                candidates.extend(callable_candidates(callables, getter));
            }
            crate::fir::ResolvedPropertyOverrideTarget::Module(_) => {
                pending.extend(
                    ir.property_overrides
                        .values()
                        .flatten()
                        .filter(|edge| edge.implementation == current)
                        .map(|edge| edge.overridden),
                );
            }
        }
    }
    candidates
}

fn select(
    classifiers: &CheckedDispatchClassifiers<'_>,
    owner: TypeName,
    candidates: Vec<OverriddenCallRealization>,
) -> Result<Option<OverriddenCallRealization>, MissingClassifier> {
    for candidate in candidates {
        if type_names_map_to_same_jvm_internal(owner, candidate.declaration_owner)
            || super::member_dispatch::inherits(classifiers, owner, candidate.declaration_owner)?
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Select dependency call-site realizations after JVM transforms have finalized call owners.
pub(super) fn realize(
    ir: &mut IrFile,
    classifiers: &dyn crate::backend::BackendClassifierSource,
    callables: &CheckedBackendCallables,
) -> Result<(), MissingClassifier> {
    let dispatch = CheckedDispatchClassifiers::new(ir, classifiers);
    let mut plans = Vec::new();
    for (raw, expression) in ir.exprs.iter().enumerate() {
        let id = u32::try_from(raw).expect("too many common IR expressions");
        let (plan_expression, owner, candidates) = match expression {
            IrExpr::Call {
                callee: Callee::Virtual { owner, target, .. },
                ..
            } => {
                let candidates = match target {
                    Some(IrVirtualTarget::Function(target)) => {
                        function_candidates(ir, callables, *target)
                    }
                    Some(IrVirtualTarget::PropertyGetter(target)) => {
                        property_getter_candidates(ir, callables, *target)
                    }
                    // Every JVM special property is read-only; an overriding source setter has no
                    // overridden declaration whose getter policy could apply to it.
                    Some(IrVirtualTarget::PropertySetter(_)) | None => Vec::new(),
                };
                (id, *owner, candidates)
            }
            _ => continue,
        };
        if candidates.is_empty() {
            continue;
        }
        if let Some(realization) = select(&dispatch, owner, candidates)? {
            plans.push((plan_expression, realization));
        }
    }
    ir.jvm_overridden_call_realizations.extend(plans);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendClassifierFact, BackendClassifierSource};
    use crate::libraries::ClassifierAccess;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default)]
    struct Facts(HashMap<TypeName, Arc<BackendClassifierFact>>);

    impl BackendClassifierSource for Facts {
        fn classifier(&self, classifier: TypeName) -> Option<Arc<BackendClassifierFact>> {
            self.0.get(&classifier).cloned()
        }
    }

    fn class(supertypes: &[TypeName]) -> Arc<BackendClassifierFact> {
        Arc::new(BackendClassifierFact {
            access: ClassifierAccess::Public,
            is_kotlin: true,
            source: true,
            outer_instance: None,
            kind: crate::libraries::TypeKind::Class,
            is_abstract: false,
            is_extensible: true,
            supertypes: supertypes.into(),
            surface: Box::new([]),
            annotations: Box::new([]),
            own_type_parameter_count: 0,
            type_param_variances: Box::new([]),
            value_underlying: None,
            value_underlying_property: None,
            role: None,
        })
    }

    fn realization(owner: TypeName) -> OverriddenCallRealization {
        OverriddenCallRealization {
            declaration_owner: owner,
            physical_name: "physicalOperation".to_string(),
            descriptor: "()Ljava/lang/Object;".to_string(),
        }
    }

    #[test]
    fn selects_a_provider_plan_through_an_opaque_non_stdlib_hierarchy() {
        let base = crate::types::type_name("review/DeclarationBase");
        let middle = crate::types::type_name("review/DeclarationMiddle");
        let leaf = crate::types::type_name("review/DeclarationLeaf");
        let mut facts = Facts::default();
        facts.0.insert(base, class(&[]));
        facts.0.insert(middle, class(&[base]));
        facts.0.insert(leaf, class(&[middle]));
        let ir = IrFile::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);
        let expected = realization(base);

        assert_eq!(
            select(&classifiers, leaf, vec![expected.clone()]),
            Ok(Some(expected))
        );
    }

    #[test]
    fn exact_declaration_identity_needs_no_classifier_fallback() {
        let owner = crate::types::type_name("review/ExactDeclaration");
        let ir = IrFile::default();
        let facts = Facts::default();
        let classifiers = CheckedDispatchClassifiers::new(&ir, &facts);
        let expected = realization(owner);

        assert_eq!(
            select(&classifiers, owner, vec![expected.clone()]),
            Ok(Some(expected))
        );
    }
}
