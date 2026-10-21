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
pub(super) fn ordinary_declarations(
    platform: &dyn SemanticPlatform,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
    roots: &[KlibBodyCallable<'_>],
    externals: &HashMap<KlibDependencyCallable, super::EmittedDependencyCallable>,
    folds_common_builtin: impl Fn(&KlibDependencyCallable) -> bool,
) -> Result<Vec<ExternalCallableRealization>, String> {
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
        if !externals.contains_key(&identity) && folds_common_builtin(&identity) {
            continue;
        }
        let (_, realization) = platform
            .external_callable_by_dependency(&identity)
            .ok_or_else(|| {
                format!("an ordinary KLIB body has no exact provider declaration: {identity:?}")
            })?;
        if realization.dependency_callable.as_ref() != Some(&identity) {
            return Err("an ordinary KLIB body provider changed its exact identity".to_owned());
        }
        let emits_body = !externals.contains_key(&identity)
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        if emits_body
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
            pending.extend(bodies.direct_callees(&identity));
        }
        declarations.push(realization);
    }
    Ok(declarations)
}

pub(super) fn ordinary_roots<'a>(
    source: &'a CheckedIrFile<'_>,
    bodies: &KlibDeclarationBodies,
    intrinsics: &KlibIntrinsicFacts,
) -> Result<Vec<KlibBodyCallable<'a>>, &'static str> {
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
