//! Candidate projection for builder-inference calls inside contextual function literals.
//!
//! The compact graph owns only the active constraint variables. Candidate collection, argument
//! mapping, overload selection, and generic inference remain in `SymbolResolver`; this adapter
//! substitutes constraints learned from typed arguments into each candidate before invoking that
//! ordinary selector, then exposes only the selected declaration's constraints for commitment.

use super::*;

pub(super) struct PostponedCallableFamily {
    callables: crate::libraries::Callables,
    arguments: Vec<crate::symbol_resolver::CallArgKind>,
    bindings: Vec<(
        crate::libraries::FunctionInfo,
        crate::symbol_resolver::GSigBinds,
    )>,
}

#[derive(Clone, Copy)]
pub(super) struct PostponedReceiverCall<'a, 'source> {
    pub(super) scope: crate::fir::SignatureScope,
    pub(super) receiver: Ty,
    pub(super) spelling: &'a str,
    pub(super) arguments: &'a [crate::fir::SigCallArgumentProbe<'source>],
    pub(super) type_arguments: &'a [Ty],
    pub(super) trailing_lambda: bool,
}

impl PostponedCallableFamily {
    pub(super) fn callables(&self) -> &crate::libraries::Callables {
        &self.callables
    }

    pub(super) fn arguments(&self) -> &[crate::symbol_resolver::CallArgKind] {
        &self.arguments
    }

    pub(super) fn selected_bindings(
        &self,
        selected: &crate::libraries::FunctionInfo,
    ) -> crate::symbol_resolver::GSigBinds {
        self.bindings
            .iter()
            .find(|(candidate, _)| same_callable(candidate, selected))
            .map(|(_, bindings)| bindings.clone())
            .unwrap_or_default()
    }
}

fn same_callable(
    left: &crate::libraries::FunctionInfo,
    right: &crate::libraries::FunctionInfo,
) -> bool {
    left.stable_declaration == right.stable_declaration
        && left.source_member == right.source_member
        && left.source_key == right.source_key
        && left.callable.owner == right.callable.owner
        && left.callable.name == right.callable.name
        && left.callable.descriptor == right.callable.descriptor
}

fn merge_active_constraints(
    bindings: &mut crate::symbol_resolver::GSigBinds,
    constraints: crate::symbol_resolver::AssignabilityConstraints,
    active: &std::collections::HashSet<&str>,
) {
    let inferred =
        constraints
            .lower
            .into_iter()
            .chain(constraints.upper.into_iter().flat_map(|(formal, bounds)| {
                bounds.into_iter().map(move |bound| (formal.clone(), bound))
            }));
    for (formal, actual) in inferred {
        if active.contains(formal.as_str()) {
            bindings
                .entry(formal)
                .and_modify(|known| {
                    *known = crate::symbol_resolver::merge_inferred_ty(Some(*known), actual)
                })
                .or_insert(actual);
        }
    }
}

pub(super) fn collect_type_parameters(
    ty: Ty,
    parameters: &mut std::collections::HashSet<&'static str>,
) {
    match ty {
        Ty::TyParam(name, _) => {
            parameters.insert(name);
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => collect_type_parameters(*inner, parameters),
        // A star's upper bound is a computed capture fact, not a source occurrence that can
        // constrain a postponed call's type parameters.
        Ty::StarProjection(_) => {}
        Ty::Obj(_, arguments) => {
            for argument in arguments {
                collect_type_parameters(*argument, parameters);
            }
        }
        Ty::Fun(signature) => {
            for parameter in &signature.params {
                collect_type_parameters(*parameter, parameters);
            }
            collect_type_parameters(signature.ret, parameters);
        }
        _ => {}
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn candidate_participates_in_signature_selection(
        candidate: &crate::libraries::FunctionInfo,
    ) -> bool {
        candidate.visibility == crate::types::Visibility::Public
            || candidate.flags.inline.must_inline()
            || candidate.stable_declaration.is_some()
    }

    pub(super) fn common_postponed_parameters(
        &self,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        shapes: Vec<Vec<Ty>>,
    ) -> Option<Vec<Ty>> {
        if shapes.is_empty() || shapes.iter().any(|shape| shape.len() != arguments.len()) {
            return None;
        }
        let mut common = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            if let crate::fir::SigCallArgumentProbe::Typed(argument) = argument {
                // A nested generic call takes the parameter every shape agrees on as its
                // expectation (`s returns emptyList()` against `returns(v: List<String>)`); an
                // ordinary typed argument constrains nothing here.
                let shared = argument
                    .contextual_call
                    .then(|| shapes.first().map(|shape| shape[index]))
                    .flatten()
                    .filter(|first| shapes.iter().all(|shape| shape[index] == *first))
                    .filter(|parameter| !parameter.mentions_ty_param());
                common.push(shared.unwrap_or_else(|| Ty::obj("kotlin/Any")));
                continue;
            }
            let functions = shapes
                .iter()
                .map(|shape| resolver.functional_expectation(shape[index]))
                .collect::<Option<Vec<_>>>()?;
            let Ty::Fun(first) = functions.first()?.non_null() else {
                return None;
            };
            let same_inputs = functions.iter().all(|function| {
                matches!(function.non_null(), Ty::Fun(signature)
                    if signature.params == first.params
                        && signature.context_count == first.context_count
                        && signature.has_receiver == first.has_receiver
                        && signature.suspend == first.suspend)
            });
            if !same_inputs {
                return None;
            }
            let same_result = functions.iter().all(
                |function| matches!(function.non_null(), Ty::Fun(signature) if signature.ret == first.ret),
            );
            if !same_result
                && matches!(
                    argument,
                    crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. }
                )
            {
                return None;
            }
            let result = if same_result {
                first.ret
            } else {
                Ty::ty_param(
                    "\0sig:common-lambda-result",
                    Ty::nullable(Ty::obj("kotlin/Any")),
                )
            };
            common.push(Ty::fun_with_shape(
                first.params.clone(),
                result,
                first.context_count,
                first.has_receiver,
                first.suspend,
            ));
        }
        Some(common)
    }

    /// Resolve the contextual parameter shape of one receiver-callable family.
    ///
    /// Most families share one declaration-slot mapping, so selection can operate on one mapped
    /// argument vector. When overloads place a source argument in different declaration slots, map
    /// and specialize each candidate independently, project its parameters back into source order,
    /// and retain only expectations common to every applicable candidate. No positional retry is
    /// valid here: source order is an output of declaration-owned mapping, not a substitute for it.
    pub(super) fn receiver_family_postponed_parameters(
        &self,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        callables: crate::libraries::Callables,
        call: PostponedReceiverCall<'_, '_>,
    ) -> Option<(Vec<Ty>, Vec<Option<usize>>)> {
        let PostponedReceiverCall {
            scope,
            receiver,
            spelling,
            arguments,
            type_arguments,
            trailing_lambda,
        } = call;
        let (parameters, slots) = if let Some((kinds, slots)) =
            Self::probe_call_arguments(callables.functions(), arguments, trailing_lambda)
        {
            let projected = self.project_postponed_callables(scope, receiver, callables, &kinds);
            let parameters = match resolver.select_receiver_function_with_params(
                receiver,
                spelling,
                projected.arguments(),
                type_arguments,
                projected.callables(),
            ) {
                Some((_, parameters)) => parameters,
                None => self.common_postponed_parameters(
                    resolver,
                    arguments,
                    resolver.receiver_function_parameter_shapes(
                        receiver,
                        projected.arguments(),
                        type_arguments,
                        projected.callables(),
                    ),
                )?,
            };
            (parameters, slots)
        } else {
            let parameters =
                self.common_candidate_mapped_parameters(resolver, callables.functions(), call)?;
            let slots = (0..arguments.len()).map(Some).collect();
            (parameters, slots)
        };
        let parameters = parameters
            .into_iter()
            .map(|parameter| {
                resolver
                    .functional_expectation(parameter)
                    .unwrap_or(parameter)
            })
            .collect();
        Some((parameters, slots))
    }

    fn common_candidate_mapped_parameters(
        &self,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        candidates: &[crate::libraries::FunctionInfo],
        call: PostponedReceiverCall<'_, '_>,
    ) -> Option<Vec<Ty>> {
        let PostponedReceiverCall {
            scope,
            receiver,
            spelling,
            arguments,
            type_arguments,
            trailing_lambda,
        } = call;
        if arguments.iter().any(|argument| {
            matches!(
                argument,
                crate::fir::SigCallArgumentProbe::PostponedLambda { spread: true, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        spread: true,
                        ..
                    }
            )
        }) {
            return None;
        }
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let extra_admitted = |first: usize, extra: usize| {
            Self::same_vararg_element_probe(&arguments[first], &arguments[extra])
        };
        let mapped = candidates
            .iter()
            .filter(|candidate| Self::candidate_participates_in_signature_selection(candidate))
            .filter_map(|candidate| {
                let slots = Self::candidate_call_slots(
                    candidate,
                    &names,
                    arguments.len(),
                    trailing_lambda,
                    &extra_admitted,
                )?;
                Some((candidate, slots))
            })
            .collect::<Vec<_>>();
        if mapped.is_empty() {
            return None;
        }
        let shapes = mapped
            .into_iter()
            .map(|(candidate, slots)| {
                crate::trace_compiler!(
                    "signature",
                    "candidate-mapped expectation {spelling} receiver={receiver:?} slots={slots:?} candidate={}{}",
                    candidate.callable.name,
                    candidate.callable.descriptor,
                );
                let kinds = slots
                    .iter()
                    .map(|source| {
                        source
                            .and_then(|source| arguments.get(source))
                            .map(Self::probe_argument_kind)
                            .unwrap_or(crate::symbol_resolver::CallArgKind::OmittedDefault)
                    })
                    .collect::<Vec<_>>();
                let callables =
                    crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
                        overloads: vec![candidate.clone()],
                    });
                let projected =
                    self.project_postponed_callables(scope, receiver, callables, &kinds);
                let parameters = match resolver.select_receiver_function_with_params(
                    receiver,
                    spelling,
                    projected.arguments(),
                    type_arguments,
                    projected.callables(),
                ) {
                    Some((_, parameters)) => parameters,
                    None => {
                        let mut shapes = resolver.receiver_function_parameter_shapes(
                            receiver,
                            projected.arguments(),
                            type_arguments,
                            projected.callables(),
                        );
                        if shapes.len() != 1 {
                            return None;
                        }
                        shapes.pop()?
                    }
                };
                let mut source_parameters = vec![Ty::obj("kotlin/Any"); arguments.len()];
                let mut assigned = vec![false; arguments.len()];
                for (parameter, source) in slots.iter().enumerate() {
                    let Some(source) = *source else {
                        continue;
                    };
                    source_parameters[source] = *parameters.get(parameter)?;
                    assigned[source] = true;
                }
                if let Some(vararg) = candidate.call_sig.vararg_index {
                    let parameter = *parameters.get(vararg)?;
                    for (source, assigned) in assigned.iter_mut().enumerate() {
                        if !*assigned {
                            source_parameters[source] = parameter;
                            *assigned = true;
                        }
                    }
                }
                crate::trace_compiler!(
                    "signature",
                    "candidate-mapped expectation {spelling} source_parameters={source_parameters:?}",
                );
                assigned
                    .iter()
                    .all(|assigned| *assigned)
                    .then_some(source_parameters)
            })
            .collect::<Option<Vec<_>>>()?;
        crate::trace_compiler!(
            "signature",
            "candidate-mapped expectation {spelling} common-shape candidates={}",
            shapes.len(),
        );
        self.common_postponed_parameters(resolver, arguments, shapes)
    }

    pub(super) fn project_postponed_callables(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        callables: crate::libraries::Callables,
        arguments: &[crate::symbol_resolver::CallArgKind],
    ) -> PostponedCallableFamily {
        let constraint_frame = self.active_scoped_constraint_frame(scope.owner);
        let mut active = std::collections::HashSet::new();
        if let Some(inputs) = constraint_frame.and_then(|(owner, index)| {
            self.scoped_constraint_inputs
                .borrow()
                .get(&owner)
                .and_then(|stack| stack.get(index))
                .cloned()
        }) {
            for input in inputs {
                collect_type_parameters(input, &mut active);
            }
        }
        if active.is_empty() {
            return PostponedCallableFamily {
                callables,
                arguments: arguments.to_vec(),
                bindings: Vec::new(),
            };
        }
        let known = constraint_frame
            .and_then(|(owner, index)| {
                self.scoped_constraints
                    .borrow()
                    .get(&owner)
                    .and_then(|stack| stack.get(index))
                    .cloned()
            })
            .unwrap_or_default();

        let (mut functions, properties) = callables.into_parts();
        let mut recorded = Vec::new();
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let arguments = arguments
            .iter()
            .map(|argument| argument.substitute_types(&known))
            .collect::<Vec<_>>();
        for candidate in &mut functions.overloads {
            let Some(mut signature) = candidate.generic_sig.clone() else {
                continue;
            };
            let mut bindings = known.clone();
            let declared_receiver = signature.receiver.or(candidate.receiver);
            if let Some(declared_receiver) = declared_receiver {
                crate::symbol_resolver::unify_inferred_ty_with_source(
                    &source,
                    declared_receiver,
                    receiver,
                    &mut bindings,
                );
            }
            let value_parameter_start = candidate.context_count.min(signature.params.len());
            for (parameter, argument) in signature.params[value_parameter_start..]
                .iter()
                .copied()
                .zip(&arguments)
            {
                if argument.is_omitted_default() || argument.is_lambda_literal() {
                    continue;
                }
                let parameter = crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings);
                crate::symbol_resolver::unify_inferred_ty_with_source(
                    &source,
                    parameter,
                    argument.type_for(parameter),
                    &mut bindings,
                );
                let constraints =
                    crate::symbol_resolver::collect_assignability_constraints_from_symbols(
                        &source,
                        parameter,
                        argument.type_for(parameter),
                    );
                merge_active_constraints(&mut bindings, constraints, &active);
            }
            if let Some(declared_receiver) = declared_receiver {
                let expected_receiver =
                    crate::symbol_resolver::ty_subst_keep_unbound(declared_receiver, &bindings);
                let constraints =
                    crate::symbol_resolver::collect_assignability_constraints_from_symbols(
                        &source,
                        expected_receiver,
                        receiver,
                    );
                merge_active_constraints(&mut bindings, constraints, &active);
            }
            let committed = bindings
                .iter()
                .filter(|(formal, _)| active.contains(formal.as_str()))
                .map(|(formal, ty)| (formal.clone(), *ty))
                .collect();

            signature.receiver = signature
                .receiver
                .map(|receiver| crate::symbol_resolver::ty_subst_keep_unbound(receiver, &bindings));
            for parameter in &mut signature.params {
                *parameter = crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings);
            }
            signature.ret = crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
            for bounds in &mut signature.formal_bounds {
                for bound in bounds {
                    *bound = crate::symbol_resolver::ty_subst_keep_unbound(*bound, &bindings);
                }
            }
            candidate.receiver = candidate
                .receiver
                .map(|receiver| crate::symbol_resolver::ty_subst_keep_unbound(receiver, &bindings));
            candidate.generic_sig = Some(signature);
            recorded.push((candidate.clone(), committed));
        }
        PostponedCallableFamily {
            callables: crate::libraries::Callables::from_parts(functions, properties),
            arguments,
            bindings: recorded,
        }
    }

    pub(super) fn commit_postponed_bindings(
        &self,
        scope: crate::fir::SignatureScope,
        bindings: crate::symbol_resolver::GSigBinds,
    ) {
        if bindings.is_empty() {
            return;
        }
        let Some((constraint_owner, constraint_index)) =
            self.active_scoped_constraint_frame(scope.owner)
        else {
            return;
        };
        let mut constraints = self.scoped_constraints.borrow_mut();
        let Some(active) = constraints
            .get_mut(&constraint_owner)
            .and_then(|stack| stack.get_mut(constraint_index))
        else {
            return;
        };
        ProductionSignatureSemantics::merge_scoped_constraints(active, bindings);
    }
}
