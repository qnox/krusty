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
    /// Functions declared inside the value's scope.
    pub(super) nearer_lexical: &'a [LexicalCallable],
    /// Inapplicable members of an implicit receiver.
    pub(super) receiver: &'a [crate::libraries::FunctionInfo],
    /// Functions declared outside the value's scope.
    pub(super) farther_lexical: &'a [LexicalCallable],
    /// Top-level functions and other receiver-less candidates.
    pub(super) later: &'a [crate::libraries::FunctionInfo],
    /// Same-file constructors whose arguments were considered and rejected.
    pub(super) constructors: &'a [CtorDelegationCandidate],
    /// Simple name of the classifier those constructors belong to.
    pub(super) constructor_name: &'a str,
    /// A provider constructor that mapped and then rejected an argument type.
    /// The string is its `constructor(...): Name` rendering.
    pub(super) provider_constructor: Option<(&'a str, Ty, Ty)>,
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

    /// Report a deferred function value together with every other candidate whose arguments map
    /// but whose types do not. kotlinc then reports that none of those candidates is applicable.
    /// A candidate that does not map is omitted: that failure is worse, so the caller commits the
    /// deferred value instead of reporting it.
    pub(super) fn report_deferred_lexical_invoke_among(
        &mut self,
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
        let Some(invoke) = self.rejected_type_mismatch_section(
            call_args,
            &format!(
                "{suspend}fun invoke({parameters}): {}",
                signature.ret.source_name()
            ),
            &signature.params,
            &invoke_sig,
        ) else {
            return false;
        };
        let nearer = peers
            .nearer_lexical
            .iter()
            .filter_map(|callable| self.lexical_callable_type_mismatch(call_args, callable))
            .collect::<Vec<_>>();
        let farther = peers
            .farther_lexical
            .iter()
            .filter_map(|callable| self.lexical_callable_type_mismatch(call_args, callable))
            .chain(
                peers
                    .receiver
                    .iter()
                    .chain(peers.later)
                    .filter_map(|candidate| {
                        self.function_candidate_type_mismatch(call_args, name, candidate)
                    }),
            )
            .chain(peers.constructors.iter().filter_map(|candidate| {
                self.constructor_type_mismatch(call_args, peers.constructor_name, candidate)
            }))
            .chain(
                peers
                    .provider_constructor
                    .and_then(|(display, expected, actual)| {
                        self.rendered_argument_mismatch_section(display, expected, actual)
                    }),
            )
            .collect::<Vec<_>>();
        if nearer.is_empty() && farther.is_empty() {
            return false;
        }
        let mut message = String::from(INAPPLICABLE_OVERLOAD_PREFIX);
        for section in &nearer {
            message.push_str(section);
        }
        message.push_str(&invoke);
        for section in &farther {
            message.push_str(section);
        }
        self.diags.error(span, message);
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
    ) -> Option<String> {
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
        self.rejected_type_mismatch_section(
            call_args,
            &super::source_signature_display::source_function_display(
                self.file,
                function,
                signature.ret,
            ),
            &signature.params[context_count..],
            &call_sig,
        )
    }

    fn constructor_type_mismatch(
        &self,
        call_args: CallArgs<'_>,
        class_name: &str,
        candidate: &CtorDelegationCandidate,
    ) -> Option<String> {
        if class_name.is_empty() {
            return None;
        }
        let parameters = candidate.target.params();
        let call_sig = CallSig::metadata_member(
            parameters.len(),
            candidate.param_names.clone(),
            candidate.defaults.clone(),
            candidate.vararg,
        );
        self.rejected_type_mismatch_section(
            call_args,
            &Self::constructor_candidate_display(class_name, candidate),
            parameters,
            &call_sig,
        )
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

    fn rendered_argument_mismatch_section(
        &self,
        display: &str,
        expected: Ty,
        actual: Ty,
    ) -> Option<String> {
        if display.is_empty()
            || actual == Ty::Error
            || expected == Ty::Error
            || actual.mentions_pending()
            || expected.mentions_pending()
        {
            return None;
        }
        let reason = self.assignability_error_message(
            expected,
            self.suspend_converted_argument(expected, actual),
            "argument",
        )?;
        let mut section = format!("\n\n{display}:\n  ");
        let mut chars = reason.chars();
        if let Some(first) = chars.next() {
            section.extend(first.to_uppercase());
        }
        section.extend(chars);
        Some(section)
    }

    fn function_candidate_type_mismatch(
        &self,
        call_args: CallArgs<'_>,
        name: &str,
        candidate: &crate::libraries::FunctionInfo,
    ) -> Option<String> {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
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
            .map(|&parameter| crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings))
            .collect::<Vec<_>>();
        self.rejected_type_mismatch_section(
            call_args,
            &signature_display_with_defaults(
                &self.member_candidate_display(name, candidate),
                &call_sig.param_defaults,
            ),
            &parameters,
            &call_sig,
        )
    }

    /// The candidate section kotlinc prints for a mapped call whose argument types do not fit.
    /// `None` when the arguments do not map, or when no argument has a concrete type mismatch:
    /// those failures are not peers of a deferred function value.
    fn rejected_type_mismatch_section(
        &self,
        CallArgs {
            call,
            args,
            arg_tys,
        }: CallArgs<'_>,
        display: &str,
        parameters: &[Ty],
        signature: &CallSig,
    ) -> Option<String> {
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
        let mut section = format!("\n\n{display}:");
        let mut mismatched = false;
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
            let Some(reason) = self.assignability_error_message(
                expected,
                self.suspend_converted_argument(expected, actual),
                "argument",
            ) else {
                continue;
            };
            section.push_str("\n  ");
            let mut chars = reason.chars();
            if let Some(first) = chars.next() {
                section.extend(first.to_uppercase());
            }
            section.extend(chars);
            mismatched = true;
        }
        mismatched.then_some(section)
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

/// A signature rendered without its defaults (`y: Int`) still denotes those parameters. kotlinc
/// prints each one as `y: Int = ...` in a candidate list.
fn signature_display_with_defaults(display: &str, defaults: &[bool]) -> String {
    if !defaults.iter().any(|has_default| *has_default) || display.contains(" = ...") {
        return display.to_string();
    }
    let Some(open) = display.find('(') else {
        return display.to_string();
    };
    let Some(close) = matching_paren(display, open) else {
        return display.to_string();
    };
    let mut parameters = split_top_level(&display[open + 1..close], ',');
    if parameters.len() != defaults.len() {
        return display.to_string();
    }
    for (parameter, &has_default) in parameters.iter_mut().zip(defaults) {
        if has_default {
            parameter.push_str(" = ...");
        }
    }
    let mut rendered = String::new();
    rendered.push_str(&display[..open + 1]);
    rendered.push_str(&parameters.join(","));
    rendered.push_str(&display[close..]);
    rendered
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (offset, character) in text[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            found if found == separator && depth == 0 => {
                parts.push(text[start..index].to_string());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(text[start..].to_string());
    parts
}
