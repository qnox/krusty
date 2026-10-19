//! The `cannot infer type for type parameter` report over a selected call's common inference
//! outcome.
//!
//! Every selected callable, a constructor included, records its type arguments in the selected
//! generic-call record; a formal nothing constrained has no recorded solution. The report reads
//! that record together with the callable's result signature at a completion root, after any
//! enclosing call or joined conditional sibling has had the chance to complete it. It never
//! reconstructs inference evidence from the specialized parameters or the completed result type.

use super::*;

impl Checker<'_> {
    /// Record a source constructor call's inferred declaration-order `bindings`, where a type
    /// parameter nothing bound is still `Ty::Error`.
    pub(super) fn record_source_constructor_type_arguments(
        &mut self,
        call: ExprId,
        class: &ClassSig,
        bindings: &[Ty],
    ) {
        let bound = class
            .type_params
            .iter()
            .cloned()
            .zip(bindings.iter().copied())
            .filter(|(_, binding)| *binding != Ty::Error)
            .collect::<crate::symbol_resolver::GSigBinds>();
        let bounds = class
            .type_param_bounds
            .iter()
            .map(|bound| {
                (*bound != Ty::Error)
                    .then_some(*bound)
                    .into_iter()
                    .collect()
            })
            .collect::<Vec<_>>();
        let arguments = crate::symbol_resolver::solved_constructor_type_arguments(
            class.internal_name(),
            &class.type_params,
            &bounds,
            &bound,
        );
        self.replace_call_type_arguments(call, arguments);
    }

    /// Record a dependency constructor call's inferred `bindings`. Only the classifier's own type
    /// parameters are the call's to infer; the captured ones of an inner classifier come from its
    /// outer receiver and are always solved.
    pub(super) fn record_library_constructor_type_arguments(
        &mut self,
        call: ExprId,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let arguments = Self::library_constructor_type_arguments(owner, classifier, bindings);
        self.replace_call_type_arguments(call, arguments);
    }

    /// Diagnose the inference outcome of a dependency constructor that access control rejects. The
    /// outcome is source-observable, but a rejected call never enters the selected-call record.
    pub(super) fn report_rejected_constructor_type_arguments(
        &mut self,
        call: ExprId,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        let arguments = Self::library_constructor_type_arguments(owner, classifier, bindings);
        let Some(formal) = classifier
            .type_params()
            .iter()
            .zip(arguments)
            .find_map(|(formal, argument)| argument.is_none().then_some(formal))
        else {
            return;
        };
        self.diags.error(
            self.call_callee_name_span(call),
            format!(
                "cannot infer type for type parameter '{}'. Specify it explicitly.",
                crate::types::type_parameter_source_name(formal)
            ),
        );
    }

    fn library_constructor_type_arguments(
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> Vec<Option<Ty>> {
        let own = classifier.own_type_parameter_count;
        let formals = classifier.type_params();
        let bounds = classifier.type_param_bounds();
        let own_formals = formals.get(..own).unwrap_or_default();
        let own_bounds = bounds.get(..own).unwrap_or_default();
        let mut arguments = crate::symbol_resolver::solved_constructor_type_arguments(
            owner,
            own_formals,
            own_bounds,
            bindings,
        );
        let completed = crate::symbol_resolver::completed_constructor_result_arguments(
            owner, formals, bounds, bindings,
        );
        arguments.extend(completed.into_iter().skip(arguments.len()).map(Some));
        arguments
    }

    pub(super) fn report_unbound_call_result_type_parameter(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
    ) -> bool {
        let Some(signature) = self
            .unbound_call_result_signature(call)
            .map(std::borrow::Cow::into_owned)
        else {
            return false;
        };
        let actual = self.expr_types[call.0 as usize];
        let resolved = self.selected_call_type_arguments(call);
        let lexical_formals = scope.lexical_tparam_identities();
        // A formal lexically in scope here resolved to the enclosing declaration's own type is a
        // complete solution of this call's fresh variable, not an uninferred occurrence.
        let open_formals = signature
            .formals
            .iter()
            .filter(|formal| !lexical_formals.contains(formal))
            .cloned()
            .collect::<Vec<_>>();
        let unbound_value_lambda_formals = self.unbound_value_lambda_call_formals.get(&call);
        let open_lambda_result_formals = self.open_lambda_result_call_formals.get(&call);
        let call_arguments = match self.file.expr(call) {
            Expr::Call { args, .. } => args.as_slice(),
            _ => &[],
        };
        let had_postponed_argument_diagnostic = call_arguments
            .iter()
            .any(|argument| self.postponed_diagnostics.was_captured(*argument));
        let call_span = self.span(call);
        let has_nested_diagnostic = self.diags.diags.iter().any(|diagnostic| {
            diagnostic.severity == crate::diag::Severity::Error
                && diagnostic.file == self.file_index
                && diagnostic.span.lo >= call_span.lo
                && diagnostic.span.hi <= call_span.hi
        }) || self
            .postponed_diagnostics
            .contains_error_in_span(self.file_index, call_span);
        let Some((index, formal)) = signature
            .formals
            .iter()
            .enumerate()
            .find(|(index, formal)| {
                // A recorded argument that is only a lambda result's open nested-call variable
                // (`R := T` of `Mono.empty()`) leaves this formal uninferred along with it.
                let open_dependency = resolved
                    .and_then(|arguments| arguments.get(*index))
                    .copied()
                    .flatten()
                    .is_some_and(|argument| {
                        self.type_argument_depends_on_open_lambda_result(call, argument)
                    });
                (ty_mentions_param(actual, std::slice::from_ref(*formal))
                    || ty_mentions_param(signature.ret, std::slice::from_ref(*formal)))
                    && (unbound_value_lambda_formals
                        .is_some_and(|formals| formals.contains(formal))
                        || open_dependency
                        || had_postponed_argument_diagnostic
                        || has_nested_diagnostic
                        || !signature
                            .params
                            .iter()
                            .any(|parameter| {
                                ty_mentions_param(*parameter, std::slice::from_ref(*formal))
                            }))
                    && (open_lambda_result_formals
                        .is_some_and(|formals| formals.contains(formal))
                        || open_dependency
                        || resolved
                            .and_then(|arguments| arguments.get(*index))
                            .copied()
                            .flatten()
                            .is_none_or(|argument| ty_mentions_param(argument, &open_formals)))
                    // A star-projected receiver can leave the selected method result bounded by a
                    // non-lexical classifier parameter. That stable symbolic bound is the captured
                    // result Kotlin chooses; it is neither an uninferred method formal nor a
                    // spelling-based fallback. A caller-owned lexical parameter does not have this
                    // property and still requires ordinary inference evidence.
                    && actual
                        .type_parameter_occurrence_bound(formal)
                        .or_else(|| signature.ret.type_parameter_occurrence_bound(formal))
                        .and_then(|bound| bound.non_null().ty_param_name())
                        .is_none_or(|bound| {
                            signature.formals.iter().any(|owned| owned == bound)
                                || lexical_formals.iter().any(|lexical| lexical == bound)
                        })
            })
        else {
            return false;
        };
        // With written type arguments, only a `_` placeholder leaves a formal open; kotlinc
        // reports it at that placeholder rather than at the callee.
        let span = self
            .file
            .call_type_args
            .get(&call.0)
            .and_then(|syntax| syntax.arguments.get(index))
            .map_or_else(
                || self.call_callee_name_span(call),
                |argument| argument.span,
            );
        self.diags.error(
            span,
            format!(
                "cannot infer type for type parameter '{}'. Specify it explicitly.",
                crate::types::type_parameter_source_name(formal)
            ),
        );
        true
    }
}

/// Complete a source constructor's declaration-order `bindings`: a `null`-only binding is
/// `Nothing?`, and a type parameter nothing bound takes its declared bound as the call's
/// provisional result (its selected-call record has no solution for it unless it is a self type).
pub(super) fn complete_source_constructor_bindings(
    class: &ClassSig,
    mut bindings: Vec<Ty>,
) -> Vec<Ty> {
    for (index, binding) in bindings.iter_mut().enumerate() {
        if *binding == Ty::Null {
            *binding = Ty::nullable(Ty::Nothing);
        } else if *binding == Ty::Error {
            *binding = class
                .type_param_bounds
                .get(index)
                .copied()
                .filter(|bound| *bound != Ty::Error)
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        }
    }
    bindings
}
