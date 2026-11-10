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
    // Each callee, with the value class whose member's body calls it.
    let mut pending = roots
        .iter()
        .map(|root| (root.identity().clone(), None))
        .collect::<VecDeque<_>>();
    let mut visited = HashSet::default();
    let mut declarations = Vec::new();
    let mut classes = crate::klib_lowering::ClassInitializations::default();
    while let Some((identity, value_class)) = pending.pop_front() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        // A private member of the class whose body calls it has no provider declaration. The
        // caller authenticates the class, and the member is emitted as that caller is. The class
        // is a value class or an ordinary class whose member body this walk already admitted.
        if let Some(class) = value_class.filter(|class| {
            platform
                .external_callable_by_dependency(&identity)
                .is_none()
                && !externals.contains_key(&identity)
                && crate::klib_lowering::is_private_value_member(bodies, &identity, class)
        }) {
            pending.extend(
                bodies
                    .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                    .into_iter()
                    .map(|callee| (callee, Some(class.clone()))),
            );
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
        let (declaration_target, realization) = match platform
            .external_callable_by_dependency(&identity)
        {
            Some(declaration) => declaration,
            None if raw_header_capability && known_target.is_none() => {
                pending.extend(
                    bodies
                        .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                        .into_iter()
                        .map(|callee| (callee, None)),
                );
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
            if let Some(callees) = classes
                .callees(&facts, &identity, bodies)
                .map_err(|reason| format!("an invalid class initialization: {reason:?}"))?
            {
                pending.extend(callees.into_iter().map(|callee| (callee, None)));
            }
        }
        let primitive = facts
            .common_primitive(&identity, bodies)
            .map_err(|reason| format!("an invalid common primitive declaration: {reason:?}"))?;
        let target = facts
            .target_intrinsic(&identity, bodies, capabilities.target_realizes_intrinsic)
            .map_err(|reason| format!("an invalid target declaration: {reason:?}"))?;
        let getter = !externals.contains_key(&identity)
            && primitive.is_none()
            && target.is_none()
            && intrinsics.get(&identity).is_none()
            && facts
                .target_getter(&identity, bodies, capabilities.target_realizes_getter)
                .map_err(|reason| format!("an invalid target getter declaration: {reason:?}"))?;
        let member = facts
            .target_member(&identity, capabilities.target_represents_classifier)
            .map_err(|reason| format!("an invalid target member declaration: {reason:?}"))?;
        // A member of a class this unit materializes is installed with its class: the class's
        // initialization and members are the edges it executes.
        let class_member = !externals.contains_key(&identity)
            && !constructor
            && !member
            && realization.kind == ExternalCallableKind::Member
            && !is_value_class_member(selected)
            // A producer that publishes compiled code without the body realizes the member.
            && bodies.function_has_body(&identity) == Some(true)
            && match classes
                .callees(&facts, &identity, bodies)
                .map_err(|reason| format!("an invalid class member declaration: {reason:?}"))?
            {
                Some(callees) => {
                    pending.extend(callees.into_iter().map(|callee| (callee, None)));
                    true
                }
                None => false,
            };
        let emits_body = !externals.contains_key(&identity)
            && !class_member
            && !member
            && !constructor
            && primitive.is_none()
            && target.is_none()
            && !getter
            && intrinsics.get(&identity).is_none()
            && bodies.function_has_body(&identity) == Some(true);
        if requires_ordinary
            && emits_body
            && (!(matches!(
                realization.kind,
                ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
            ) || is_value_class_member(selected)
                || selected.closes_as_ordinary_member())
                || !realization
                    .klib_body_declaration_modifiers
                    .is_some_and(|modifiers| modifiers.permits_ordinary_body()))
        {
            return Err(format!("a transitive KLIB body has no supported ordinary declaration contract: {identity:?}"));
        }
        if emits_body {
            // Private members of this class are reached with the class, not through a provider.
            let value_class = (is_value_class_member(selected)
                || selected.closes_as_ordinary_member())
            .then(|| selected.class_contract().cloned())
            .flatten();
            pending.extend(
                bodies
                    .emitted_body_callables(&identity, crate::klib_lowering::EMITTED_CALL_FORMS)
                    .into_iter()
                    .map(|callee| (callee, value_class.clone())),
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
    target_represents_classifier: &dyn Fn(crate::types::TypeName) -> bool,
) -> Result<Vec<KlibBodyCallable<'a>>, String> {
    let mut roots = Vec::new();
    let mut seen = HashSet::default();
    let mut classes = crate::klib_lowering::ClassInitializations::default();
    for expression in crate::ir::emitted_body_roots(&source.ir)
        .into_iter()
        .flat_map(|root| crate::ir::value_namespace_expressions(&source.ir, root))
    {
        let target = match source.ir.expr(expression) {
            IrExpr::Call { callee, .. } => match &**callee {
                // A call leaving arguments out reaches the same body through its default entry.
                Callee::External { target, .. } => *target,
                _ => continue,
            },
            IrExpr::New {
                external_target: Some(target),
                ..
            } => *target,
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
        let Some(selected) = source.callables.klib_body_callable_at(expression, target) else {
            continue;
        };
        let construction = matches!(source.ir.expr(expression), IrExpr::New { .. });
        if construction != (callable.kind == ExternalCallableKind::Constructor) {
            return Err(
                "an emitted dependency construction disagrees with its selected callable role"
                    .to_owned(),
            );
        }
        if construction {
            // A target can already provide a constructor whose KLIB publishes metadata only.
            // Only an executable serialized declaration asks this phase to realize its body.
            if bodies.function_has_body(selected.identity()) != Some(true) {
                continue;
            }
            if !seen.contains(selected.identity())
                && classes
                    .callees(callees, selected.identity(), bodies)
                    .map_err(|reason| format!("an invalid constructor root: {reason:?}"))?
                    .is_some()
            {
                seen.insert(selected.identity());
                roots.push(selected);
            }
            continue;
        }
        if seen.contains(selected.identity()) {
            continue;
        }
        // A member of a classifier the target represents itself is the target's to realize.
        if callees
            .target_member(selected.identity(), target_represents_classifier)
            .map_err(|reason| format!("an invalid target member declaration: {reason:?}"))?
        {
            continue;
        }
        // This category accepts declaration-owned top-level and extension frames, the members of
        // a value class, which are final and read their receiver only through its one value, and
        // the serialized members of a final class this unit materializes with all of its members.
        if !callable.is_top_level()
            && !is_value_class_member(selected)
            && (callable.kind != ExternalCallableKind::Member
                || bodies.function_has_body(selected.identity()) != Some(true)
                || classes
                    .callees(callees, selected.identity(), bodies)
                    .map_err(|reason| format!("an invalid class member root: {reason:?}"))?
                    .is_none())
        {
            continue;
        }
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

/// Whether `callable` is a member of a value class which a support unit emits as a member of the
/// class it materializes. A builtin type of the common type model is not such a class.
fn is_value_class_member(callable: KlibBodyCallable<'_>) -> bool {
    callable.kind() == ExternalCallableKind::Member
        && callable
            .class_contract()
            .is_some_and(|class| class.is_value && !class.is_builtin_type())
}
