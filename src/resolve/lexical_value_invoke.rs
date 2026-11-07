//! Whether a lexical function value claims a bare call `name(args)` through the invoke convention.
//!
//! kotlinc's call tower visits the local levels before the implicit receivers and the top level,
//! and a level wins only with an applicable candidate. A function value whose fixed parameters
//! cannot take the arguments is an inapplicable candidate at its level: the tower moves on, so a
//! member of a nearer lambda receiver or a top-level function of the same name is called instead.
//! Only when no later level succeeds does the function value's own mismatch become the
//! diagnostic, as the nearest failed candidate.

use super::*;

/// A lexical function value that declined a bare call, kept until the call tower has tried every
/// later level.
pub(super) struct DeferredLexicalInvoke {
    pub(super) receiver_ty: Ty,
    pub(super) origin: ReceiverFnValueOrigin,
}

/// The bare call a deferred function value is finally committed to.
pub(super) struct LexicalInvokeSite<'a> {
    pub(super) callee: ExprId,
    pub(super) name: &'a str,
    pub(super) span: Span,
    pub(super) expected: Option<Ty>,
}

impl Checker<'_> {
    /// Whether the explicit invoke form `value.invoke(args)` of the function value typed
    /// `value_ty` certainly cannot take `args`: an argument whose checked type is not assignable to
    /// its fixed parameter. A form whose shape is decided elsewhere (the implicit receiver form,
    /// callable objects, postponed callable literals, open type parameters) is never rejected here.
    pub(super) fn lexical_invoke_rejects_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        value_ty: Ty,
        args: &[ExprId],
        implicit: Option<&ImplicitReceiverFunctionInvoke>,
    ) -> bool {
        if matches!(implicit, Some(ImplicitReceiverFunctionInvoke::Applicable)) {
            return false;
        }
        let TyKind::Fun(signature) = self.declared_function_semantic_type(value_ty).kind() else {
            return false;
        };
        if signature.params.len() != args.len()
            || args.iter().any(|&argument| {
                matches!(
                    self.file.expr(argument),
                    Expr::Lambda { .. } | Expr::CallableRef { .. }
                )
            })
        {
            return false;
        }
        // kotlinc types the arguments once, before the tower, so a later level must not see what
        // this probe proved: a cast in a later argument shadows its operand in the scope it is
        // checked in, and records flow facts. The probe checks them in a scope of its own and
        // leaves the call's flow as it found it.
        let entry_flow = scope.flow_snapshot();
        let probe_scope = scope.parameter_child();
        let arg_tys = self.invoke_operator_arg_tys(&probe_scope, call, value_ty, args);
        scope.restore_flow(&entry_flow);
        arg_tys
            .iter()
            .zip(&signature.params)
            .any(|(&actual, &parameter)| {
                actual != Ty::Error
                    && !actual.mentions_pending()
                    && !actual.mentions_ty_param()
                    && !parameter.mentions_ty_param()
                    && actual != Ty::Nothing
                    && !(actual == Ty::Null && parameter.admits_null())
                    && !self.receiver_is_assignable(actual, parameter)
            })
    }

    /// The value the deferred function value's invoke is called on.
    fn deferred_lexical_invoke_receiver(
        &mut self,
        scope: &CheckerScope<'_>,
        site: &LexicalInvokeSite<'_>,
        deferred: &DeferredLexicalInvoke,
    ) -> Ty {
        if matches!(
            deferred.origin,
            ReceiverFnValueOrigin::ClassStorage(_)
                | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. }
        ) {
            return self.expr_inner_name(scope, site.callee, site.name.to_string(), None);
        }
        deferred.receiver_ty
    }

    /// Report a deferred function value together with the inapplicable candidates of a later
    /// call-tower level. kotlinc then reports that none of the candidates is applicable, rather
    /// than either level's own argument mismatch.
    pub(super) fn report_deferred_lexical_invoke_among(
        &mut self,
        call_args: CallArgs<'_>,
        name: &str,
        span: Span,
        deferred: Option<&DeferredLexicalInvoke>,
        receiver_candidates: &[crate::libraries::FunctionInfo],
        later_candidates: &[crate::libraries::FunctionInfo],
    ) -> bool {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let Some(deferred) = deferred else {
            return false;
        };
        if (receiver_candidates.is_empty() && later_candidates.is_empty())
            || self.call_has_error_argument(args)
        {
            return false;
        }
        let TyKind::Fun(signature) = self
            .declared_function_semantic_type(deferred.receiver_ty)
            .kind()
        else {
            return false;
        };
        let mut invoke_sig = CallSig::metadata_plain(signature.params.len());
        invoke_sig.param_names = (1..=signature.params.len())
            .map(|index| format!("p{index}"))
            .collect();
        let parameters = Self::callable_ref_parameters(&signature.params, &invoke_sig.param_names);
        let suspend = if signature.suspend { "suspend " } else { "" };
        let mut message = String::from(INAPPLICABLE_OVERLOAD_PREFIX);
        self.append_rejected_lexical_candidate(
            &mut message,
            call_args,
            &format!(
                "{suspend}fun invoke({parameters}): {}",
                signature.ret.source_name()
            ),
            &signature.params,
            &invoke_sig,
        );
        for candidate in receiver_candidates.iter().chain(later_candidates) {
            let signature = candidate.semantic_signature();
            let context_count = candidate.context_count.min(signature.params.len());
            let call_sig = candidate.call_sig.suffix(context_count);
            let parameters = &signature.params[context_count..];
            let bindings = call_argument_parameter_indices(
                args.len(),
                parameters.len(),
                self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                self.file.call_has_trailing_lambda.contains(&call.0),
                &call_sig,
            )
            .map(|mapping| {
                crate::symbol_resolver::infer_generic_call_bindings(
                    &signature,
                    mapping
                        .iter()
                        .copied()
                        .zip(arg_tys.iter().copied())
                        .zip(named_whole_array_varargs(
                            &mapping,
                            self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                            &call_sig,
                        ))
                        .enumerate()
                        .map(|(source, ((parameter, actual), whole_array))| {
                            (
                                parameter + context_count,
                                actual,
                                whole_array || self.file.is_spread_arg(args[source]),
                            )
                        }),
                    candidate.call_sig.vararg_index,
                )
            })
            .unwrap_or_default();
            let parameters = parameters
                .iter()
                .map(|&parameter| {
                    crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings)
                })
                .collect::<Vec<_>>();
            self.append_rejected_lexical_candidate(
                &mut message,
                call_args,
                &self.member_candidate_display(name, candidate),
                &parameters,
                &call_sig,
            );
        }
        self.diags.error(span, message);
        true
    }

    /// Render each mapped rejection without publishing or retrying the rejected call.
    fn append_rejected_lexical_candidate(
        &self,
        message: &mut String,
        CallArgs {
            call,
            args,
            arg_tys,
        }: CallArgs<'_>,
        display: &str,
        parameters: &[Ty],
        signature: &CallSig,
    ) {
        message.push_str(&format!("\n\n{display}:"));
        let names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let Some(mapping) = call_argument_parameter_indices(
            args.len(),
            parameters.len(),
            names,
            trailing_lambda,
            signature,
        ) else {
            return;
        };
        let whole_arrays = named_whole_array_varargs(&mapping, names, signature);
        for (source, parameter) in mapping.into_iter().enumerate() {
            let declared = parameters[parameter];
            let expected = if signature.vararg_index == Some(parameter)
                && !self.file.is_spread_arg(args[source])
                && !whole_arrays[source]
            {
                declared.array_read_elem().unwrap_or(declared)
            } else {
                declared
            };
            let actual = arg_tys[source];
            if actual == Ty::Error
                || expected == Ty::Error
                || actual.mentions_pending()
                || expected.mentions_pending()
            {
                continue;
            }
            if signature.parameter_admits(parameter, expected, actual)
                && self.receiver_is_assignable(actual, expected)
                && !self.function_value_rejected(args[source], expected, actual)
            {
                continue;
            }
            if let Some(reason) = self.assignability_error_message(
                expected,
                self.suspend_converted_argument(expected, actual),
                "argument",
            ) {
                message.push_str("\n  ");
                let mut chars = reason.chars();
                if let Some(first) = chars.next() {
                    message.extend(first.to_uppercase());
                }
                message.extend(chars);
            }
        }
    }

    /// Commit a deferred function value after every later level of the call tower declined: its
    /// invoke reports the argument mismatch that made it inapplicable.
    pub(super) fn commit_deferred_lexical_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        site: &LexicalInvokeSite<'_>,
        deferred: DeferredLexicalInvoke,
    ) -> Ty {
        let receiver_ty = self.deferred_lexical_invoke_receiver(scope, site, &deferred);
        let argument_receiver_ty = self
            .receiver_function_value(scope, site.name)
            .map_or(receiver_ty, |(signature, _)| {
                Ty::from(TyKind::Fun(signature))
            });
        let CallArgs { call, args, .. } = call_args;
        let arg_tys = self.invoke_operator_arg_tys(scope, call, argument_receiver_ty, args);
        self.record_invoke_or_report(
            scope,
            CallArgs {
                call,
                args,
                arg_tys: &arg_tys,
            },
            site.callee,
            receiver_ty,
            site.span,
            CallResultConstraint::direct(site.expected),
        )
        .unwrap_or(Ty::Error)
    }

    /// A lambda/callable-reference literal can claim a function-value call rung only when that
    /// already-declared function shape supplies a callable expectation for the corresponding slot.
    /// Unlike a generic function declaration, invoking a value `(T) -> R` cannot infer or replace
    /// its enclosing `T`; when `T` is not itself callable, `value { ... }` is inapplicable and the
    /// next call-tower rung (for example an implicit-receiver extension) must be considered.
    pub(super) fn local_function_value_accepts_literal_arguments(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        arguments: &[ExprId],
    ) -> bool {
        let Some(TyKind::Fun(signature)) = self
            .local_callable_type(scope, name)
            .map(|(ty, _)| ty.kind())
        else {
            // Callable objects perform their own overload applicability selection.
            return true;
        };
        if arguments.iter().all(|&argument| {
            !matches!(
                self.file.expr(argument),
                Expr::Lambda { .. } | Expr::CallableRef { .. }
            )
        }) {
            // Arity and ordinary value-argument mismatches do not make a selected lexical value
            // disappear from the call tower. Its fixed `invoke` declaration owns the diagnostic.
            // Only postponed callable literals need this pre-applicability shape check: they can be
            // typed solely after a competing callable supplies an expectation, so a lexical value
            // whose corresponding parameter is not callable must let the next rung participate.
            return true;
        }
        let context_count = signature.context_count.min(signature.params.len());
        let explicit_context = context_count > 0 && arguments.len() == signature.params.len();
        let mut parameters = if explicit_context {
            signature.params.as_slice()
        } else {
            &signature.params[context_count..]
        };
        if signature.has_receiver && parameters.len() != arguments.len() {
            let Some((&receiver, values)) = parameters.split_first() else {
                return false;
            };
            if values.len() != arguments.len()
                || self
                    .receiver_function_implicit_receiver(
                        scope,
                        receiver,
                        values.len(),
                        arguments.len(),
                    )
                    .is_none()
            {
                return false;
            }
            parameters = values;
        }
        if parameters.len() != arguments.len() {
            return false;
        }
        arguments
            .iter()
            .zip(parameters)
            .all(|(&argument, &expected)| match self.file.expr(argument) {
                Expr::Lambda { .. } | Expr::CallableRef { .. } => {
                    expected.non_null().is_fun() || self.semantic_sam_signature(expected).is_some()
                }
                _ => true,
            })
    }
}
