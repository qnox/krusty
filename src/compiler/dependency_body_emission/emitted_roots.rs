//! Exact ordinary dependency declarations referenced by structurally emitted expression bodies.

use crate::backend::CheckedIrFile;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, IrCheckedOperation, IrExpr};
use crate::klib_libraries::{KlibDeclarationBodies, KlibIntrinsicFacts};
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, KlibBodyCallable, KlibDependencyCallable,
    SemanticPlatform,
};
use std::collections::VecDeque;

/// Freeze declaration contracts along the exact producer graph before ordinary body lowering.
/// A body-bearing callee cannot regain the legacy header-only path merely because source FIR did
/// not also select it. Intrinsic/bodyless leaves retain their separately authenticated boundary.
pub(super) fn declarations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
    roots: &[KlibBodyCallable<'_>],
    externals: &HashMap<KlibDependencyCallable, super::EmittedDependencyCallable>,
    capabilities: &super::DependencyLeafCapabilities<'_>,
    requires_ordinary: bool,
) -> Result<Vec<(crate::fir::ExternalCallableId, ExternalCallableRealization)>, String> {
    let mut pending = roots
        .iter()
        .map(|root| root.identity().clone())
        .collect::<VecDeque<_>>();
    let mut visited = HashSet::default();
    let mut declarations = Vec::new();
    while let Some(identity) = pending.pop_front() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        // Positive common-lowering capability, never a missing-provider fallback. Earlier emitted
        // exports remain exact external edges and retain their declaration authentication.
        if !externals.contains_key(&identity) && capabilities.folds_common_builtin(&identity) {
            continue;
        }
        // Intrinsic support already admits body-bearing raw headers without a source/provider
        // selection. Optional enrichment does not invent a primitive on a miss. Ordinary bodies,
        // retained exports, and target-intrinsic/bodyless leaves still require their provider.
        let raw_header_capability = !requires_ordinary
            && !externals.contains_key(&identity)
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        let known_target = capabilities
            .callees
            .target_intrinsic(&identity, bodies, capabilities.target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?;
        let (declaration_target, realization) =
            match platform.external_callable_by_dependency(&identity) {
                Some(declaration) => declaration,
                None if raw_header_capability && known_target.is_none() => {
                    pending.extend(bodies.emitted_body_callables(
                        &identity,
                        crate::klib_lowering::EMITTED_CALL_FORMS,
                    ));
                    continue;
                }
                None => {
                    return Err(format!(
                    "a KLIB dependency has no required exact provider declaration: {identity:?}"
                ))
                }
            };
        if realization.dependency_callable.as_ref() != Some(&identity) {
            return Err("an ordinary KLIB body provider changed its exact identity".to_owned());
        }
        let selected = realization
            .klib_body_callable()
            .ok_or("a dependency provider has no KLIB body contract")?;
        let facts = capabilities.callees.with_declaration(selected);
        let constructor = facts
            .authenticates_constructor(&identity, bodies)
            .map_err(|reason| format!("an invalid constructor declaration: {reason:?}"))?;
        if constructor {
            if let Some(callees) = facts
                .constructor_initialization_callees(&identity, bodies)
                .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
            {
                pending.extend(callees);
            }
        }
        let primitive = facts
            .common_primitive(&identity, bodies)
            .map_err(|reason| format!("an invalid common primitive declaration: {reason:?}"))?;
        let target = facts
            .target_intrinsic(&identity, bodies, capabilities.target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?;
        let emits_body = !externals.contains_key(&identity)
            && !constructor
            && primitive.is_none()
            && target.is_none()
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        if requires_ordinary
            && emits_body
            && (!matches!(
                realization.kind,
                ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
            ) || !realization
                .klib_body_declaration_modifiers
                .is_some_and(|modifiers| modifiers.permits_ordinary_body()))
        {
            return Err(format!("a transitive KLIB body has no supported ordinary declaration contract: {identity:?}"));
        }
        if emits_body {
            pending.extend(
                bodies.emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS),
            );
        }
        declarations.push((declaration_target, realization));
    }
    Ok(declarations)
}

pub(super) fn ordinary_roots<'a>(
    source: &'a CheckedIrFile<'_>,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
    callees: &crate::klib_lowering::KlibCalleeFacts<'a>,
    target_realizes_intrinsic: &dyn Fn(crate::libraries::CompilerIntrinsic) -> bool,
) -> Result<Vec<KlibBodyCallable<'a>>, String> {
    let mut roots = Vec::new();
    let mut seen = HashSet::default();
    for expression in crate::ir::emitted_body_roots(&source.ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&source.ir, root))
    {
        let target = match source.ir.expr(expression) {
            IrExpr::Call { callee, .. } => match &**callee {
                Callee::External {
                    target, defaults, ..
                } if defaults.is_empty() => *target,
                _ => continue,
            },
            IrExpr::Checked(operation) => match &**operation {
                IrCheckedOperation::ExternalPropertyRead { target, .. } => {
                    source
                        .callables
                        .property(*target)
                        .ok_or("an emitted dependency property has no frozen fact")?
                        .getter
                }
                IrCheckedOperation::ExternalPropertyWrite { target, .. } => source
                    .callables
                    .property(*target)
                    .ok_or("an emitted dependency property has no frozen fact")?
                    .setter
                    .ok_or("an emitted dependency property write has no selected setter")?,
                _ => continue,
            },
            _ => continue,
        };
        let callable = source
            .callables
            .callable(target)
            .ok_or("an emitted dependency callable has no frozen fact")?;
        // Instance dispatch/storage is a different emission capability. This category accepts
        // declaration-owned top-level and extension frames, not instance-reading member bodies.
        if !callable.is_top_level() {
            continue;
        }
        let Some(selected) = source.callables.klib_body_callable_at(expression, target) else {
            continue;
        };
        if callees
            .target_intrinsic(selected.identity(), bodies, target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?
            .is_some()
        {
            continue;
        }
        let Some(modifiers) = callable.klib_body_declaration_modifiers else {
            continue;
        };
        if !modifiers.permits_ordinary_body()
            || intrinsics.get(selected.identity()).is_some()
            || bodies.function_has_body(selected.identity()) != Some(true)
        {
            continue;
        }
        // The complete selected/raw header is authenticated by transactional unit lowering.
        // A malformed known ordinary body is an obligation, not a failed eligibility probe.
        if seen.insert(selected.identity()) {
            roots.push(selected);
        }
    }
    Ok(roots)
}
