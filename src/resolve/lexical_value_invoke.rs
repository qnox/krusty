//! Whether a lexical function value claims a bare call `name(args)` through the invoke convention.
//!
//! kotlinc's call tower visits the local levels before the implicit receivers and the top level,
//! and a level wins only with an applicable candidate. A function value whose fixed parameters
//! cannot take the arguments is an inapplicable candidate at its level: the tower moves on, so a
//! member of a nearer lambda receiver or a top-level function of the same name is called instead.
//! Only when no later level succeeds does the function value's own mismatch become the
//! diagnostic. A later candidate whose arguments map but whose types do not is a peer of that
//! mismatch, and kotlinc reports that none of those candidates is applicable. A later candidate
//! that does not map is a worse failure: the deferred value is committed before that failure is
//! reported.

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

/// Candidates that share the deferred value's type-mismatch applicability, in tower order.
pub(super) struct DeferredInvokePeers<'a> {
    /// Inapplicable members of an implicit receiver.
    pub(super) receiver: &'a [crate::libraries::FunctionInfo],
    /// Top-level functions and other receiver-less candidates.
    pub(super) later: &'a [crate::libraries::FunctionInfo],
    /// Same-file constructors whose arguments were considered and rejected.
    pub(super) source_constructors: Option<SourceConstructorRejections<'a>>,
    /// Exact provider constructor attempts, including mapping declines but rendering only their
    /// positively recorded type rejections.
    pub(super) provider_constructors: &'a [std::sync::Arc<RejectedProviderConstructors>],
}

pub(super) struct SourceConstructorRejections<'a> {
    pub(super) candidates: &'a [CtorDelegationCandidate],
    pub(super) rejected: &'a [ConstructorCandidateRejection],
    pub(super) name: &'a str,
    pub(super) generic: bool,
}

/// An assessed type-inapplicable declaration. Mapping and genericity stay typed until the common
/// specificity operation has chosen the diagnostic owner; rendered headers never drive selection.
struct RejectedInvokeCandidate {
    display: String,
    declared_arguments: Vec<Ty>,
    generic: bool,
    mismatches: Vec<constructor_arguments::ConstructorArgumentMismatch>,
    invoke: bool,
}

/// Whether a function value's fixed `parameter` certainly cannot take an argument of type
/// `actual`. A type parameter of an enclosing declaration is rigid inside its body and is judged
/// like any other type; only a variable that an active postponed call still infers (`open`) and an
/// argument whose type is not known yet are left to the constraint system. Both the body checker
/// and implicit-signature evaluation decide the lexical invoke rung with this one rule.
pub(in crate::resolve) fn function_value_parameter_rejects(
    oracle: &dyn crate::assignable::TypeOracle,
    open: &[crate::types::TyParamName],
    actual: Ty,
    parameter: Ty,
) -> bool {
    actual != Ty::Error
        && actual != Ty::Nothing
        && !actual.mentions_pending()
        && !ty_mentions_param(actual, open)
        && !ty_mentions_param(parameter, open)
        && !(actual == Ty::Null && parameter.admits_null())
        && !crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            oracle,
            actual,
            parameter,
        )
}

/// The leading parameters a scope supplies when a function value is invoked with `arguments`
/// explicit arguments, or `None` when no invocation form takes that many. A function type has
/// neither defaults nor varargs, so kotlinc admits three forms: every parameter explicit, the
/// context arguments from scope, and both the context arguments and the extension receiver from
/// scope. The body checker and implicit-signature evaluation share this one rule.
pub(in crate::resolve) fn function_value_implicit_prefix(
    signature: &crate::types::FnSig,
    arguments: usize,
) -> Option<usize> {
    let contexts = signature.context_count.min(signature.params.len());
    let receiver = usize::from(signature.has_receiver && contexts < signature.params.len());
    [0, contexts, contexts + receiver]
        .into_iter()
        .find(|&prefix| signature.params.len() - prefix == arguments)
}

/// The number of leading parameters a scope supplies in the invocation form implicit-signature
/// evaluation selects for a function value ([`function_value_implicit_prefix`]). `None` when no
/// form admits the arguments, by count, by a named or spread argument, or by a typed argument
/// [`function_value_parameter_rejects`]. Literal, lambda and provisional call arguments are typed
/// by the selected parameter, so they decide nothing here.
pub(in crate::resolve) fn signature_function_value_invoke_shape(
    table: &SymbolTable,
    source_file: u32,
    open: &[crate::types::TyParamName],
    signature: &crate::types::FnSig,
    arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
) -> Option<usize> {
    let implicit_prefix = function_value_implicit_prefix(signature, arguments.len())?;
    if arguments
        .iter()
        .any(|argument| argument.name.is_some() || argument.spread)
    {
        return None;
    }
    let module = crate::module_symbols::ModuleSymbols::for_file(table, source_file);
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn crate::symbol_source::SymbolSource,
        &*table.libraries as &dyn crate::symbol_source::SymbolSource,
    ]);
    let oracle = crate::symbol_resolver::SourceOracle(&source);
    let rejected = signature.params[implicit_prefix..]
        .iter()
        .zip(arguments)
        .any(|(&parameter, argument)| {
            argument.integer_literal.is_none()
                && !argument.lambda
                && !argument.contextual_call
                && !argument.wrapped_contextual_call
                && function_value_parameter_rejects(&oracle, open, argument.ty.get(), parameter)
        });
    (!rejected).then_some(implicit_prefix)
}

impl Checker<'_> {
    /// Every later level declined: report the type-mismatch peers together, or commit the nearer
    /// value before any later mapping-failure reporter. This does not publish a rejected peer.
    pub(super) fn finish_deferred_lexical_invoke_failure(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        site: &LexicalInvokeSite<'_>,
        deferred: Option<DeferredLexicalInvoke>,
        peers: DeferredInvokePeers<'_>,
    ) -> Option<Ty> {
        let deferred = deferred?;
        if self.report_deferred_lexical_invoke_among(
            scope,
            call_args,
            site.name,
            site.span,
            Some(&deferred),
            peers,
        ) {
            Some(Ty::Error)
        } else {
            Some(self.commit_deferred_lexical_invoke(scope, call_args, site, deferred))
        }
    }

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
        // An argument count no invocation form admits cannot be taken.
        if function_value_implicit_prefix(signature, args.len()).is_none() {
            return true;
        }
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
        // Its diagnostics are dropped too: an argument that cannot be checked against the value's
        // parameter without an error (`value.let { other(it) }` whose lambda result is not that
        // parameter) makes the value inapplicable, and the selected level checks the argument
        // again. A warning is not a failure: kotlinc still calls the value.
        let entry_flow = scope.flow_snapshot();
        let diagnostics = self.diags.diags.len();
        let probe_scope = scope.parameter_child();
        let arg_tys = self.invoke_operator_arg_tys(&probe_scope, call, value_ty, args);
        scope.restore_flow(&entry_flow);
        let argument_failed = self.diags.diags[diagnostics..]
            .iter()
            .any(|diagnostic| diagnostic.severity == crate::diag::Severity::Error);
        self.diags.diags.truncate(diagnostics);
        if argument_failed {
            return true;
        }
        let open_variables = self.active_postponed_type_variables();
        arg_tys
            .iter()
            .zip(&signature.params)
            .any(|(&actual, &parameter)| {
                function_value_parameter_rejects(self, &open_variables, actual, parameter)
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

    /// Report a deferred function value together with every other candidate whose arguments map
    /// but whose types do not. kotlinc then reports that none of those candidates is applicable.
    /// A candidate that does not map is omitted: that failure is worse, so the caller commits the
    /// deferred value instead of reporting it.
    pub(super) fn report_deferred_lexical_invoke_among(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        name: &str,
        span: Span,
        deferred: Option<&DeferredLexicalInvoke>,
        peers: DeferredInvokePeers<'_>,
    ) -> bool {
        let Some(deferred) = deferred else {
            return false;
        };
        if self.call_has_error_argument(call_args.args) {
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
        let Some(mut invoke) = self.rejected_type_mismatch_candidate(
            call_args,
            format!(
                "{suspend}fun invoke({parameters}): {}",
                signature.ret.source_name()
            ),
            &signature.params,
            &signature.params,
            &invoke_sig,
            false,
        ) else {
            return false;
        };
        invoke.invoke = true;
        let (nearer_lexical, farther_lexical) = Self::lexical_functions_around_value(scope, name);
        let mut candidates = nearer_lexical
            .iter()
            .filter_map(|callable| self.lexical_callable_type_mismatch(call_args, callable))
            .collect::<Vec<_>>();
        candidates.push(invoke);
        candidates.extend(
            farther_lexical
                .iter()
                .filter_map(|callable| self.lexical_callable_type_mismatch(call_args, callable)),
        );
        candidates.extend(
            peers
                .receiver
                .iter()
                .chain(peers.later)
                .filter_map(|candidate| {
                    self.function_candidate_type_mismatch(call_args, name, candidate)
                }),
        );
        if let Some(group) = peers.source_constructors {
            for rejection in group.rejected {
                let candidate = &group.candidates[rejection.candidate_index];
                candidates.extend(self.rejected_constructor_candidate(
                    call_args,
                    Self::constructor_candidate_display(group.name, candidate),
                    rejection,
                    group.generic,
                ));
            }
        }
        for group in peers.provider_constructors {
            for (candidate, rejection) in group.type_mismatches() {
                candidates.extend(self.rejected_constructor_candidate(
                    call_args,
                    Self::constructor_candidate_display(
                        group.classifier().nested_segment_ref(),
                        candidate,
                    ),
                    rejection,
                    group.generic(),
                ));
            }
        }
        crate::symbol_resolver::retain_most_specific_declarations(
            &self.fed_source(),
            None,
            &mut candidates,
            |candidate| {
                (
                    None,
                    candidate.declared_arguments.as_slice(),
                    candidate.generic,
                    None,
                )
            },
        );
        if let [candidate] = candidates.as_slice() {
            if candidate.invoke {
                // The existing invoke commitment owns its contextual argument checks and spans.
                return false;
            }
            for mismatch in &candidate.mismatches {
                let argument = call_args.args[mismatch.source_argument];
                self.report_assignability_error(
                    mismatch.expected,
                    self.suspend_converted_argument(mismatch.expected, mismatch.actual),
                    self.span(argument),
                    "argument",
                );
            }
        } else {
            let mut message = String::from(INAPPLICABLE_OVERLOAD_PREFIX);
            for candidate in &candidates {
                message.push_str(&format!("\n\n{}:", candidate.display));
                for mismatch in &candidate.mismatches {
                    let reason = self
                        .rejected_argument_reason(mismatch)
                        .expect("an assessed type mismatch has a diagnostic reason");
                    message.push_str("\n  ");
                    message.push_str(&reason.message);
                }
            }
            self.diags.error(span, message);
        }
        true
    }

    /// Lexical functions inside the value's scope, then functions outside it.
    pub(super) fn lexical_functions_around_value(
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> (Vec<LexicalCallable>, Vec<LexicalCallable>) {
        let mut nearer = Vec::new();
        let mut farther = Vec::new();
        let mut passed_value = false;
        for rung in scope.ancestors() {
            let functions = rung
                .own_binding(name, super::scope::Ns::Function)
                .map(|binding| binding.funs().to_vec())
                .unwrap_or_default();
            if passed_value {
                farther.extend(functions);
            } else {
                nearer.extend(functions);
                if rung.own_binding(name, super::scope::Ns::Value).is_some() {
                    passed_value = true;
                }
            }
        }
        (nearer, farther)
    }

    fn lexical_callable_type_mismatch(
        &self,
        call_args: CallArgs<'_>,
        callable: &LexicalCallable,
    ) -> Option<RejectedInvokeCandidate> {
        let LexicalCallableOrigin::Local(statement) = callable.origin else {
            return None;
        };
        let Stmt::LocalFun(function) = self.file.stmt(statement) else {
            return None;
        };
        if function.receiver.is_some() {
            return None;
        }
        let signature = &callable.signature;
        let context_count = signature.context_count.min(signature.params.len());
        let call_sig = signature.call_sig().suffix(context_count);
        let parameters = &signature.params[context_count..];
        let bindings = signature
            .generic_sig
            .as_ref()
            .map(|generic| {
                self.rejected_generic_argument_bindings(
                    call_args,
                    generic,
                    context_count,
                    parameters,
                    &call_sig,
                    signature.vararg_index,
                )
            })
            .unwrap_or_default();
        let parameters = parameters
            .iter()
            .map(|&parameter| crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings))
            .collect::<Vec<_>>();
        self.rejected_type_mismatch_candidate(
            call_args,
            super::source_signature_display::source_function_display(
                self.file,
                function,
                signature.ret,
            ),
            &parameters,
            &signature.params[context_count..],
            &call_sig,
            signature
                .generic_sig
                .as_ref()
                .is_some_and(|signature| !signature.formals.is_empty()),
        )
    }

    fn rejected_constructor_candidate(
        &self,
        call_args: CallArgs<'_>,
        display: String,
        rejection: &ConstructorCandidateRejection,
        generic: bool,
    ) -> Option<RejectedInvokeCandidate> {
        let mismatches = rejection
            .mapped_mismatches
            .iter()
            .copied()
            .filter(|mismatch| self.rejected_argument_reason(mismatch).is_some())
            .collect::<Vec<_>>();
        if mismatches.is_empty() {
            return None;
        }
        assert_eq!(rejection.argument_slots.len(), call_args.args.len());
        assert_eq!(rejection.declared_arguments.len(), call_args.args.len());
        Some(RejectedInvokeCandidate {
            display,
            declared_arguments: rejection.declared_arguments.clone(),
            generic,
            mismatches,
            invoke: false,
        })
    }

    pub(super) fn constructor_candidate_display(
        class_name: &str,
        candidate: &CtorDelegationCandidate,
    ) -> String {
        let rendered = candidate
            .target
            .params()
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let name = candidate
                    .param_names
                    .get(index)
                    .filter(|name| !name.is_empty())
                    .map(String::as_str)
                    .unwrap_or("p");
                let default = if candidate.defaults.get(index).copied().unwrap_or(false) {
                    " = ..."
                } else {
                    ""
                };
                format!("{name}: {}{default}", parameter.source_name())
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("constructor({rendered}): {class_name}")
    }

    fn rejected_argument_reason(
        &self,
        mismatch: &constructor_arguments::ConstructorArgumentMismatch,
    ) -> Option<crate::diag::NamedMessage> {
        if mismatch.actual == Ty::Error
            || mismatch.expected == Ty::Error
            || mismatch.actual.mentions_pending()
            || mismatch.expected.mentions_pending()
        {
            return None;
        }
        self.argument_mismatch_message(
            mismatch.expected,
            self.suspend_converted_argument(mismatch.expected, mismatch.actual),
        )
    }

    fn function_candidate_type_mismatch(
        &self,
        call_args: CallArgs<'_>,
        name: &str,
        candidate: &crate::libraries::FunctionInfo,
    ) -> Option<RejectedInvokeCandidate> {
        let signature = candidate.semantic_signature();
        let context_count = candidate.context_count.min(signature.params.len());
        let call_sig = candidate.call_sig.suffix(context_count);
        let parameters = &signature.params[context_count..];
        let bindings = self.rejected_generic_argument_bindings(
            call_args,
            &signature,
            context_count,
            parameters,
            &call_sig,
            candidate.call_sig.vararg_index,
        );
        let parameters = parameters
            .iter()
            .map(|&parameter| crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings))
            .collect::<Vec<_>>();
        let display = self
            .source_callable_display(candidate)
            .unwrap_or_else(|| Self::generic_callable_display(name, candidate));
        self.rejected_type_mismatch_candidate(
            call_args,
            display,
            &parameters,
            &signature.params[context_count..],
            &call_sig,
            !signature.formals.is_empty(),
        )
    }

    fn rejected_generic_argument_bindings(
        &self,
        CallArgs {
            call,
            args,
            arg_tys,
        }: CallArgs<'_>,
        signature: &crate::libraries::GenericSig,
        context_count: usize,
        parameters: &[Ty],
        call_sig: &CallSig,
        vararg_index: Option<usize>,
    ) -> crate::symbol_resolver::GSigBinds {
        call_argument_parameter_indices(
            args.len(),
            parameters.len(),
            self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
            self.file.call_has_trailing_lambda.contains(&call.0),
            call_sig,
        )
        .map(|mapping| {
            crate::symbol_resolver::infer_generic_call_bindings(
                signature,
                mapping
                    .iter()
                    .copied()
                    .zip(arg_tys.iter().copied())
                    .zip(named_whole_array_varargs(
                        &mapping,
                        self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                        call_sig,
                    ))
                    .enumerate()
                    .map(|(source, ((parameter, actual), whole_array))| {
                        (
                            parameter + context_count,
                            actual,
                            whole_array || self.file.is_spread_arg(args[source]),
                        )
                    }),
                vararg_index,
            )
        })
        .unwrap_or_default()
    }

    /// Assess only mapped argument-type contradictions. Keep declaration shapes separate from
    /// inferred expectations: applicability and declaration specificity answer different questions.
    fn rejected_type_mismatch_candidate(
        &self,
        CallArgs {
            call,
            args,
            arg_tys,
        }: CallArgs<'_>,
        display: String,
        parameters: &[Ty],
        declared: &[Ty],
        signature: &CallSig,
        generic: bool,
    ) -> Option<RejectedInvokeCandidate> {
        let names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let mapping = call_argument_parameter_indices(
            args.len(),
            parameters.len(),
            names,
            trailing_lambda,
            signature,
        )?;
        let whole_arrays = named_whole_array_varargs(&mapping, names, signature);
        let mut declared_arguments = Vec::with_capacity(mapping.len());
        let mut mismatches = Vec::new();
        for (source, parameter) in mapping.into_iter().enumerate() {
            let element = signature.vararg_index == Some(parameter)
                && !self.file.is_spread_arg(args[source])
                && !whole_arrays[source];
            let expected = parameters[parameter];
            let declared = *declared.get(parameter)?;
            let expected = if element {
                expected.array_read_elem()?
            } else {
                expected
            };
            let declared = if element {
                declared.array_read_elem()?
            } else {
                declared
            };
            declared_arguments.push(declared);
            let actual = arg_tys[source];
            if signature.parameter_admits(parameter, expected, actual)
                && self.receiver_is_assignable(actual, expected)
                && !self.function_value_rejected(args[source], expected, actual)
            {
                continue;
            }
            let mismatch = constructor_arguments::ConstructorArgumentMismatch {
                source_argument: source,
                expected,
                actual,
            };
            if self.rejected_argument_reason(&mismatch).is_some() {
                mismatches.push(mismatch);
            }
        }
        (!mismatches.is_empty()).then_some(RejectedInvokeCandidate {
            display,
            declared_arguments,
            generic,
            mismatches,
            invoke: false,
        })
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
            // Arity and ordinary value-argument mismatches are judged once the arguments are
            // typed (`lexical_invoke_rejects_arguments`); a rejected value is deferred, and its
            // fixed `invoke` declaration owns the diagnostic only if no later level applies.
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
