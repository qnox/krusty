//! Selection of the scope value that fills each implicit context parameter of a call.
//!
//! The implicit-receiver tower is searched nearest-first, then the lexical bindings. A selected
//! binding is recorded with its flow identity, so a later phase maps the value by identity and
//! never resolves its spelling or shadow depth again.
//!
//! A lifted local function captures the bindings its body passes this way. Each is retrieved by
//! the identity its selection recorded, so a nearer same-named value never stands in for it.

use super::scope::{ContextValue, Ns};
use super::{
    arg_assignable_simple, selected_context_values, CheckerScope, ImplicitReceiverSelection, Local,
    MissingContextParameter, ReceiverDeclarationRole, ReceiverFnValueOrigin,
    SelectedContextSources,
};
use crate::ast::ExprId;
use crate::fx_hash::HashSet;
use crate::types::Ty;

use crate::context_parameters::ContextBindingSource;

/// The lexical binding a named context receiver is also visible as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBinding {
    pub name: String,
    pub source: ContextBindingSource,
    /// Flow identity recorded at selection; synthetic checked-FIR reads carry none.
    pub flow_identity: Option<u32>,
}

/// Runtime identity of a value selected for an implicit context parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedContextArgument {
    Binding {
        name: String,
        source: ContextBindingSource,
        flow_identity: Option<u32>,
    },
    ImplicitReceiver(ImplicitReceiverSelection),
}

impl super::Checker<'_> {
    /// Resolve each context-parameter type to one semantic scope value. The implicit-receiver tower is
    /// searched nearest-first, followed by lexical bindings. The returned identities are the complete
    /// checker/lowerer handoff; lowering does not repeat resolution.
    pub(super) fn select_context_arguments(
        &self,
        scope: &CheckerScope<'_>,
        ctx_types: &[Ty],
    ) -> Option<Vec<ResolvedContextArgument>> {
        self.select_context_arguments_with_types(scope, ctx_types)
            .ok()
            .map(|arguments| {
                arguments
                    .into_iter()
                    .map(|(argument, _)| argument)
                    .collect()
            })
    }

    pub(super) fn select_context_arguments_with_types(
        &self,
        scope: &CheckerScope<'_>,
        ctx_types: &[Ty],
    ) -> Result<Vec<(ResolvedContextArgument, Ty)>, MissingContextParameter> {
        let matches = |have: Ty, want: Ty| -> bool {
            if have == want {
                return true;
            }
            if let Some(bound) = want.ty_param_bound() {
                return bound.is_erased_top() || arg_assignable_simple(bound, have);
            }
            match (have.obj_internal(), want.obj_internal()) {
                (Some(h), Some(w)) => self.obj_name_is_subtype(h, w),
                _ => want.mentions_ty_param() && have.erased_recv() == want.erased_recv(),
            }
        };
        let mut out = Vec::with_capacity(ctx_types.len());
        for (index, &want) in ctx_types.iter().enumerate() {
            // A narrowed receiver keeps its actual receiver-path identity. A lexical context
            // parameter with an escaped `this` name must not become that receiver's flow proof.
            let narrowed_receiver = self
                .this_narrow
                .or_else(|| self.lookup_path_narrowing(scope, &self.current_receiver_path(scope)?));
            if let Some(actual) = narrowed_receiver.filter(|actual| matches(*actual, want)) {
                let receiver = self
                    .declared_implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.current)
                    .ok_or(MissingContextParameter { index, ty: want })?;
                let selected =
                    ResolvedContextArgument::ImplicitReceiver(ImplicitReceiverSelection {
                        ty: actual,
                        current: receiver.current,
                        receiver_depth: receiver.receiver_depth,
                        classifier: None,
                        context_binding: None,
                        singleton: self.implicit_singleton_value(actual, receiver.current),
                        context_parameter: receiver
                            .receiver_role
                            .is_some_and(ReceiverDeclarationRole::is_context_parameter),
                        construction_sites: self
                            .implicit_receiver_selection(receiver)
                            .construction_sites,
                    });
                crate::trace_compiler!(
                    "resolve",
                    "context argument want={want:?} selected={selected:?}"
                );
                out.push((selected, actual));
                continue;
            }
            // The selected binding's flow identity is recorded with the selection, so a later
            // phase maps the value by identity instead of resolving its coordinate again.
            let mut selected_identity = None;
            let selected = scope.find_context_value(
                |receiver| matches(receiver, want),
                |binding| {
                    let local = binding.value().filter(|local| {
                        matches!(local.origin, ReceiverFnValueOrigin::Local)
                            && local.is_context_parameter
                            && matches(local.ty, want)
                    });
                    selected_identity = local.map(|local| local.flow_identity);
                    local.is_some()
                },
            );
            let Some(selected) = selected else {
                crate::trace_compiler!(
                    "resolve",
                    "context argument missing want={want:?} receivers={:?}",
                    self.implicit_receivers(scope)
                        .into_iter()
                        .map(|receiver| receiver.ty)
                        .collect::<Vec<_>>(),
                );
                return Err(MissingContextParameter { index, ty: want });
            };
            let (selected, actual) = match selected {
                ContextValue::ImplicitReceiver {
                    ty,
                    current,
                    receiver_depth,
                    context_name,
                    context_shadow_depth,
                } => {
                    // A named context parameter has a lexical alias, but its runtime identity is
                    // still the selected implicit-receiver rung. Preserve both facts: FIR reads the
                    // receiver coordinate and uses `context_binding` only when a nested body must
                    // capture the named alias. Reclassifying it as an ordinary local loses the
                    // receiver when Pass 2 reconstructs a lambda scope.
                    // The alias is the binding at the coordinate just selected in this scope.
                    let receiver_role = self
                        .implicit_receivers(scope)
                        .into_iter()
                        .find(|receiver| receiver.receiver_depth == receiver_depth)
                        .ok_or(MissingContextParameter { index, ty: want })?
                        .receiver_role;
                    let context_binding = context_receiver_binding(
                        scope,
                        context_name,
                        context_shadow_depth,
                        receiver_role,
                    )
                    .map_err(|()| MissingContextParameter { index, ty: want })?;
                    let argument =
                        ResolvedContextArgument::ImplicitReceiver(ImplicitReceiverSelection {
                            ty,
                            current,
                            receiver_depth,
                            classifier: None,
                            context_binding,
                            singleton: self.implicit_singleton_value(ty, current),
                            context_parameter: receiver_role
                                .is_some_and(ReceiverDeclarationRole::is_context_parameter),
                            construction_sites:
                                super::constructor_receiver_frames::scoped_construction_sites(
                                    &self.constructor_receiver_frames,
                                    scope,
                                    receiver_depth,
                                ),
                        });
                    (argument, ty)
                }
                ContextValue::Binding { name } => {
                    let identity =
                        selected_identity.ok_or(MissingContextParameter { index, ty: want })?;
                    let (local, source) = selected_context_binding(scope, &name, identity)
                        .ok_or(MissingContextParameter { index, ty: want })?;
                    let ty = local.ty;
                    // A selected lexical binding keeps its own certified storage, including an
                    // escaped source name such as `this`. Receiver aliases enter through the
                    // implicit-receiver arm and its declaration-role authority instead.
                    let argument = ResolvedContextArgument::Binding {
                        name,
                        source,
                        flow_identity: selected_identity,
                    };
                    (argument, ty)
                }
            };
            crate::trace_compiler!(
                "resolve",
                "context argument want={want:?} selected={selected:?}"
            );
            out.push((selected, actual));
        }
        Ok(out)
    }

    /// Each binding a local function's `body` passes as an implicit context argument or receiver,
    /// with its name: the exact binding the checker selected, found by its recorded flow identity.
    pub(super) fn selected_context_bindings(
        &self,
        scope: &CheckerScope<'_>,
        body: ExprId,
    ) -> Vec<(String, Local)> {
        let sources = SelectedContextSources {
            expr_types: &self.expr_types,
            implicit_receiver_selections: &self.implicit_receiver_selections,
            context_args: &self.context_args,
            resolved_calls: &self.resolved_calls,
            resolved_super_calls: &self.resolved_super_calls,
            stmt_lowers: &self.stmt_lowers,
        };
        let (selected, direct_receivers) = selected_context_values(self.file, sources, body, true);
        let receiver_binding = |selection: ImplicitReceiverSelection| {
            let binding = selection.context_binding?;
            Some((binding.name, binding.flow_identity?))
        };
        selected
            .into_iter()
            .filter_map(|source| match source {
                ResolvedContextArgument::Binding {
                    name,
                    flow_identity,
                    ..
                } => Some((name, flow_identity?)),
                ResolvedContextArgument::ImplicitReceiver(selection) => receiver_binding(selection),
            })
            .chain(direct_receivers.into_iter().filter_map(receiver_binding))
            .filter_map(|(name, identity)| {
                let (local, _) = selected_context_binding(scope, &name, identity)?;
                Some((name, local))
            })
            .collect()
    }

    /// A local function's captures in name order, each with its exact binding and how many nearer
    /// same-named bindings shadow it in `scope`, where the function is declared. A value `captured`
    /// names is the binding that name resolves to there. A value the body passes as a context
    /// argument (`selected`) is the binding the checker selected, and is a capture of its own unless
    /// a captured name already resolves to that same binding: a context value shadowed by a nearer
    /// same-named local is a second capture, not a duplicate of the local.
    pub(super) fn captured_local_bindings(
        &self,
        scope: &CheckerScope<'_>,
        captured: Vec<(String, Ty, bool)>,
        selected: Vec<(String, Local)>,
        capturable: &HashSet<String>,
    ) -> CapturedLocals {
        let mut bound = captured
            .into_iter()
            .map(|capture| {
                let local = self.lookup(scope, &capture.0);
                (capture, local)
            })
            .collect::<Vec<_>>();
        for (name, local) in selected {
            let already_captured = bound.iter().any(|(_, captured)| {
                captured.is_some_and(|captured| captured.flow_identity == local.flow_identity)
            });
            if capturable.contains(&name) && !already_captured {
                bound.push(((name, local.ty, false), Some(local)));
            }
        }
        bound.sort_by(|left, right| left.0 .0.cmp(&right.0 .0));
        let mut out = CapturedLocals::default();
        for (capture, local) in bound {
            let shadow_depth = local
                .and_then(|local| local.lexical_capture_identity)
                .and_then(|identity| self.lexical_binding_by_identity(scope, &capture.0, identity))
                .map_or(0, |(_, depth)| depth);
            out.captured.push(capture);
            out.locals.push(local);
            out.shadow_depths.push(shadow_depth);
        }
        out
    }
}

/// A lifted local function's captures in parameter order: each capture's name, type and
/// shared-cell hint, its exact binding, and its shadow depth where the function is declared.
#[derive(Default)]
pub(super) struct CapturedLocals {
    pub(super) captured: Vec<(String, Ty, bool)>,
    pub(super) locals: Vec<Option<Local>>,
    pub(super) shadow_depths: Vec<u32>,
}

/// A receiver declaration role certifies nonlexical receiver storage. Any other named alias
/// requires its actual selected lexical binding; a missing certificate is not a receiver fallback.
fn context_receiver_binding(
    scope: &CheckerScope<'_>,
    name: Option<String>,
    shadow_depth: usize,
    receiver_role: Option<ReceiverDeclarationRole>,
) -> Result<Option<ContextBinding>, ()> {
    let Some(name) = name else {
        return Ok(None);
    };
    if matches!(
        receiver_role,
        Some(ReceiverDeclarationRole::Lambda { .. } | ReceiverDeclarationRole::Extension(_))
    ) {
        return Ok(None);
    }
    let local = scope
        .shadowed_binding(Ns::Value, &name, shadow_depth)
        .and_then(|binding| binding.value())
        .ok_or(())?;
    let (local, source) = selected_context_binding(scope, &name, local.flow_identity).ok_or(())?;
    Ok(Some(ContextBinding {
        name,
        source,
        flow_identity: Some(local.flow_identity),
    }))
}

/// Find the actual selected value, recording whether its lexical source crosses a class
/// boundary. The class certificate is positive scope evidence, not a failed capture lookup.
fn selected_context_binding(
    scope: &CheckerScope<'_>,
    name: &str,
    identity: u32,
) -> Option<(Local, ContextBindingSource)> {
    let mut crossed_class = false;
    let mut seen = HashSet::default();
    let mut shadow_depth = 0;
    for rung in scope.ancestors() {
        let mut bindings = Vec::new();
        rung.own_bindings(Ns::Value, |candidate_name, binding| {
            if candidate_name == name {
                if let Some(local) = binding.value() {
                    bindings.push(local);
                }
            }
        });
        for local in bindings.into_iter().rev() {
            if !seen.insert(local.flow_identity) {
                continue;
            }
            if local.flow_identity == identity {
                let source = if crossed_class
                    || matches!(local.origin, ReceiverFnValueOrigin::ClassStorage(_))
                {
                    ContextBindingSource::RequiredClassCapture {
                        lexical_binding: local.lexical_capture_identity?,
                    }
                } else {
                    ContextBindingSource::ProvenLocalDepth {
                        shadow_depth,
                        lexical_binding: local.lexical_capture_identity,
                    }
                };
                return Some((local, source));
            }
            shadow_depth += 1;
        }
        crossed_class |= matches!(rung.kind(), super::scope::ScopeKind::Class { .. });
    }
    None
}

#[cfg(test)]
mod tests;
