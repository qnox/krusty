//! Declaration, context and argument-mapping facts for terminal rejected-call diagnostics.
//!
//! Planning consumes the exact retained normalized candidates. It neither selects a callable nor
//! rechecks argument expressions; single-candidate diagnostics and joined diagnostics can inspect
//! the same applied declaration coordinates without running provider lookup again.

use super::*;

/// The call's fixed receiver/result/type-argument evidence and mapped rejected declarations.
/// The arity decision is retained from before specificity filtering, exactly as the original owner
/// computed it; consumers must not infer a new decision from the final family size.
pub(super) struct MappedRejectionPlan<'f, 'c> {
    pub(super) constraints: CallConstraints<'c>,
    pub(super) candidates: Vec<MappedRejectionCandidate<'f>>,
    pub(super) any_type_arity_matches: bool,
}

/// One retained candidate whose value arguments have a complete mapping. The declaration keeps its
/// bounds, source input/projection provenance and receiver identity; `shape` preserves the applied
/// context parameters and the source-to-declaration coordinate map.
pub(super) struct MappedRejectionCandidate<'f> {
    pub(super) candidate: &'f crate::libraries::FunctionInfo,
    pub(super) shape: ContextualCallShape,
    pub(super) parameters: Vec<usize>,
    pub(super) specificity: Vec<Ty>,
    pub(super) omitted_defaults: usize,
    pub(super) generic: bool,
    pub(super) missing_context: Option<MissingContextParameter>,
    pub(super) type_arity_matches: bool,
}

impl Checker<'_> {
    pub(super) fn mapped_rejection_candidates<'f, 'c>(
        &self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        constraints: CallConstraints<'c>,
        candidates: &'f [crate::libraries::FunctionInfo],
    ) -> MappedRejectionPlan<'f, 'c> {
        let type_args = constraints.type_args;
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let mut mapped = candidates
            .iter()
            .filter_map(|candidate| {
                let signature = candidate.semantic_signature();
                let type_arity_matches =
                    type_args.is_empty() || signature.formals.len() == type_args.len();
                let mut bindings = constraints.bindings(&self.fed_source(), &signature);
                let initial_params = signature
                    .params
                    .iter()
                    .map(|&parameter| {
                        crate::types::ty_subst_formals(parameter, &signature.formals, &bindings)
                    })
                    .collect::<Vec<_>>();
                let (mut shape, missing_context) = match self.contextual_call_shape_result(
                    scope,
                    &initial_params,
                    &candidate.call_sig,
                    candidate.context_count,
                    argument_names,
                ) {
                    Ok(shape) => (shape, None),
                    Err(missing) => {
                        let context_count = candidate.context_count.min(initial_params.len());
                        let explicitly_named = |parameter: usize| {
                            self.file.explicit_context_arguments
                                && candidate.call_sig.param_names.get(parameter).is_some_and(
                                    |parameter| {
                                        argument_names.is_some_and(|names| {
                                            names
                                                .iter()
                                                .flatten()
                                                .any(|argument| argument == parameter)
                                        })
                                    },
                                )
                        };
                        let parameter_indices = (context_count..initial_params.len())
                            .chain(
                                (0..context_count).filter(|&parameter| explicitly_named(parameter)),
                            )
                            .collect::<Vec<_>>();
                        let selected_context_indices = (0..context_count)
                            .filter(|&parameter| !explicitly_named(parameter))
                            .collect::<Vec<_>>();
                        let mut context_sources = vec![None; context_count];
                        let mut context_actual_types = vec![None; context_count];
                        for parameter in selected_context_indices {
                            let Ok(selected) = self.select_context_arguments_with_types(
                                scope,
                                std::slice::from_ref(&initial_params[parameter]),
                            ) else {
                                continue;
                            };
                            let Some((source, actual)) = selected.into_iter().next() else {
                                continue;
                            };
                            context_sources[parameter] = Some(source);
                            context_actual_types[parameter] = Some(actual);
                        }
                        let shape = ContextualCallShape {
                            params: parameter_indices
                                .iter()
                                .map(|&parameter| initial_params[parameter])
                                .collect(),
                            call_sig: candidate.call_sig.select_parameters(&parameter_indices),
                            parameter_indices,
                            context_sources,
                            context_actual_types,
                        };
                        // Context lookup consumes the applied shape, but kotlinc names an
                        // unavailable parameter with its declaration type (`x: T`, not `x: String`).
                        let diagnostic_missing = MissingContextParameter {
                            index: missing.index,
                            ty: signature.params[missing.index],
                        };
                        (shape, Some(diagnostic_missing))
                    }
                };
                for (parameter, actual) in shape.context_actual_types.iter().enumerate() {
                    let Some(actual) = actual else { continue };
                    let declared = *signature.params.get(parameter)?;
                    unify_member_extension_slot(declared, *actual, &[], &mut bindings);
                }
                let params = signature
                    .params
                    .iter()
                    .map(|&parameter| {
                        crate::types::ty_subst_formals(parameter, &signature.formals, &bindings)
                    })
                    .collect::<Vec<_>>();
                shape.params = shape
                    .parameter_indices
                    .iter()
                    .map(|&parameter| params[parameter])
                    .collect();
                let parameters = call_argument_parameter_indices(
                    args.len(),
                    shape.params.len(),
                    argument_names,
                    trailing_lambda,
                    &shape.call_sig,
                )?;
                let omitted_defaults = map_call_sig_args_with_trailing(
                    args,
                    argument_names,
                    shape.params.len(),
                    &shape.call_sig,
                    trailing_lambda,
                )
                .ok()?
                .iter()
                .filter(|argument| argument.is_none())
                .count();
                let specificity = parameters
                    .iter()
                    .enumerate()
                    .map(|(source, &parameter)| {
                        let declared = shape.params[parameter];
                        if shape.call_sig.vararg_index == Some(parameter)
                            && !self.file.is_spread_arg(args[source])
                        {
                            declared.array_read_elem().unwrap_or(declared)
                        } else {
                            declared
                        }
                    })
                    .collect::<Vec<_>>();
                let generic = candidate
                    .generic_sig
                    .as_ref()
                    .is_some_and(|signature| !signature.formals.is_empty());
                Some(MappedRejectionCandidate {
                    candidate,
                    shape,
                    parameters,
                    specificity,
                    omitted_defaults,
                    generic,
                    missing_context,
                    type_arity_matches,
                })
            })
            .collect::<Vec<_>>();
        // Scope-tower priority chooses the nearest APPLICABLE level. A nearer declaration whose
        // source arguments do not even map does not hide a mapped declaration on an outer level
        // (`with(outer) { with(inner) { receiver.extension(null) } }`). Selection already follows
        // that rule; rejected-call recovery must project the same tower before deciding which one
        // candidate owns the argument-type diagnostic.
        if let Some(nearest_scope_rank) = mapped
            .iter()
            .map(|mapped| {
                (
                    mapped.candidate.scope_rung.selection_rank(),
                    mapped.candidate.scope_rank,
                )
            })
            .min()
        {
            mapped.retain(|mapped| {
                (
                    mapped.candidate.scope_rung.selection_rank(),
                    mapped.candidate.scope_rank,
                ) == nearest_scope_rank
            });
        }
        let any_type_arity_matches = mapped.iter().any(|mapped| mapped.type_arity_matches);
        if any_type_arity_matches {
            mapped.retain(|mapped| mapped.type_arity_matches);
        }
        if mapped.len() > 1 {
            let dominated = mapped
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    mapped
                        .iter()
                        .any(|other| {
                            other.specificity == entry.specificity
                                && other.omitted_defaults < entry.omitted_defaults
                        })
                        .then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            mapped = mapped
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        crate::symbol_resolver::retain_most_specific_declarations(
            &self.fed_source(),
            Some(constraints.receiver),
            &mut mapped,
            |mapped| {
                let candidate = mapped.candidate;
                (
                    candidate
                        .is_extension()
                        .then(|| candidate.semantic_receiver())
                        .flatten(),
                    mapped.specificity.as_slice(),
                    mapped.generic,
                    Some(candidate.callable.owner),
                )
            },
        );
        MappedRejectionPlan {
            constraints,
            candidates: mapped,
            any_type_arity_matches,
        }
    }
}

/// The already-checked source arguments and declaration receiver substitution for one report.
/// Arguments remain in source order; the candidate's retained mapping supplies both parameter
/// coordinate systems. Reading this view cannot retype an expression or commit a selected call.
#[derive(Clone, Copy)]
pub(super) struct RejectedArgumentSite<'a> {
    pub(super) args: &'a [ExprId],
    pub(super) arg_tys: &'a [Ty],
    pub(super) whole_arrays: &'a [bool],
    pub(super) signature: &'a crate::libraries::GenericSig,
    pub(super) receiver_bindings: &'a crate::symbol_resolver::GSigBinds,
}

/// Applied and declaration-projected input types for a mapped source argument. The projection is
/// retained as evidence, not collapsed into ordinary assignability or a rendered diagnostic.
pub(super) struct RejectedArgumentInput {
    pub(super) expected: Ty,
    pub(super) actual: Ty,
    pub(super) input: Ty,
    pub(super) projection: Option<crate::symbol_resolver::InputProjection>,
}

impl MappedRejectionCandidate<'_> {
    pub(super) fn argument_input(
        &self,
        checker: &Checker<'_>,
        source: usize,
        site: RejectedArgumentSite<'_>,
    ) -> Option<RejectedArgumentInput> {
        let RejectedArgumentSite {
            args,
            arg_tys,
            whole_arrays,
            signature,
            receiver_bindings,
        } = site;
        let candidate = self.candidate;
        let shape = &self.shape;
        let parameter = *self.parameters.get(source)?;
        let (&declared, &actual) = shape.params.get(parameter).zip(arg_tys.get(source))?;
        let expected = if shape.call_sig.vararg_index == Some(parameter)
            && !checker.file.is_spread_arg(args[source])
            && !whole_arrays[source]
        {
            declared.array_read_elem().unwrap_or(declared)
        } else {
            declared
        };
        let declaration_parameter = shape.parameter_indices[parameter];
        let raw_input = candidate
            .source_inputs
            .as_deref()
            .map(|declaration| {
                declaration
                    .declared_input(declaration_parameter)
                    .expect("retained declaration inputs preserve the mapped parameter layout")
            })
            .unwrap_or(signature.params[declaration_parameter]);
        let raw_input = if shape.call_sig.vararg_index == Some(parameter)
            && !checker.file.is_spread_arg(args[source])
            && !whole_arrays[source]
        {
            raw_input.array_read_elem().unwrap_or(raw_input)
        } else {
            raw_input
        };
        let (input, projection) = crate::symbol_resolver::instantiate_input_slot(
            &checker.fed_source(),
            Some(signature),
            raw_input,
            receiver_bindings,
            crate::symbol_resolver::UnboundSpecialization::Preserve,
        );
        Some(RejectedArgumentInput {
            expected,
            actual,
            input,
            projection,
        })
    }
}
