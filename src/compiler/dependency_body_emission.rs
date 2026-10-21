//! Production lowering and emission of exact selected KLIB dependency bodies.
//!
//! The semantic provider supplies one exact module-qualified body graph and its authenticated
//! intrinsic declarations. Common lowering follows serialized call identities through that graph.
//! A successfully closed unit is emitted before its source caller and publishes every transitive
//! declaration it realizes through the ordinary dependency-export registry, so target call
//! emission performs no lookup or spelling dispatch.

use crate::backend::{
    Artifact, Backend, BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers,
    CheckedIrFile, CheckedIrUnitKind, DependencyExportSignatures,
};
use crate::diag::{DiagSink, Span};
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{Callee, IrExpr};
use crate::klib_lowering::{DependencyBodyUnit, EmittedDependencyCallable, KlibCalleeFacts};
use crate::libraries::KlibDependencyCallable;
use crate::resolve::PassTwoSymbols;
use crate::types::{Ty, TyKind, TypeName};
use std::collections::VecDeque;

mod emitted_roots;

/// Exact dependency support declarations already emitted into one target program.
///
/// This state outlives independently analyzed source modules. It retains the semantic singleton
/// receiver contract selected when each declaration was emitted, but deliberately stores no opaque
/// [`ExternalCallableId`](crate::fir::ExternalCallableId): each module owns a fresh provider and
/// rebinds these exact KLIB identities to that provider's IDs before lowering an external edge.
#[derive(Default)]
pub(super) struct DependencySupportState {
    snapshot: Option<crate::libraries::DependencyBodySnapshot>,
    emitted: HashMap<KlibDependencyCallable, Option<PublishedSingletonDispatch>>,
}

#[derive(Clone, Copy)]
struct PublishedSingletonDispatch {
    ty: Ty,
    classifier: TypeName,
}

impl DependencySupportState {
    fn accepts_snapshot(&mut self, snapshot: &crate::libraries::DependencyBodySnapshot) -> bool {
        match &self.snapshot {
            Some(selected) => selected == snapshot,
            None => {
                self.snapshot = Some(snapshot.clone());
                true
            }
        }
    }
}

/// Emit authenticated intrinsic support and concrete ordinary bodies selected by emitted uses.
/// Each category closes and validates independently, once across the complete target program.
pub(super) fn emit_dependencies<B: Backend>(
    source: &CheckedIrFile<'_>,
    symbols: &PassTwoSymbols,
    module_facts: &BackendModuleFacts,
    backend: &B,
    state: &mut B::State,
    support_state: &mut DependencySupportState,
    diags: &mut DiagSink,
) -> Vec<Artifact> {
    if !backend.emits_dependency_support() {
        return Vec::new();
    }
    let platform = symbols.semantic_platform();
    let Some(facts) = platform.klib_dependency_body_facts() else {
        return Vec::new();
    };
    let Some(snapshot) = facts.snapshot() else {
        return Vec::new();
    };
    if !support_state.accepts_snapshot(snapshot) {
        diags.error(
            Span::new(0, 0),
            "internal error: one target emission session cannot combine different KLIB provider snapshots",
        );
        return Vec::new();
    }
    let bodies = facts.bodies();
    let intrinsics = facts.intrinsics();
    let selected = source.callables.klib_body_callables();
    let callees = KlibCalleeFacts::new(selected.iter().copied());
    let mut roots: Vec<crate::libraries::KlibBodyCallable<'_>> = Vec::new();
    for callable in selected.iter().copied().filter(|callable| {
        !support_state.emitted.contains_key(callable.identity())
            && platform.is_platform_stdlib_dependency(callable.identity())
            && DependencyBodyUnit::is_scalar_root_candidate(*callable, bodies)
            && bodies.is_supported_intrinsic_root(callable.identity(), intrinsics)
    }) {
        if !roots
            .iter()
            .any(|root| root.identity() == callable.identity())
        {
            roots.push(callable);
        }
    }
    let ordinary = match emitted_roots::ordinary_roots(source, bodies, intrinsics) {
        Ok(roots) => roots,
        Err(error) => {
            diags.error(Span::new(0, 0), format!("internal error: {error}"));
            return Vec::new();
        }
    };
    let mut artifacts = Vec::new();
    for (requires_intrinsic_link, mut roots) in [(true, roots), (false, ordinary)] {
        // Intrinsic support may have emitted an ordinary root transitively in the preceding unit.
        roots.retain(|root| !support_state.emitted.contains_key(root.identity()));
        if roots.is_empty() {
            continue;
        }
        let Some(mut externals) = rebound_externals(platform, &support_state.emitted, diags) else {
            return Vec::new();
        };
        let mut unit = DependencyBodyUnit::default();
        let capabilities = DependencyLeafCapabilities {
            unit: &unit,
            callees: &callees,
            target_realizes_bodyless: backend.realizes_bodyless_dependency_support(),
        };
        let declarations = match emitted_roots::declarations(
            platform,
            bodies,
            intrinsics,
            &roots,
            &externals,
            &capabilities,
            !requires_intrinsic_link,
        ) {
            Ok(declarations) => declarations,
            Err(error) => {
                diags.error(
                    Span::new(0, 0),
                    format!("cannot close a selected ordinary KLIB body: {error}"),
                );
                return Vec::new();
            }
        };
        let callees = KlibCalleeFacts::with_declarations(
            selected.iter().copied(),
            declarations
                .iter()
                .filter_map(|declaration| declaration.klib_body_callable()),
        );
        let capabilities = DependencyLeafCapabilities {
            unit: &unit,
            callees: &callees,
            target_realizes_bodyless: backend.realizes_bodyless_dependency_support(),
        };
        if !prepare_bodyless_externals(
            platform,
            bodies,
            intrinsics,
            &roots,
            &capabilities,
            &mut externals,
            diags,
        ) {
            return Vec::new();
        }
        for callable in roots {
            if let Err(decline) = unit.lower_root_with_intrinsics_and_externals(
                callable, bodies, &callees, intrinsics, &externals,
            ) {
                if requires_intrinsic_link && !decline.is_internal_inconsistency() {
                    // A wrapper whose body uses a form lowering does not model yet is not support: the
                    // declined body left the unit as it was, and the backend declines its call sites.
                    crate::trace_compiler!("lower", "intrinsic support root declined: {decline}");
                    continue;
                }
                diags.error(
                    Span::new(0, 0),
                    format!("cannot lower a selected KLIB dependency body: {decline}"),
                );
                return Vec::new();
            }
        }
        if unit.callable_functions().next().is_none() {
            continue;
        }
        if requires_intrinsic_link && !links_emitted_support(unit.ir(), &externals) {
            diags.error(
            Span::new(0, 0),
            "internal error: authenticated KLIB intrinsic roots lowered without an exact intrinsic or emitted-support edge",
        );
            return Vec::new();
        }
        if let Err(error) = unit.ir().validate_determined_types() {
            diags.error(
                Span::new(0, 0),
                format!("internal error: dependency bodies have an undetermined type: {error:?}"),
            );
            return Vec::new();
        }
        if let Err(error) = unit.ir().validate_semantic_contracts() {
            diags.error(
                Span::new(0, 0),
                format!("internal error: dependency bodies violate common IR contracts: {error:?}"),
            );
            return Vec::new();
        }

        let mut exports = DependencyExportSignatures::default();
        let closed = unit
            .callable_functions()
            // A reachable bodyless declaration remains an exact provider edge which the target may
            // realize from its frozen callable fact. It is not emitted code and therefore cannot be
            // published as a dependency export: an export is a trampoline to a body in this unit.
            .filter(|(_, function)| {
                crate::backend::has_standalone_dependency_code(unit.ir(), *function)
            })
            .map(|(identity, function)| (identity.clone(), function))
            .collect::<Vec<_>>();
        for (identity, function) in &closed {
            exports.record_callable(*function, identity.clone());
            if let Some(classifier) = unit.authenticated_singleton_classifier(identity) {
                let inert_platform_singleton =
                    bodies.singleton_initialization_is_inert(&identity.module, classifier);
                exports.record_singleton_member_receiver(
                    *function,
                    classifier,
                    !inert_platform_singleton,
                );
            }
            if let Some(&receiver) = unit.ir().singleton_extension_unread_receivers.get(function) {
                let inert_platform_singleton = match receiver.kind() {
                    TyKind::Obj(classifier, arguments) if arguments.is_empty() => {
                        bodies.singleton_initialization_is_inert(&identity.module, classifier)
                    }
                    _ => false,
                };
                exports.record_singleton_extension_unread_receiver(
                    *function,
                    receiver,
                    !inert_platform_singleton,
                );
            }
        }
        let mut published = Vec::with_capacity(closed.len());
        for (identity, _) in &closed {
            let Some((_external, realization)) = platform.external_callable_by_dependency(identity)
            else {
                diags.error(
                Span::new(0, 0),
                "internal error: a lowered KLIB dependency body has no exact provider realization",
            );
                return Vec::new();
            };
            if realization.dependency_callable.as_ref() != Some(identity) {
                diags.error(
                    Span::new(0, 0),
                    "internal error: a KLIB dependency body realization changed its exact identity",
                );
                return Vec::new();
            }
            // The provider's stored realization is the undecorated declaration. Retain the singleton
            // receiver of the exact checked edge variant which authenticated the serialized header;
            // input order cannot decide the receiver contract published to later support units.
            let singleton_dispatch = match (
                unit.authenticated_singleton_dispatch(identity),
                unit.authenticated_singleton_classifier(identity),
            ) {
                (Some(ty), Some(classifier)) => Some(PublishedSingletonDispatch { ty, classifier }),
                (None, None) => None,
                _ => {
                    diags.error(
                    Span::new(0, 0),
                    "internal error: an authenticated singleton KLIB body lost its exact classifier",
                );
                    return Vec::new();
                }
            };
            published.push((identity.clone(), singleton_dispatch));
        }
        let callables = match CheckedBackendCallables::freeze(unit.ir(), platform) {
            Ok(callables) => callables,
            Err(error) => {
                diags.error(
                    Span::new(0, 0),
                    format!(
                    "internal error: cannot freeze an already-emitted KLIB support call: {error:?}"
                ),
                );
                return Vec::new();
            }
        };
        let ordinal = support_state.emitted.len();
        let support_module = format!(
            "{}$klib_body${}${ordinal}",
            source.module_name,
            source.source.raw()
        );
        let support = CheckedIrFile {
            ir: unit.into_ir(),
            kind: CheckedIrUnitKind::DependencySupport,
            source: source.source,
            classifiers: CheckedBackendClassifiers::new(module_facts, platform),
            callables,
            dependency_exports: exports,
            native_plugins: symbols.native_plugins(),
            module_name: &support_module,
            stems: source.stems,
            origins: source.origins,
        };
        let outputs = backend.lower_ir_file(support, state, diags);
        if diags.has_errors() {
            return Vec::new();
        }
        support_state.emitted.extend(published);
        artifacts.extend(outputs);
    }
    artifacts
}

fn rebound_externals(
    platform: &dyn crate::libraries::SemanticPlatform,
    emitted: &HashMap<KlibDependencyCallable, Option<PublishedSingletonDispatch>>,
    diags: &mut DiagSink,
) -> Option<HashMap<KlibDependencyCallable, EmittedDependencyCallable>> {
    let mut rebound = HashMap::with_capacity_and_hasher(emitted.len(), Default::default());
    for (identity, singleton_dispatch) in emitted {
        let Some((external, realization)) = platform.external_callable_by_dependency(identity)
        else {
            diags.error(
                Span::new(0, 0),
                "internal error: an emitted KLIB support identity is absent from the current module provider",
            );
            return None;
        };
        if realization.dependency_callable.as_ref() != Some(identity) {
            diags.error(
                Span::new(0, 0),
                "internal error: the current module provider rebound a different KLIB support identity",
            );
            return None;
        }
        rebound.insert(
            identity.clone(),
            EmittedDependencyCallable::new(
                external,
                singleton_dispatch.map(|dispatch| dispatch.ty),
                singleton_dispatch.map(|dispatch| dispatch.classifier),
            ),
        );
    }
    Some(rebound)
}

/// Common-operation and target-external capabilities of the same dependency emission unit.
struct DependencyLeafCapabilities<'a> {
    unit: &'a DependencyBodyUnit,
    callees: &'a KlibCalleeFacts<'a>,
    target_realizes_bodyless: bool,
}

impl DependencyLeafCapabilities<'_> {
    fn folds_common_builtin(&self, identity: &KlibDependencyCallable) -> bool {
        self.unit.folds_common_builtin(identity, self.callees)
    }
}

/// Prepare exact provider handles only for declaration edges actual lowering will retain.
/// A missing provider realization is an inconsistency, never permission to search or fold.
fn prepare_bodyless_externals(
    platform: &dyn crate::libraries::SemanticPlatform,
    bodies: &crate::klib_libraries::KlibDeclarationBodies,
    intrinsics: &crate::klib_libraries::KlibIntrinsicFacts,
    roots: &[crate::libraries::KlibBodyCallable<'_>],
    capabilities: &DependencyLeafCapabilities<'_>,
    externals: &mut HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
    diags: &mut DiagSink,
) -> bool {
    let mut pending = roots
        .iter()
        .map(|root| root.identity().clone())
        .collect::<VecDeque<_>>();
    let mut visited = HashSet::default();
    while let Some(identity) = pending.pop_front() {
        if !visited.insert(identity.clone()) {
            continue;
        }
        for callee in bodies.direct_callees(&identity) {
            if externals.contains_key(&callee) {
                continue;
            }
            if capabilities.folds_common_builtin(&callee) {
                continue;
            }
            match capabilities.callees.common_primitive(&callee, bodies) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(reason) => {
                    diags.error(
                        Span::new(0, 0),
                        format!("invalid common KLIB primitive: {reason:?}"),
                    );
                    return false;
                }
            }
            if intrinsics.get(&callee).is_some() {
                // A target intrinsic is called through its provider handle like a bodyless
                // declaration; the target's emitter realizes that call.
                let Some(external) = provider_external(platform, &callee, diags) else {
                    return false;
                };
                externals.insert(callee, external);
                continue;
            }
            let Some(has_body) = bodies.function_has_body(&callee) else {
                continue;
            };
            if has_body {
                pending.push_back(callee);
                continue;
            }
            if !capabilities.target_realizes_bodyless {
                diags.error(
                    Span::new(0, 0),
                    "internal error: the target cannot realize a bodyless KLIB support dependency",
                );
                return false;
            }
            let Some(external) = provider_external(platform, &callee, diags) else {
                return false;
            };
            externals.insert(callee, external);
        }
    }
    true
}

/// The provider's opaque handle for a reachable declaration support code calls without a body of
/// its own. A missing or rebound realization is an inconsistency, never a reason to search.
fn provider_external(
    platform: &dyn crate::libraries::SemanticPlatform,
    callee: &KlibDependencyCallable,
    diags: &mut DiagSink,
) -> Option<EmittedDependencyCallable> {
    let Some((external, realization)) = platform.external_callable_by_dependency(callee) else {
        crate::trace_compiler!("lower", "missing exact bodyless KLIB provider: {callee:?}");
        diags.error(
            Span::new(0, 0),
            "internal error: a bodyless KLIB dependency has no exact provider realization",
        );
        return None;
    };
    if realization.dependency_callable.as_ref() != Some(callee) {
        diags.error(
            Span::new(0, 0),
            "internal error: a bodyless KLIB dependency realization changed its exact identity",
        );
        return None;
    }
    Some(EmittedDependencyCallable::new(external, None, None))
}

fn links_emitted_support(
    ir: &crate::ir::IrFile,
    emitted: &HashMap<KlibDependencyCallable, EmittedDependencyCallable>,
) -> bool {
    ir.exprs.iter().any(|expression| {
        let IrExpr::Call {
            callee: call_callee,
            ..
        } = expression
        else {
            return false;
        };
        let Callee::External { target, .. } = &**call_callee else {
            return false;
        };
        emitted.values().any(|emitted| emitted.target() == *target)
    })
}

#[cfg(test)]
mod tests {
    use super::DependencySupportState;
    use crate::libraries::{
        DependencyBodyProviderRole, DependencyBodyProviderSnapshot, DependencyBodySnapshot,
    };

    fn provider(
        archive: &str,
        module: &str,
        compiler_version: &str,
        role: DependencyBodyProviderRole,
    ) -> DependencyBodyProviderSnapshot {
        DependencyBodyProviderSnapshot::new(
            archive.into(),
            module,
            Some("2.4.0"),
            Some(compiler_version),
            Some("2.4.0"),
            [compiler_version.as_bytes()[0]; 32],
            role,
        )
    }

    #[test]
    fn one_target_session_rejects_a_different_klib_provider_snapshot() {
        let first = DependencyBodySnapshot::new(vec![provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.10",
            DependencyBodyProviderRole::PlatformWasm,
        )]);
        let same_inputs_another_compilation = first.clone();
        let independently_opened_inputs = DependencyBodySnapshot::new(vec![provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.20",
            DependencyBodyProviderRole::PlatformWasm,
        )]);
        let mut state = DependencySupportState::default();

        assert!(state.accepts_snapshot(&first));
        assert!(state.accepts_snapshot(&same_inputs_another_compilation));
        assert!(!state.accepts_snapshot(&independently_opened_inputs));
        assert!(state.accepts_snapshot(&first));
    }

    #[test]
    fn a_target_session_binds_every_transitive_body_provider_independent_of_input_order() {
        let stdlib = provider(
            "/toolchain/stdlib",
            "stdlib",
            "2.4.20",
            DependencyBodyProviderRole::PlatformWasm,
        );
        let dependency = provider(
            "/toolchain/dependency",
            "dependency",
            "2.4.20",
            DependencyBodyProviderRole::Dependency,
        );
        let first = DependencyBodySnapshot::new(vec![stdlib.clone(), dependency.clone()]);
        let reverse_input_order = DependencyBodySnapshot::new(vec![dependency, stdlib.clone()]);
        let changed_dependency = DependencyBodySnapshot::new(vec![
            stdlib,
            provider(
                "/toolchain/dependency",
                "dependency",
                "2.4.21",
                DependencyBodyProviderRole::Dependency,
            ),
        ]);
        let mut state = DependencySupportState::default();

        assert!(state.accepts_snapshot(&first));
        assert!(state.accepts_snapshot(&reverse_input_order));
        assert!(!state.accepts_snapshot(&changed_dependency));
    }
}
