//! Checking a call against the member extensions of the implicit receivers: selecting one, recording
//! its type arguments and dispatch receiver, and reporting the level's rejections only when no
//! later tower level can answer the call.

use super::member_extension_selection::{
    MemberExtensionFunctionSelection, MemberExtensionSearch, MemberExtensionSelection,
    RejectedMemberExtensions,
};
use super::*;

impl Checker<'_> {
    pub(super) fn check_member_extension_function_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        extension_receiver: Ty,
        name: &str,
        inference_bindings: Option<&crate::symbol_resolver::GSigBinds>,
    ) -> Option<Ty> {
        self.check_member_extension_function_call_mode(
            scope,
            call_args,
            extension_receiver,
            name,
            MemberExtensionSearch {
                kinds: MemberExtensionSelection::All,
                receivers: None,
                rejections: RejectedMemberExtensions::YieldToApplicableExtension,
                result: CallResultConstraint::direct(None),
                inference_bindings,
            },
        )
    }

    /// Select a zero-value-argument member extension for compact signature evaluation.
    ///
    /// The compact graph has semantic argument types but deliberately no parser `ExprId`s. A
    /// zero-argument call needs none, so it can use the complete ordinary member-extension
    /// candidate collector, context binding, priority, and overload selector without retaining or
    /// fabricating syntax identities.
    pub(super) fn check_member_extension_function_call_mode(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        extension_receiver: Ty,
        name: &str,
        search: MemberExtensionSearch<'_>,
    ) -> Option<Ty> {
        let MemberExtensionSearch {
            kinds: selection,
            receivers,
            rejections: rejected,
            result: result_constraint,
            inference_bindings,
        } = search;
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        let explicit_type_args = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.clone())
            .unwrap_or_default()
            .iter()
            .map(|argument| self.type_ref_ty(scope, argument))
            .collect::<Vec<_>>();
        let request = MemberExtensionFunctionCall {
            extension_receiver,
            result_constraint,
            name,
            args,
            arg_tys,
            arg_names: arg_names.as_deref(),
            explicit_type_args: &explicit_type_args,
            trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
            inference_bindings,
        };
        // `recv(args)` is the `invoke` convention and therefore admits only `operator`
        // declarations. Keep that language rule at the invoke boundary instead of storing a mode
        // in the general member-extension request: explicit `recv.invoke(args)` and every other
        // member-extension consumer must continue to see ordinary declarations.
        let selected = match receivers {
            Some(receivers) => {
                self.member_extension_function_with_receivers(scope, receivers, request, selection)
            }
            None => self.member_extension_function(scope, request, selection),
        };
        match selected {
            MemberExtensionFunctionSelection::Selected(candidate) => {
                let mut candidate = *candidate;
                for &(parameter, source) in &candidate.argument_parameters {
                    let (Some(&expected), Some(&actual)) =
                        (candidate.visible_params.get(parameter), arg_tys.get(source))
                    else {
                        continue;
                    };
                    if let Some(coerced) =
                        self.unit_coerced_lambda_type(args[source], expected, actual)
                    {
                        self.set(args[source], coerced);
                    }
                }
                if candidate.visibility != Visibility::Public {
                    self.reject_function_if_inaccessible(
                        &candidate.diagnostic_candidate,
                        candidate.owner,
                        self.call_callee_name_span(call),
                    );
                }
                let mut validation_params = candidate.visible_params.clone();
                if let Some(vararg) = candidate.call_sig.vararg_index {
                    if let Some(element) = validation_params.get_mut(vararg) {
                        *element = Ty::array(*element);
                    }
                }
                if !self.expect_selected_call_args(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys,
                    },
                    &validation_params,
                    &validation_params,
                    &candidate.call_sig,
                    None,
                ) {
                    return Some(Ty::Error);
                }
                // Dispatch opcode is a fact of the selected OWNER, not of where its declaration was
                // loaded from. Query the same federated type shape used during selection so a member
                // extension inherited from a dependency interface cannot silently become invokevirtual.
                let interface = self
                    .resolver()
                    .classifier(candidate.owner)
                    .is_some_and(|shape| shape.is_interface());
                let dispatch_receiver =
                    self.implicit_receiver_selection(candidate.dispatch_receiver);
                let result = self.commit_member_extension_type_arguments(
                    call,
                    &explicit_type_args,
                    &mut candidate,
                );
                self.narrow_member_extension_receiver(call, &candidate);
                self.resolved_calls
                    .insert(call, candidate.resolved_call(dispatch_receiver, interface));
                self.mark_extension_receiver_used(call, candidate.dispatch_receiver);
                Some(result)
            }
            MemberExtensionFunctionSelection::None(rejections)
                if rejected == RejectedMemberExtensions::YieldToApplicableExtension
                    && !rejections.call_candidates.is_empty()
                    && self
                        .select_extension_rung(
                            scope,
                            call_args,
                            extension_receiver,
                            &explicit_type_args,
                            result_constraint.expected(),
                            &self.stable_receiver_callables(extension_receiver, name),
                        )
                        .selection
                        .is_some() =>
            {
                None
            }
            MemberExtensionFunctionSelection::None(rejections) => self
                .report_single_mapped_candidate_type_errors(
                    scope,
                    call,
                    name,
                    args,
                    arg_tys,
                    CallConstraints {
                        type_args: &explicit_type_args,
                        receiver: extension_receiver,
                        expected: result_constraint.expected(),
                    },
                    &rejections.call_candidates,
                ),
            MemberExtensionFunctionSelection::Ambiguous(_) => {
                self.diags.error(
                    self.call_callee_name_span(call),
                    format!("overload resolution ambiguity for member '{name}'"),
                );
                Some(Ty::Error)
            }
        }
    }
}
