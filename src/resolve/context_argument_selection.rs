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
    arg_assignable_simple, selected_context_values, CheckerScope, ContextBinding,
    ImplicitReceiverSelection, Local, MissingContextParameter, ReceiverDeclarationRole,
    ReceiverFnValueOrigin, ResolvedContextArgument, SelectedContextSources,
};
use crate::ast::ExprId;
use crate::fx_hash::HashSet;
use crate::types::Ty;

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
            // `if (this is Scope)` changes the semantic type of the current receiver without
            // changing its runtime identity. The scope tower deliberately stores the declared
            // receiver type, so consult the checker's flow-aware receiver view before the raw
            // context-value walk. Multiple equal context parameters may select this same value.
            if let Some(actual) = self
                .actual_this_narrow(scope)
                .filter(|actual| matches(*actual, want))
            {
                let receiver = self
                    .implicit_receivers(scope)
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
                    let context_binding = context_name.map(|name| ContextBinding {
                        flow_identity: scope
                            .shadowed_binding(Ns::Value, &name, context_shadow_depth)
                            .and_then(|binding| Some(binding.value()?.flow_identity)),
                        name,
                        shadow_depth: context_shadow_depth,
                    });
                    let argument =
                        ResolvedContextArgument::ImplicitReceiver(ImplicitReceiverSelection {
                            ty,
                            current,
                            receiver_depth,
                            classifier: None,
                            context_binding,
                            singleton: self.implicit_singleton_value(ty, current),
                            context_parameter: self
                                .implicit_receivers(scope)
                                .into_iter()
                                .find(|receiver| receiver.receiver_depth == receiver_depth)
                                .and_then(|receiver| receiver.receiver_role)
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
                ContextValue::Binding { name, shadow_depth } => {
                    let ty = self
                        .lookup(scope, &name)
                        .ok_or(MissingContextParameter { index, ty: want })?
                        .ty;
                    let argument = if name == "this" {
                        let receiver = self
                            .implicit_receivers(scope)
                            .into_iter()
                            .find(|receiver| receiver.current)
                            .ok_or(MissingContextParameter { index, ty: want })?;
                        ResolvedContextArgument::ImplicitReceiver(ImplicitReceiverSelection {
                            // Keep the smart-cast result selected from the lexical shadow. Runtime
                            // identity still comes from the receiver rung, not a nonexistent local
                            // slot named `this`.
                            ty,
                            current: receiver.current,
                            receiver_depth: receiver.receiver_depth,
                            classifier: None,
                            context_binding: Some(ContextBinding {
                                name,
                                shadow_depth,
                                flow_identity: selected_identity,
                            }),
                            singleton: self.implicit_singleton_value(ty, receiver.current),
                            // The binding was selected as a context parameter.
                            context_parameter: true,
                            construction_sites: self
                                .implicit_receiver_selection(receiver)
                                .construction_sites,
                        })
                    } else {
                        ResolvedContextArgument::Binding {
                            name,
                            shadow_depth,
                            flow_identity: selected_identity,
                        }
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
                let local = binding_by_flow_identity(scope, &name, identity)?;
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

/// The value binding `name` in `scope` whose flow identity is `identity`.
fn binding_by_flow_identity(scope: &CheckerScope<'_>, name: &str, identity: u32) -> Option<Local> {
    scope.ancestors().find_map(|rung| {
        let mut found = None;
        rung.own_bindings(Ns::Value, |bound, binding| {
            if found.is_none() && bound == name {
                found = binding
                    .value()
                    .filter(|local| local.flow_identity == identity);
            }
        });
        found
    })
}
