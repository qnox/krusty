//! The outer call shape a callable-reference argument is resolved against.
//!
//! A reference argument needs its expected function type before its own overload family can be
//! selected, so the call's candidate is chosen first from the ordinary arguments. That choice walks
//! the same scope tower as the final selection.

use super::*;

impl Checker<'_> {
    /// Select the outer source call shape before contextually checking callable-reference arguments.
    /// Ordinary syntax/arity/non-reference argument scoring runs first; the selected signature then
    /// supplies the expected callable type to the single callable-reference resolver.
    pub(super) fn select_callable_ref_outer_signature(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        name: &str,
        slots: ArgSlots<'_>,
        expected_result: Option<Ty>,
    ) -> Option<(
        Signature,
        crate::symbol_resolver::GSigBinds,
        crate::fx_hash::HashSet<String>,
    )> {
        let ArgSlots {
            args,
            partial_arg_tys: partial,
            arg_names,
            trailing_lambda,
        } = slots;
        let explicit_type_arguments = self
            .file
            .call_type_args
            .get(&call.0)
            .map(|syntax| syntax.arguments.clone())
            .unwrap_or_default()
            .iter()
            .map(|argument| self.type_ref_ty(scope, argument))
            .collect::<Vec<_>>();
        let select_level = |functions: Vec<crate::libraries::FunctionInfo>,
                            receiver: Option<Ty>| {
            let mut candidates = Vec::new();
            for function in functions {
                if !self.source_callable_visible(&function) {
                    continue;
                }
                let signature = signature_from_resolved_function(&function);
                crate::trace_compiler!(
                    "resolve",
                    "callable-reference outer candidate name={name} source={:?} owner={} jvm_name={} descriptor={} visibility={:?} inline={:?} overload_rank={} params={:?} generic={:?}",
                    function.source_key,
                    function.callable.owner.render(),
                    function.callable.name,
                    function.callable.descriptor,
                    function.visibility,
                    function.flags.inline,
                    function.overload_rank,
                    signature.params,
                    signature.generic_sig,
                );
                let call_sig = signature.call_sig();
                let Some(argument_parameters) = call_argument_parameter_indices(
                    args.len(),
                    signature.params.len(),
                    arg_names,
                    trailing_lambda,
                    &call_sig,
                ) else {
                    continue;
                };

                let mut bindings = crate::symbol_resolver::GSigBinds::default();
                let semantic_params = if let Some(generic) = signature.generic_sig.as_ref() {
                    bindings = crate::symbol_resolver::seeded_gsig_binds(
                        generic,
                        &explicit_type_arguments,
                    );
                    // An extension's own formals are first constrained by the receiver it is
                    // called on, exactly as the final selection binds them.
                    if let Some((declared, actual)) = generic.receiver.zip(receiver) {
                        let mut inferred = crate::symbol_resolver::GSigBinds::default();
                        crate::symbol_resolver::unify_receiver_from_symbols(
                            &self.fed_source(),
                            declared,
                            actual,
                            &mut inferred,
                        );
                        crate::symbol_resolver::merge_generic_bindings(
                            generic,
                            explicit_type_arguments.len(),
                            &mut bindings,
                            inferred,
                        );
                    }
                    let inferred = crate::symbol_resolver::infer_generic_bindings(
                        generic,
                        argument_parameters
                            .iter()
                            .copied()
                            .zip(partial.iter().copied())
                            .enumerate()
                            .filter_map(|(source, (parameter, actual))| {
                                let actual = actual?;
                                let declared = *generic.params.get(parameter)?;
                                // Integer literals are soft constraints until the surrounding
                                // callable reference has been resolved. In
                                // `choose(0, ::takesByte)`, binding `T = Int` here prevents the
                                // reference from contributing the stronger `T = Byte` constraint,
                                // even though the literal is contextually admissible as `Byte`.
                                // Final call scoring still sees the literal expression and records
                                // its selected conversion after all constraints are known.
                                if ty_mentions_param(declared, &generic.formals)
                                    && args.get(source).is_some_and(|argument| {
                                        folded_integer_literal(self.file, *argument).is_some()
                                    })
                                {
                                    return None;
                                }
                                Some((parameter, actual))
                            }),
                    );
                    crate::symbol_resolver::merge_generic_bindings(
                        generic,
                        explicit_type_arguments.len(),
                        &mut bindings,
                        inferred,
                    );
                    if let Some(expected) = expected_result {
                        if let Some(inferred) =
                            crate::symbol_resolver::infer_generic_return_bindings(
                                generic,
                                expected,
                                |actual, bound| self.receiver_is_assignable(actual, bound),
                            )
                        {
                            crate::symbol_resolver::merge_generic_bindings(
                                generic,
                                explicit_type_arguments.len(),
                                &mut bindings,
                                inferred,
                            );
                        }
                    }
                    if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                        generic,
                        &bindings,
                        |actual, bound| self.generic_bound_admits(actual, bound),
                    ) {
                        continue;
                    }
                    generic
                        .params
                        .iter()
                        .map(|parameter| {
                            crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings)
                        })
                        .collect::<Vec<_>>()
                } else {
                    signature.params.clone()
                };
                let Some(score) =
                    call_applicability::score(self, scope, &semantic_params, &call_sig, slots)
                else {
                    crate::trace_compiler!(
                        "resolve",
                        "callable-reference outer candidate rejected name={name} semantic_params={semantic_params:?} partial={partial:?}",
                    );
                    continue;
                };
                let low_priority_annotation =
                    crate::types::type_name("kotlin/internal/LowPriorityInOverloadResolution");
                candidates.push((
                    score.rank,
                    function
                        .callable
                        .annotations
                        .contains(&low_priority_annotation),
                    signature,
                    bindings,
                ));
            }
            if candidates.iter().any(|(_, low_priority, ..)| !low_priority) {
                candidates.retain(|(_, low_priority, ..)| !*low_priority);
            }
            let best = candidates.iter().map(|candidate| candidate.0).max()?;
            crate::trace_compiler!(
                "resolve",
                "callable-reference outer selection name={name} candidates={} best={best:?}",
                candidates.len(),
            );
            let mut best_candidates = candidates
                .into_iter()
                .filter(|candidate| candidate.0 == best);
            let (_, _, signature, bindings) = best_candidates.next()?;
            if best_candidates.next().is_some() {
                return None;
            }
            let invariant_expected_result_constraints = expected_result
                .and_then(|expected| {
                    let source = self.fed_source();
                    crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                        &source,
                        signature.generic_sig.as_ref()?,
                        expected,
                        |actual, bound| self.receiver_is_assignable(actual, bound),
                    )
                    .map(|expected_bindings| {
                        crate::symbol_resolver::expected_result_equality_formals(
                            &source,
                            signature
                                .generic_sig
                                .as_ref()
                                .expect("generic result bindings"),
                            expected,
                            &expected_bindings,
                        )
                    })
                })
                .unwrap_or_default();
            crate::trace_compiler!(
                "resolve",
                "callable-reference outer selected name={name} bindings={bindings:?} invariant_expected_result_constraints={invariant_expected_result_constraints:?}",
            );
            Some((signature, bindings, invariant_expected_result_constraints))
        };

        // Callable-reference arguments need an expected signature before their own resolution, but
        // that does not change the ordinary scope tower. Probe one candidate level at a time: each
        // implicit receiver's members, then the extensions applicable to it one scope level at a
        // time, and only then package-level callables. Every provider supplies the same
        // `FunctionInfo` structure here.
        for receiver in self.implicit_receiver_types(scope) {
            let callables = self.stable_receiver_callables(receiver, name);
            let (members, extensions): (Vec<_>, Vec<_>) = callables
                .functions()
                .iter()
                .cloned()
                .partition(|function| function.kind == crate::libraries::FnKind::Member);
            if let Some(selected) = select_level(members, None) {
                return Some(selected);
            }
            for level in extensions.chunk_by(|left, right| {
                (left.scope_rung, left.scope_rank) == (right.scope_rung, right.scope_rank)
            }) {
                if let Some(selected) = select_level(level.to_vec(), Some(receiver)) {
                    return Some(selected);
                }
            }
        }
        select_level(self.resolver().top_level_candidates(name), None)
    }
}
