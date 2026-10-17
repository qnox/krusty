//! Checked intersection facts retained across generic-call commitment.
//!
//! Ordinary expression types keep a denotable runtime view. Generic inference can additionally
//! prove that the same value inhabits several unrelated classifier views; reified diagnostics and
//! a surrounding postponed call need that complete checked fact without reconstructing it from
//! source spelling or provider origin.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct InferredIntersection {
    /// The exact committed expression type to which this fact belongs.
    pub(super) selected_result: Ty,
    /// Complete, ordered semantic constituents. The first is the inference primary; FIR stores the
    /// remainder as additional bounds.
    pub(super) parts: Vec<Ty>,
    /// Whether the fact crossed a flexible platform-type boundary.
    pub(super) through_flexible: bool,
}

impl Checker<'_> {
    /// Publish the reified formal identities carried by one normalized selected candidate. The
    /// diagnostic post-check reads this candidate-shaped fact rather than branching on the
    /// storage representation of a top-level, member, extension, or dependency callable.
    pub(super) fn publish_selected_call_reified_type_parameters(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
    ) {
        let parameters = call_sig
            .reified_type_parameter_ordinals
            .iter()
            .filter_map(|&ordinal| {
                let ordinal = usize::try_from(ordinal).ok()?;
                Some((ordinal, signature.formals.get(ordinal)?.clone()))
            })
            .collect::<Vec<_>>();
        if parameters.is_empty() {
            self.selected_call_reified_type_parameters.remove(&call);
        } else {
            self.selected_call_reified_type_parameters
                .insert(call, parameters);
        }
    }

    pub(super) fn clear_call_intersection_facts(&mut self, call: ExprId) {
        self.inferred_expression_intersections.remove(&call);
        self.resolved_call_type_argument_bounds.remove(&call);
        self.resolved_call_type_argument_intersections
            .retain(|(expression, _), _| *expression != call);
        self.selected_call_reified_type_parameters.remove(&call);
    }

    pub(super) fn inferred_expression_intersection(&self, expression: ExprId) -> Vec<Ty> {
        self.inferred_expression_intersection_fact(expression)
            .map(|fact| fact.parts)
            .unwrap_or_default()
    }

    fn inferred_expression_intersection_fact(
        &self,
        expression: ExprId,
    ) -> Option<InferredIntersection> {
        self.inferred_expression_intersections
            .get(&expression)
            .cloned()
    }

    pub(super) fn publish_conditional_intersection(
        &mut self,
        expression: ExprId,
        left: ExprId,
        right: ExprId,
        selected_result: Ty,
    ) {
        let parts = self.minimal_common_classifier_views(
            self.expr_types[left.0 as usize],
            self.expr_types[right.0 as usize],
        );
        if parts.len() > 1 {
            self.inferred_expression_intersections.insert(
                expression,
                InferredIntersection {
                    selected_result,
                    parts,
                    through_flexible: false,
                },
            );
        } else {
            self.inferred_expression_intersections.remove(&expression);
        }
    }

    /// Retain a finalized postponed argument's intersection only when that fact remains the exact
    /// solution of a direct formal and every other direct argument proves every constituent.
    /// Successful propagation normalizes the formal to the first constituent, matching FIR's
    /// primary/additional-bound contract.
    pub(super) fn retain_postponed_argument_intersections(
        &self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        explicit_type_args: &[Ty],
        parameter_indices: &[usize],
        call_sig: &CallSig,
        selected_bindings: &mut crate::symbol_resolver::GSigBinds,
        intersection_bindings: &mut HashMap<String, Vec<Ty>>,
    ) -> HashMap<String, InferredIntersection> {
        let mut propagated = HashMap::default();
        let Some(slots) = self.resolved_call_arg_slots.get(&call) else {
            return propagated;
        };
        for (formal_index, formal) in signature.formals.iter().enumerate() {
            if explicit_type_args
                .get(formal_index)
                .is_some_and(|argument| *argument != Ty::Error)
            {
                continue;
            }
            let Some(&selected_argument) = selected_bindings.get(formal) else {
                continue;
            };
            let direct_arguments = slots
                .iter()
                .enumerate()
                .filter_map(|(visible_parameter, argument)| {
                    let argument = (*argument)?;
                    let parameter = *parameter_indices.get(visible_parameter)?;
                    if call_sig.vararg_index == Some(visible_parameter)
                        || call_sig
                            .exact_params
                            .get(visible_parameter)
                            .copied()
                            .unwrap_or(false)
                        || signature
                            .params
                            .get(parameter)
                            .copied()
                            .map(Ty::non_null)
                            .and_then(Ty::ty_param_name)
                            != Some(formal.as_str())
                    {
                        return None;
                    }
                    Some(argument)
                })
                .collect::<Vec<_>>();
            for &argument in &direct_arguments {
                let Some(fact) = self.inferred_expression_intersection_fact(argument) else {
                    continue;
                };
                // A flexible producer is committed to a non-null generic parameter through its
                // lower bound. That is the same selected value, not a widening: retain the
                // boundary provenance while normalizing the call's denotable type argument.
                let selected_same_value = fact.selected_result == selected_argument
                    || (fact.through_flexible
                        && fact.selected_result.non_null() == selected_argument);
                if fact.parts.len() < 2 || !selected_same_value {
                    continue;
                }
                let all_arguments_prove_parts = direct_arguments.iter().all(|&other| {
                    let actual = self.expr_types[other.0 as usize];
                    let other_fact = self.inferred_expression_intersection_fact(other);
                    fact.parts.iter().all(|&part| {
                        self.receiver_is_assignable(actual, part)
                            || other_fact.as_ref().is_some_and(|other| {
                                other
                                    .parts
                                    .iter()
                                    .copied()
                                    .any(|view| self.receiver_is_assignable(view, part))
                            })
                    })
                });
                if !all_arguments_prove_parts {
                    continue;
                }
                selected_bindings.insert(formal.clone(), fact.parts[0]);
                intersection_bindings.insert(formal.clone(), fact.parts.clone());
                propagated.insert(formal.clone(), fact);
                break;
            }
        }
        propagated
    }

    /// Commit the type arguments and intersection provenance of an already-selected generic call.
    /// Every call representation uses this same semantic boundary; whether lowering records the
    /// declaration as top-level, static, or a member must not change inference.
    pub(super) fn commit_selected_call_intersections(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        explicit_type_args: &[Ty],
        parameter_indices: &[usize],
        call_sig: &CallSig,
        selected_bindings: &mut crate::symbol_resolver::GSigBinds,
        intersection_bindings: &mut HashMap<String, Vec<Ty>>,
        return_info: crate::libraries::ReturnInfo,
        mut final_result: Ty,
    ) -> Ty {
        let propagated = self.retain_postponed_argument_intersections(
            call,
            signature,
            explicit_type_args,
            parameter_indices,
            call_sig,
            selected_bindings,
            intersection_bindings,
        );
        if !propagated.is_empty() {
            let inferred_return =
                crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, selected_bindings);
            final_result =
                return_info.apply(signature.apply_return_policy(self.libraries, inferred_return));
        }
        let resolved = signature
            .formals
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                explicit_type_args
                    .get(index)
                    .copied()
                    .filter(|argument| *argument != Ty::Error)
                    .or_else(|| selected_bindings.get(formal).copied())
            })
            .collect::<Vec<_>>();
        if resolved.iter().any(Option::is_some) {
            self.resolved_call_type_args.insert(call, resolved);
        } else {
            // Expected-type rechecking may revisit the same parser call after an earlier
            // contextual pass had solved it. This selection is the authoritative replacement.
            self.resolved_call_type_args.remove(&call);
        }
        self.publish_selected_call_intersections(
            call,
            signature,
            call_sig,
            selected_bindings,
            intersection_bindings,
            &propagated,
            final_result,
        );
        final_result
    }

    /// Publish one selected call's exact intersection facts after its final result policy and type
    /// arguments have committed. Rechecks replace all prior facts for this call.
    pub(super) fn publish_selected_call_intersections(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
        selected_bindings: &crate::symbol_resolver::GSigBinds,
        intersection_bindings: &HashMap<String, Vec<Ty>>,
        propagated: &HashMap<String, InferredIntersection>,
        final_result: Ty,
    ) {
        self.clear_call_intersection_facts(call);
        self.publish_selected_call_reified_type_parameters(call, signature, call_sig);
        let mut additional_bounds = vec![Vec::new(); signature.formals.len()];
        for (ordinal, formal) in signature.formals.iter().enumerate() {
            let Some(parts) = intersection_bindings
                .get(formal)
                .filter(|parts| parts.len() > 1)
            else {
                continue;
            };
            let selected_result = selected_bindings.get(formal).copied().unwrap_or(parts[0]);
            let through_flexible = propagated
                .get(formal)
                .is_some_and(|fact| fact.through_flexible);
            self.resolved_call_type_argument_intersections.insert(
                (call, ordinal),
                InferredIntersection {
                    selected_result,
                    parts: parts.clone(),
                    through_flexible,
                },
            );
            additional_bounds[ordinal].extend(parts.iter().copied().skip(1));
        }
        if additional_bounds.iter().any(|bounds| !bounds.is_empty()) {
            self.resolved_call_type_argument_bounds
                .insert(call, additional_bounds);
        } else {
            self.resolved_call_type_argument_bounds.remove(&call);
        }

        let result_formal = match signature.ret {
            Ty::TyParam(formal, _) => Some(formal),
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => inner.ty_param_name(),
            _ => None,
        };
        let Some(formal) = result_formal else {
            return;
        };
        let Some(parts) = intersection_bindings
            .get(formal)
            .filter(|parts| parts.len() > 1)
        else {
            return;
        };
        self.inferred_expression_intersections.insert(
            call,
            InferredIntersection {
                selected_result: final_result,
                parts: parts.clone(),
                through_flexible: matches!(final_result, Ty::PlatformNullable(_))
                    || propagated
                        .get(formal)
                        .is_some_and(|fact| fact.through_flexible),
            },
        );
    }
}
