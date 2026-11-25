//! Selection of the receiver-less function a callable reference names (`::name`) against its
//! expected function type: top-level functions, or a classifier's associated functions. The
//! candidates already combine scope and symbol-source declarations; overload selection is the
//! shared reference-selection rule, and the context values the reference binds are selected with
//! the same operation that binds them later.

use super::*;

impl<'a> Checker<'a> {
    /// Select a top-level callable reference from its expected function type. Calls have argument
    /// expressions for overload resolution; `::name` instead uses the expected parameter and return
    /// shape. The candidates already combine scope and symbol-source declarations; origin only decides
    /// the selected target's emit owner after overload selection.
    pub(super) fn selected_top_level_ref(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        expected: &'static crate::types::FnSig,
    ) -> Option<Ty> {
        let candidates = self.resolver().top_level_candidates(name);
        self.selected_receiverless_function_ref(scope, expression, name, expected, candidates)
    }

    /// Select, against `expected`, the receiver-less function a callable reference names among
    /// `candidates`: top-level functions, or a classifier's associated functions.
    pub(super) fn selected_receiverless_function_ref(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        expected: &'static crate::types::FnSig,
        candidates: Vec<crate::libraries::FunctionInfo>,
    ) -> Option<Ty> {
        let mut inaccessible = false;
        let mut structurally_adaptable = Vec::new();
        let mut unavailable_default = None;
        let mut unresolved_type_arguments = false;
        let mut context_parameter_candidate = None;
        let mut candidates = candidates
            .into_iter()
            .filter_map(|mut function| {
                if function.kind != crate::libraries::FnKind::TopLevel {
                    return None;
                }
                crate::trace_compiler!(
                    "resolve",
                    "top-level reference candidate name={name} params={:?} ret={:?} generic={:?} visibility={:?} owner={}",
                    function.callable.params,
                    function.callable.ret,
                    function.generic_sig,
                    function.visibility,
                    function.callable.owner.render(),
                );
                // A regular function can adapt to a suspend function type; the reverse conversion is
                // invalid. Suspension is a property of the selected declaration/signature, never
                // inferred from a classifier name.
                if function.callable.suspend && !expected.suspend {
                    return None;
                }
                // Without `CallableReferencesToContextual` a declaration with context parameters
                // cannot be referenced at all.
                if function.context_count > 0 && !self.file.language.callable_references_to_contextual {
                    context_parameter_candidate.get_or_insert_with(|| function.clone());
                    return None;
                }
                let accessible = crate::callable_access::source_callable_accessible(
                    function.visibility,
                    function.source_file,
                    Some(self.file_index),
                    || self.libraries.dependency_access_policy().internal_callable_accessible(
                        function.callable.external_identity,
                        function.callable.owner,
                    ),
                );
                if !accessible {
                    inaccessible = true;
                    return None;
                }
                let supplied = expected.params.len();
                let declared = function.callable.params.len();
                let vararg = function.call_sig.vararg_index;
                let structurally_fits = supplied <= declared
                    && (supplied..declared).all(|parameter| {
                        function.call_sig.param_has_default(parameter)
                            || vararg == Some(parameter)
                    });
                if structurally_fits {
                    structurally_adaptable.push(function.clone());
                }
                // Overload specificity compares the declarations, not their instantiation for this
                // expected shape: `listOf(vararg elements: T)` takes `Array<out T>` where
                // `listOf(element: T)` takes any `T`, whatever the expected array type binds.
                let declared_shape = callable_reference_selection::erased_declared_parameters(
                    &function.callable.params,
                );
                let context_arguments = self.reference_context_argument_types(scope, &function);
                let specialization = self.specialize_callable_ref_candidate(
                    &mut function,
                    None,
                    Some(&expected.params),
                    Some(expected.ret),
                    context_arguments.as_deref(),
                );
                if matches!(
                    specialization,
                    Err(CallableRefSpecialization::UnresolvedTypeArguments)
                ) {
                    unresolved_type_arguments = true;
                }
                let type_arguments = specialization.ok()?;
                let plan = self.callable_ref_argument_plan(&function, expected)?;
                // A stable module identity owns its checked default expressions. The literal
                // payload is required only for the identity-less cross-file source path.
                if function.stable_declaration.is_none()
                    && function
                        .source_file
                        .is_some_and(|source_file| source_file != self.file_index)
                {
                    if let Some(parameter) = plan.iter().enumerate().find_map(
                        |(parameter, argument)| {
                            matches!(argument, AdaptedRefArgument::Default)
                                .then_some(parameter)
                                .filter(|&parameter| {
                                    let semantic = function.callable.params[parameter];
                                    !function
                                        .default_values
                                        .get(parameter)
                                        .and_then(Option::as_ref)
                                        .is_some_and(|value| value.fills_param_ty(semantic))
                                })
                        },
                    ) {
                        unavailable_default.get_or_insert((function.clone(), parameter));
                        return None;
                    }
                }
                let TyKind::Fun(adapted_expected) =
                    callable_reference_selection::realize_expected_shape(
                        expected,
                        &function.callable.params,
                        function.callable.ret,
                        &plan,
                    ).kind()
                else {
                    unreachable!()
                };
                if !self.callable_ref_is_compatible(
                    &adapted_expected.params,
                    function.callable.ret,
                    expected.suspend,
                    adapted_expected,
                    true,
                ) {
                    return None;
                }
                let direct_prefix = plan.iter().enumerate().all(|(target, argument)| match argument {
                    AdaptedRefArgument::Value(value) => *value == target,
                    AdaptedRefArgument::Vararg {
                        values,
                        whole_array: true,
                    } => values.as_slice() == [target],
                    AdaptedRefArgument::Context
                    | AdaptedRefArgument::Default
                    | AdaptedRefArgument::Vararg {
                        whole_array: false,
                        ..
                    } => false,
                });
                let vararg_collect = vararg.and_then(|index| {
                    (index + 1 == declared
                        && plan.get(index).is_some_and(|argument| {
                            matches!(argument, AdaptedRefArgument::Vararg { values, whole_array: false }
                                if !values.is_empty())
                        })
                        && plan[..index].iter().enumerate().all(|(target, argument)| {
                            matches!(argument, AdaptedRefArgument::Value(value) if *value == target)
                        }))
                    .then_some(index)
                });
                let legacy_trailing_omission = plan.iter().enumerate().all(|(target, argument)| {
                    if target < supplied {
                        matches!(argument, AdaptedRefArgument::Value(value) if *value == target)
                    } else {
                        match argument {
                            AdaptedRefArgument::Default => true,
                            AdaptedRefArgument::Vararg {
                                values,
                                whole_array: false,
                            } => values.is_empty(),
                            _ => false,
                        }
                    }
                });
                let argument_mapping = (!direct_prefix
                    && vararg_collect.is_none()
                    && !legacy_trailing_omission)
                    .then(|| plan.clone());
                let needs_adapter = supplied != declared
                    || vararg_collect.is_some()
                    || argument_mapping.is_some()
                    || function.callable.suspend != expected.suspend
                    || (expected.ret == Ty::Unit && function.callable.ret != Ty::Unit);
                Some((
                    function,
                    adapted_expected,
                    needs_adapter,
                    vararg_collect,
                    argument_mapping,
                    type_arguments,
                    plan,
                    declared_shape,
                ))
            })
            .collect::<Vec<_>>();
        crate::symbol_resolver::retain_nearest_scope_level(&mut candidates, |c| {
            crate::symbol_resolver::scope_level(&c.0)
        });
        if candidates.is_empty() {
            if let Some(function) = context_parameter_candidate {
                return Some(self.report_context_parameter_reference(expression, name, &function));
            }
            if unresolved_type_arguments {
                self.diags.error(
                    self.member_name_span(expression, name),
                    format!("not enough information to infer type variable for callable reference '{name}'"),
                );
                return Some(Ty::Error);
            }
            if inaccessible {
                self.diags.error(
                    self.member_name_span(expression, name),
                    format!("cannot access '{name}': it is private in its file"),
                );
                return Some(Ty::Error);
            }
            if let Some((function, parameter)) = unavailable_default {
                let parameter = function
                    .call_sig
                    .param_names
                    .get(parameter)
                    .filter(|name| !name.is_empty())
                    .map_or("value", String::as_str);
                self.diags.error(
                    self.member_name_span(expression, name),
                    format!(
                        "cannot adapt cross-file reference '{name}': default value for parameter \
                         '{parameter}' is not available at this call site"
                    ),
                );
                return Some(Ty::Error);
            }
            if let Some(candidate) = structurally_adaptable.first() {
                // A callable reference checked against an outer call's still-symbolic function type
                // cannot decide that outer call's inference failure. Leave the reference postponed;
                // the outer generic call reports the uninferred parameter at its own call site.
                let expected_is_symbolic = expected
                    .params
                    .iter()
                    .chain(std::iter::once(&expected.ret))
                    .any(|ty| ty.non_null().is_ty_param());
                if expected_is_symbolic {
                    return None;
                }
                if self.active_postponed_callable_reference_recheck != Some(expression) {
                    let display = self
                        .source_callable_display(candidate)
                        .unwrap_or_else(|| Self::callable_candidate_display(name, candidate));
                    self.diags.error(
                        self.member_name_span(expression, name),
                        format!("inapplicable candidate(s): {display}"),
                    );
                }
                return Some(Ty::Error);
            }
        }
        let specificity = candidates
            .iter()
            .map(
                |candidate| callable_reference_selection::AdaptedReferenceSpecificity {
                    parameters: &candidate.7,
                    plan: &candidate.6,
                    is_vararg: candidate.0.call_sig.vararg_index.is_some(),
                },
            )
            .collect::<Vec<_>>();
        let maximal = callable_reference_selection::maximal_adapted_references(
            &specificity,
            |left_params, right_params| {
                self.callable_ref_parameter_shape_at_least_as_specific(left_params, right_params)
            },
        );
        let [selected_index] = maximal.as_slice() else {
            if maximal.is_empty() {
                return None;
            }
            let mut message = "overload resolution ambiguity between candidates:".to_string();
            for index in maximal {
                message.push('\n');
                message.push_str(&Self::callable_candidate_display(
                    name,
                    &candidates[index].0,
                ));
            }
            self.diags
                .error(self.member_name_span(expression, name), message);
            return Some(Ty::Error);
        };
        let (
            selected,
            adapted_expected,
            needs_adapter,
            vararg_collect,
            argument_mapping,
            type_arguments,
            _,
            _,
        ) = &candidates[*selected_index];
        self.replace_call_type_arguments(
            expression,
            type_arguments.iter().copied().map(Some).collect(),
        );
        if callable_reference_publication::lacks_reference_realization(selected) {
            self.expr_lowers.insert(
                expression,
                ExprLowering::UnavailableCallableReference {
                    signature: Ty::from(TyKind::Fun(adapted_expected)),
                    reason: "selected callable reference has no backend realization",
                },
            );
            return Some(Ty::from(TyKind::Fun(adapted_expected)));
        }
        if *needs_adapter {
            return Some(self.record_adapted_top_level_function_ref(
                expression,
                name,
                selected,
                adapted_expected,
                *vararg_collect,
                argument_mapping.clone(),
            ));
        }
        let ty = if adapted_expected.suspend {
            Ty::fun_suspend(adapted_expected.params.clone(), adapted_expected.ret)
        } else {
            Ty::fun(adapted_expected.params.clone(), adapted_expected.ret)
        };
        Some(self.record_top_level_function_ref(expression, name, selected, ty))
    }
}
