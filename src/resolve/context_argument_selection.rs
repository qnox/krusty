//! Selection of the scope value that fills each implicit context parameter of a call.
//!
//! The implicit-receiver tower is searched nearest-first, then the lexical bindings. A selected
//! binding is recorded with its flow identity, so a later phase maps the value by identity and
//! never resolves its spelling or shadow depth again.

use super::scope::{ContextValue, Ns};
use super::{
    arg_assignable_simple, CheckerScope, ContextBinding, ImplicitReceiverSelection,
    MissingContextParameter, ReceiverFnValueOrigin, ResolvedContextArgument,
};
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
}
