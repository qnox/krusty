//! Invocation of a value whose function type has an extension receiver, `block()` or
//! `receiver.block()`: finding the value, splitting its `context(C…) R.(V…) -> T` shape, supplying
//! the implicit receiver and context arguments, and recording the selected invoke.

use super::*;

/// Result of trying the implicit `value()` spelling of a receiver-function value.
pub(super) enum ImplicitReceiverFunctionInvoke {
    Applicable,
    /// Value-parameter arity does not match. The explicit form `value(receiver, args)` may.
    TryExplicit,
    /// The implicit spelling is this call's shape and it is not applicable. A later callable may
    /// still own the name; the exact receiver/context failure is reported only when nothing else is.
    Inapplicable {
        signature: &'static crate::types::FnSig,
        missing_receiver: bool,
        missing_context: Vec<MissingContextParameter>,
    },
}

/// A receiver-function signature's parameters split at kotlinc's function-type boundaries.
pub(super) struct ReceiverFunctionParts {
    pub(super) context: &'static [Ty],
    pub(super) receiver: Ty,
    pub(super) values: &'static [Ty],
}

impl Checker<'_> {
    pub(super) fn receiver_function_value(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<(&'static crate::types::FnSig, ReceiverFnValueOrigin)> {
        if let Some((semantic, origin)) = self.local_callable_type(scope, name) {
            return match semantic {
                Ty::Fun(signature) if signature.has_receiver => {
                    let origin = match origin {
                        ReceiverFnValueOrigin::DispatchProperty {
                            owner,
                            receiver_identity,
                            declared_ty,
                            enum_entry_property,
                            owner_storage,
                        } => {
                            let receivers = self.implicit_receivers(scope);
                            let recorded_is_owner = receivers.iter().any(|receiver| {
                                receiver.identity == receiver_identity
                                    && self.receiver_is_assignable(receiver.ty, Ty::obj_name(owner))
                            });
                            let receiver_identity = if recorded_is_owner {
                                receiver_identity
                            } else {
                                receivers
                                    .into_iter()
                                    .find(|receiver| {
                                        receiver.extension_receiver.is_none()
                                            && self.receiver_is_assignable(
                                                receiver.ty,
                                                Ty::obj_name(owner),
                                            )
                                    })
                                    .map(|receiver| receiver.identity)
                                    .unwrap_or(receiver_identity)
                            };
                            ReceiverFnValueOrigin::DispatchProperty {
                                owner,
                                receiver_identity,
                                declared_ty,
                                enum_entry_property,
                                owner_storage,
                            }
                        }
                        origin => origin,
                    };
                    Some((signature, origin))
                }
                _ => None,
            };
        }
        for receiver in self.implicit_receivers(scope) {
            let Some(owner) = receiver.ty.obj_internal() else {
                continue;
            };
            if let Some(property) = self
                .scoped_properties(scope, owner)
                .into_iter()
                .rev()
                .find(|property| property.name == name)
            {
                return match property.ty {
                    Ty::Fun(signature) if signature.has_receiver => Some((
                        signature,
                        ReceiverFnValueOrigin::DispatchProperty {
                            owner: property.owner,
                            receiver_identity: receiver.identity,
                            declared_ty: property.ty,
                            enum_entry_property: property.enum_entry_property,
                            owner_storage: false,
                        },
                    )),
                    _ => None,
                };
            }
        }
        match self.select_top_level_property(scope, name) {
            TopLevelPropertySelection::Selected(property) if matches!(property.property.ty, Ty::Fun(signature) if signature.has_receiver) =>
            {
                let Ty::Fun(signature) = property.property.ty else {
                    unreachable!("guard selected a function-typed property")
                };
                Some((signature, ReceiverFnValueOrigin::TopLevelProperty))
            }
            _ => None,
        }
    }

    /// Split a receiver-function signature at its context, receiver and value boundaries. The
    /// function type `context(C…) R.(V…) -> T` lays its parameters out as `[C…, R, V…]`.
    pub(super) fn receiver_function_parts(
        signature: &'static crate::types::FnSig,
    ) -> Option<ReceiverFunctionParts> {
        if !signature.has_receiver {
            return None;
        }
        let (context, rest) = signature.params.split_at_checked(signature.context_count)?;
        let (&receiver, values) = rest.split_first()?;
        Some(ReceiverFunctionParts {
            context,
            receiver,
            values,
        })
    }

    /// Whether the scope supplies every context argument of a receiver-function value invoked with
    /// its context parameters omitted.
    pub(super) fn receiver_function_context_available(
        &self,
        scope: &CheckerScope<'_>,
        parts: &ReceiverFunctionParts,
    ) -> bool {
        parts.context.is_empty()
            || self
                .select_context_arguments(scope, parts.context)
                .is_some()
    }

    pub(super) fn receiver_function_member_call_params(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver_ty: Ty,
        name: &str,
        arguments: &[ExprId],
        partial_argument_types: &[Option<Ty>],
    ) -> Option<Vec<Ty>> {
        let (signature, _) = self.receiver_function_value(scope, name)?;
        let parts = Self::receiver_function_parts(signature)?;
        if !self.receiver_function_value_applicable(
            scope,
            call,
            arguments,
            partial_argument_types,
            receiver_ty,
            signature,
        ) {
            return None;
        }
        Some(parts.values.to_vec())
    }

    /// Read-only applicability of one receiver-function value at its tower level. A fixed function
    /// value has no callable type parameters, defaults, vararg, or source-callable parameter names;
    /// the ordinary candidate scorer still owns mapping and type compatibility, including postponed
    /// lambdas. Only the selected candidate is contextually rechecked and recorded.
    pub(super) fn receiver_function_value_applicable(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        arguments: &[ExprId],
        partial_argument_types: &[Option<Ty>],
        receiver_ty: Ty,
        signature: &'static crate::types::FnSig,
    ) -> bool {
        // A function-type value has neither callable type parameters nor stable source parameter
        // names. Reject those two source forms before feeding its positional shape to the common
        // scorer; another callable at the same tower spelling may still accept the call.
        if self
            .file
            .call_type_args
            .get(&call.0)
            .is_some_and(|arguments| !arguments.is_empty())
            || self
                .file
                .call_arg_names
                .get(&call.0)
                .is_some_and(|names| names.iter().any(Option::is_some))
        {
            return false;
        }
        let Some(parts) = Self::receiver_function_parts(signature) else {
            return false;
        };
        if !self.receiver_is_assignable(receiver_ty, parts.receiver)
            || !self.receiver_function_context_available(scope, &parts)
        {
            return false;
        }
        let call_sig = CallSig::metadata_plain(parts.values.len());
        self.call_candidate_score(
            scope,
            parts.values,
            &call_sig,
            ArgSlots {
                args: arguments,
                partial_arg_tys: partial_argument_types,
                arg_names: self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
            },
        )
        .is_some()
    }

    pub(super) fn receiver_function_implicit_receiver(
        &self,
        scope: &CheckerScope<'_>,
        expected: Ty,
        value_parameter_count: usize,
        actual_argument_count: usize,
    ) -> Option<ImplicitReceiver> {
        if value_parameter_count != actual_argument_count {
            return None;
        }
        self.implicit_receivers(scope)
            .into_iter()
            .find(|actual| self.receiver_is_assignable(actual.ty, expected))
    }

    /// Commit invocation of a value whose function type has an extension receiver. The caller chooses
    /// only whether source syntax supplied that receiver explicitly; argument validation and lowering
    /// use one path for locals, properties, callable-reference locals, and safe calls.
    pub(super) fn record_receiver_function_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        name: &str,
        signature: &'static crate::types::FnSig,
        origin: ReceiverFnValueOrigin,
        explicit_receiver: Option<Ty>,
    ) -> Option<Ty> {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let parts = Self::receiver_function_parts(signature)?;
        let (expected_receiver, params) = (parts.receiver, parts.values);
        if params.len() != arg_tys.len() {
            return None;
        }
        // Context parameters are always supplied implicitly here: the form that passes them
        // explicitly (`f(context, receiver, value)`) is the ordinary function-value invoke.
        let context_args = if parts.context.is_empty() {
            None
        } else {
            Some(self.select_context_arguments(scope, parts.context)?)
        };
        let implicit_receiver = match explicit_receiver {
            Some(actual) => {
                let partial = arg_tys.iter().copied().map(Some).collect::<Vec<_>>();
                if !self.receiver_function_value_applicable(
                    scope, call, args, &partial, actual, signature,
                ) {
                    return None;
                }
                None
            }
            None => Some(self.receiver_function_implicit_receiver(
                scope,
                expected_receiver,
                params.len(),
                arg_tys.len(),
            )?),
        };
        let property_callee = match origin {
            ReceiverFnValueOrigin::TopLevelProperty => {
                let Expr::Call { callee, .. } = self.file.expr(call) else {
                    return None;
                };
                let TopLevelPropertySelection::Selected(access) =
                    self.select_top_level_property(scope, name)
                else {
                    return None;
                };
                self.set(*callee, access.property.ty);
                self.expr_lowers
                    .insert(*callee, ExprLowering::TopLevelPropertyGet(access));
                Some(*callee)
            }
            ReceiverFnValueOrigin::DispatchProperty {
                receiver_identity, ..
            } => {
                let Expr::Call { callee, .. } = self.file.expr(call) else {
                    return None;
                };
                let receiver = self
                    .implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.identity == receiver_identity)?;
                let selection = self
                    .select_property_read(scope, receiver.ty, name)
                    .ok()
                    .flatten()?;
                let property_ty = self.record_property_read(scope, Some(*callee), selection);
                self.set(*callee, property_ty);
                self.mark_implicit_receiver_selection(*callee, receiver);
                Some(*callee)
            }
            ReceiverFnValueOrigin::Local
            | ReceiverFnValueOrigin::ClassStorage(_)
            | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. } => None,
        };
        self.expect_call_args(scope, params, false, args, arg_tys);
        if let Some(context_args) = context_args {
            self.context_args.insert(call, context_args);
        }
        if let Some(receiver) = implicit_receiver {
            self.mark_implicit_receiver_selection(call, receiver);
        }
        self.expr_lowers.insert(
            call,
            ExprLowering::ReceiverFnInvoke {
                name: name.to_string(),
                params: signature.params.clone(),
                context_count: parts.context.len(),
                ret: signature.ret,
                origin,
                property_callee,
                implicit_receiver: implicit_receiver.map(|receiver| receiver.ty),
                suspend: signature.suspend,
            },
        );
        Some(signature.ret)
    }

    /// Decide whether `name()` is an implicit invoke of this receiver-function value.
    ///
    /// Applicability requires the extension receiver and every context argument. A miss is not yet
    /// a diagnostic: kotlinc keeps searching, so an applicable top-level `fun name()` wins over a
    /// local `context(Needed) Receiver.() -> T` whose `Needed` is not in scope. The context gaps
    /// are reported only when this value is the only candidate. Anonymous function-type context
    /// parameters are `p1`, `p2`, … in source order.
    pub(super) fn classify_implicit_receiver_function_invoke(
        &self,
        scope: &CheckerScope<'_>,
        argument_count: usize,
        signature: &'static crate::types::FnSig,
    ) -> ImplicitReceiverFunctionInvoke {
        let Some(parts) = Self::receiver_function_parts(signature) else {
            return ImplicitReceiverFunctionInvoke::TryExplicit;
        };
        if parts.values.len() != argument_count {
            return ImplicitReceiverFunctionInvoke::TryExplicit;
        }
        let missing_receiver = self
            .receiver_function_implicit_receiver(
                scope,
                parts.receiver,
                parts.values.len(),
                argument_count,
            )
            .is_none();
        let missing_context = self.unavailable_context_parameters(scope, parts.context);
        if missing_receiver || !missing_context.is_empty() {
            return ImplicitReceiverFunctionInvoke::Inapplicable {
                signature,
                missing_receiver,
                missing_context,
            };
        }
        ImplicitReceiverFunctionInvoke::Applicable
    }

    pub(super) fn report_function_value_context_gaps(
        &mut self,
        call: ExprId,
        missing: &[MissingContextParameter],
    ) {
        let width = missing
            .iter()
            .map(|gap| gap.index)
            .max()
            .map_or(0, |index| index + 1);
        let names = (0..width)
            .map(|index| format!("p{}", index + 1))
            .collect::<Vec<_>>();
        for &gap in missing {
            self.report_missing_context_parameter(call, gap, &names);
        }
    }

    pub(super) fn report_function_value_invoke_gaps(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        signature: &'static crate::types::FnSig,
        missing_receiver: bool,
        missing_context: &[MissingContextParameter],
    ) {
        if !missing_context.is_empty() {
            self.report_function_value_context_gaps(call, missing_context);
            return;
        }
        if !missing_receiver {
            return;
        }
        let context_count = signature.context_count.min(signature.params.len());
        let params = &signature.params[context_count..];
        let param_names = (0..params.len())
            .map(|index| format!("p{}", index + 1))
            .collect::<Vec<_>>();
        let defaults = vec![false; params.len()];
        self.report_function_arity(
            call,
            DiagnosticFunction {
                name: "invoke",
                params,
                param_names: &param_names,
                param_defaults: &defaults,
                required: params.len(),
                vararg: false,
                context_count: 0,
                ret: signature.ret,
                source_display: None,
            },
            args,
        );
    }

    fn unavailable_context_parameters(
        &self,
        scope: &CheckerScope<'_>,
        context: &[Ty],
    ) -> Vec<MissingContextParameter> {
        context
            .iter()
            .enumerate()
            .filter(|(_, ty)| {
                self.select_context_arguments_with_types(scope, &[**ty])
                    .is_err()
            })
            .map(|(index, &ty)| MissingContextParameter { index, ty })
            .collect()
    }
}
