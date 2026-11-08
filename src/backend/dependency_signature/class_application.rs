//! Class-dependent slots consume the complete application FIR already checked. Callable type
//! arguments remain a separate declaration-owned substitution namespace.

use super::*;
use crate::fx_hash::HashSet;
use crate::types::SemanticCallableOwner;

pub(super) fn apply(
    fact: &BackendCallableFact,
    application: &CheckedDependencyApplication<'_>,
    mut signature: InstantiatedDependencySignature,
) -> Result<InstantiatedDependencySignature, &'static str> {
    let Some(class) = fact
        .klib_body_class_contract
        .as_ref()
        .filter(|class| !class.parameters.formals.is_empty())
    else {
        return Ok(signature);
    };
    let count = class.parameters.formals.len();
    if !fact.is_member_function()
        || fact.declaration_owner != Some(SemanticCallableOwner::Classifier(class.classifier))
        || fact
            .dependency_callable
            .as_ref()
            .is_none_or(|identity| identity.module != class.module)
        || class.signature.member_id().is_some()
        || class.parameters.bounds.len() != count
        || class.parameters.variances.len() != count
        || class.parameters.own_count > count
        || fact.parameter_identities.len() != fact.params.len()
        || fact.context_count > fact.params.len()
    {
        return Err("a dependency class application has an inconsistent declaring-class contract");
    }
    let mut seen = HashSet::default();
    let mut formals = Vec::with_capacity(count);
    for formal in &class.parameters.formals {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return Err("a dependency class application has a non-formal class parameter");
        };
        if !seen.insert(identity)
            || fact
                .generic_sig
                .as_ref()
                .is_some_and(|callable| callable.formals.contains(&identity))
        {
            return Err("a dependency class application repeats a formal identity");
        }
        formals.push(identity.as_str().to_owned());
    }
    if signature.parameters.len() != application.parameters.len() {
        return Err("a dependency class application changed its parameter count");
    }
    for (declared, checked) in signature.parameters.iter_mut().zip(application.parameters) {
        if crate::types::ty_mentions_param(*declared, &formals) {
            // This complete type is a checked application, not a template to unify or retry.
            *declared = *checked;
        } else if *declared != *checked {
            return Err(
                "a fixed dependency parameter disagrees with its checked class application",
            );
        }
    }
    if crate::types::ty_mentions_param(signature.result, &formals) {
        signature.result = application.result;
    } else if signature.result != application.result {
        return Err("a fixed dependency result disagrees with its checked class application");
    }
    match (signature.receiver, application.extension_receiver_parameter) {
        (None, None) => {}
        (Some(receiver), Some(position)) if position as usize == fact.context_count => {
            let checked = application
                .parameters
                .get(position as usize)
                .ok_or("a dependency class application lost its extension receiver")?;
            if crate::types::ty_mentions_param(receiver, &formals) {
                signature.receiver = Some(*checked);
            } else if receiver != *checked {
                return Err(
                    "a fixed dependency receiver disagrees with its checked class application",
                );
            }
        }
        _ => return Err("a dependency class application changed its receiver roles"),
    }
    Ok(signature)
}
