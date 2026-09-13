//! Call-site type-variable constraint collection, propagation, and solving.

use super::*;

pub(crate) fn infer_generic_call_constraints_from_symbols(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    infer_generic_call_constraints_with_receiver_and_argument_ordinals(
        source,
        generic_sig,
        None,
        actuals
            .into_iter()
            .enumerate()
            .map(|(argument, (parameter, actual, whole_array))| {
                (argument, parameter, actual, whole_array)
            }),
        vararg_index,
    )
}

/// Infer value-argument constraints together with the callable's semantic extension receiver.
/// Receiver evidence participates in the same declaration-bound graph but has no source-argument
/// ordinal, so it cannot be misreported as an argument mismatch.
pub(crate) fn infer_generic_call_constraints_with_receiver_from_symbols(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actual_receiver: Option<Ty>,
    actuals: impl IntoIterator<Item = (usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    infer_generic_call_constraints_with_receiver_and_argument_ordinals(
        source,
        generic_sig,
        actual_receiver,
        actuals
            .into_iter()
            .enumerate()
            .map(|(argument, (parameter, actual, whole_array))| {
                (argument, parameter, actual, whole_array)
            }),
        vararg_index,
    )
}

/// The same constraint operation with an explicit source-argument ordinal. A non-denotable
/// conditional contributes each branch type as a lower constraint while every constituent still
/// belongs to the one source argument for diagnostics.
pub(crate) fn infer_generic_call_constraints_with_argument_ordinals(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    infer_generic_call_constraints_with_receiver_and_argument_ordinals(
        source,
        generic_sig,
        None,
        actuals,
        vararg_index,
    )
}

fn infer_generic_call_constraints_with_receiver_and_argument_ordinals(
    source: &dyn SymbolSource,
    generic_sig: &GenericSig,
    actual_receiver: Option<Ty>,
    actuals: impl IntoIterator<Item = (usize, usize, Ty, bool)>,
    vararg_index: Option<usize>,
) -> InferredCallBindings {
    let mut constraints = CallInferenceConstraints::default();
    if let (Some(shape), Some(actual)) = (generic_sig.receiver, actual_receiver) {
        collect_call_inference_constraints(
            source,
            shape,
            actual,
            ConstraintPosition::Lower,
            ConstraintOrigin::Receiver,
            &mut constraints,
        );
    }
    for (argument, parameter, actual, whole_array) in actuals {
        let Some(mut shape) = generic_sig.params.get(parameter).copied() else {
            continue;
        };
        if vararg_index == Some(parameter)
            && (!whole_array || actual.non_null().array_elem().is_none())
        {
            let Some(element) = shape.array_elem() else {
                continue;
            };
            shape = element;
        }
        // Callable references and function values constrain a functional-interface parameter
        // through its single abstract method, not through their nominal `KFunctionN`/`FunctionN`
        // carrier. This belongs at the constraint seam: callers that need the full upper/lower
        // relation and callers that need only solved bindings must observe the same inference.
        if matches!(actual.non_null(), Ty::Fun(_)) {
            if let Some(sam) = semantic_sam_signature(source, shape) {
                shape = Ty::fun_with_shape(
                    sam.params,
                    sam.ret,
                    sam.context_count,
                    sam.has_receiver,
                    sam.suspend,
                );
            }
        }
        collect_call_inference_constraints(
            source,
            shape,
            actual,
            ConstraintPosition::Lower,
            ConstraintOrigin::Argument(argument),
            &mut constraints,
        );
    }
    constraints.solve(source, generic_sig)
}

#[derive(Clone, Copy)]
enum ConstraintPosition {
    Lower,
    Upper,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConstraintOrigin {
    Receiver,
    Argument(usize),
}

impl ConstraintPosition {
    fn through(self, variance: crate::types::TypeVariance) -> Self {
        match variance {
            crate::types::TypeVariance::In => match self {
                Self::Lower => Self::Upper,
                Self::Upper => Self::Lower,
            },
            crate::types::TypeVariance::Invariant | crate::types::TypeVariance::Out => self,
        }
    }
}

#[derive(Default)]
struct CallInferenceConstraints {
    lower: GSigBinds,
    lower_inputs: std::collections::HashMap<String, Vec<(Ty, ConstraintOrigin)>>,
    upper: std::collections::HashMap<String, Vec<Ty>>,
}

/// Type-variable constraints implied by one semantic assignability relation (`actual <: expected`).
/// Unlike call inference, either side may contain variables owned by an enclosing postponed call.
/// Keeping this relation source-aware is essential for applied supertypes: `Set<String>` flowing to
/// `Collection<E>` constrains `E`, and `MutableSet<E>` flowing to
/// `MutableCollection<in String>` constrains that same enclosing `E` in the opposite position.
pub(crate) struct AssignabilityConstraints {
    pub lower: GSigBinds,
    pub upper: std::collections::HashMap<String, Vec<Ty>>,
}

pub(crate) fn collect_assignability_constraints_from_symbols(
    source: &dyn SymbolSource,
    expected: Ty,
    actual: Ty,
) -> AssignabilityConstraints {
    fn actual_side(
        source: &dyn SymbolSource,
        expected: Ty,
        actual: Ty,
        position: ConstraintPosition,
        constraints: &mut CallInferenceConstraints,
    ) {
        let actual = actual.projection_inner().unwrap_or(actual);
        match actual {
            Ty::TyParam(name, _) => {
                let expected = expected.projection_inner().unwrap_or(expected);
                constraints.insert(source, name, expected, position, ConstraintOrigin::Receiver);
            }
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
                actual_side(source, expected.non_null(), *inner, position, constraints)
            }
            Ty::Fun(actual) => {
                let Ty::Fun(expected) = expected.non_null() else {
                    return;
                };
                for (&expected, &actual) in expected.params.iter().zip(&actual.params) {
                    actual_side(
                        source,
                        expected,
                        actual,
                        ConstraintPosition::Lower,
                        constraints,
                    );
                }
                actual_side(
                    source,
                    expected.ret,
                    actual.ret,
                    ConstraintPosition::Upper,
                    constraints,
                );
            }
            Ty::Obj(actual_owner, _) => {
                let Some(expected_owner) = expected.non_null().obj_internal() else {
                    return;
                };
                let applied = if actual_owner == expected_owner {
                    actual
                } else {
                    receiver_hierarchy(source, actual)
                        .into_iter()
                        .map(|(ty, _)| ty)
                        .find(|ty| ty.obj_internal() == Some(expected_owner))
                        .unwrap_or(actual)
                };
                let (Ty::Obj(owner, actual_arguments), Ty::Obj(_, expected_arguments)) =
                    (applied.non_null(), expected.non_null())
                else {
                    return;
                };
                if owner != expected_owner {
                    return;
                }
                let variances = source
                    .classifier(owner)
                    .map(|classifier| classifier.type_param_variances.clone())
                    .unwrap_or_default();
                for (index, (&expected, &actual)) in
                    expected_arguments.iter().zip(actual_arguments).enumerate()
                {
                    let (expected, position) = match expected {
                        Ty::InProjection(inner) => (*inner, ConstraintPosition::Lower),
                        Ty::OutProjection(inner) => (*inner, ConstraintPosition::Upper),
                        Ty::StarProjection(_) => continue,
                        _ => {
                            let position = match variances
                                .get(index)
                                .copied()
                                .unwrap_or(crate::types::TypeVariance::Invariant)
                            {
                                crate::types::TypeVariance::In => ConstraintPosition::Lower,
                                crate::types::TypeVariance::Out => ConstraintPosition::Upper,
                                // Equality supplies a usable lower solution; ordinary
                                // applicability still enforces the invariant relation.
                                crate::types::TypeVariance::Invariant => ConstraintPosition::Lower,
                            };
                            (expected, position)
                        }
                    };
                    actual_side(source, expected, actual, position, constraints);
                }
            }
            _ => {}
        }
    }

    let mut constraints = CallInferenceConstraints::default();
    collect_call_inference_constraints(
        source,
        expected,
        actual,
        ConstraintPosition::Lower,
        ConstraintOrigin::Receiver,
        &mut constraints,
    );
    actual_side(
        source,
        expected,
        actual,
        ConstraintPosition::Upper,
        &mut constraints,
    );
    AssignabilityConstraints {
        lower: constraints.lower,
        upper: constraints.upper,
    }
}

pub(crate) struct InferredCallBindings {
    pub bindings: GSigBinds,
    /// Every lower constraint before it is approximated to one denotable binding. Callers that
    /// model Kotlin's non-denotable intersection result consume these only after candidate
    /// selection; ordinary applicability continues to use `bindings`.
    pub lower_inputs: std::collections::HashMap<String, Vec<Ty>>,
    pub upper_only: std::collections::HashSet<String>,
    pub upper_bounds: std::collections::HashMap<String, Vec<Ty>>,
    pub bound_violation: Option<GenericBoundViolation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GenericBoundViolation {
    /// Ordinal in the mapped actual-argument stream supplied to constraint collection.
    pub argument: usize,
    pub expected: Ty,
    pub actual: Ty,
}

impl InferredCallBindings {
    /// Concrete solutions supplied solely by contravariant/input positions. A solution is
    /// publishable only when one observed upper is below every other observed upper; otherwise the
    /// constraint remains open and its caller may apply the language's bottom/default policy.
    pub(crate) fn tightest_upper_bindings(&self, source: &dyn SymbolSource) -> GSigBinds {
        self.upper_only
            .iter()
            .filter_map(|formal| {
                let upper_bounds = self.upper_bounds.get(formal)?;
                let selected = upper_bounds.iter().copied().find(|candidate| {
                    upper_bounds
                        .iter()
                        .all(|upper| resolution_subtype(source, *candidate, *upper))
                })?;
                Some((formal.clone(), selected))
            })
            .collect()
    }
}

impl CallInferenceConstraints {
    fn insert(
        &mut self,
        source: &dyn SymbolSource,
        name: &str,
        actual: Ty,
        position: ConstraintPosition,
        origin: ConstraintOrigin,
    ) {
        if actual.mentions_error() {
            return;
        }
        let actual = inference_actual(actual);
        if matches!(actual, Ty::TyParam(actual_name, _) if actual_name == name) {
            return;
        }
        match position {
            ConstraintPosition::Lower => {
                let inputs = self.lower_inputs.entry(name.to_string()).or_default();
                if !inputs.contains(&(actual, origin)) {
                    inputs.push((actual, origin));
                }
                // A formal's FIRST lower constraint keeps a use-site projection intact (`B<*>`
                // against `B<T>` binds `T := out Any?` — the stand-in for kotlinc's captured type):
                // the parameter then substitutes back to the argument's own type, so the call stays
                // applicable, while the VALUE read out of it is approximated where it is recorded.
                // A second, concrete constraint merges as before, which strips the projection and
                // widens — an invariant occurrence then rejects the write, exactly as kotlinc does.
                let merged = match self.lower.get(name).copied() {
                    None => actual,
                    Some(current) => merge_inferred_ty_from_symbols(Some(source), current, actual),
                };
                self.lower.insert(name.to_string(), merged);
            }
            ConstraintPosition::Upper => {
                let bounds = self.upper.entry(name.to_string()).or_default();
                if !bounds.contains(&actual) {
                    bounds.push(actual);
                }
            }
        }
    }

    /// Propagate the declaration's direct variable bounds as constraints before collapsing them to
    /// one denotable solution. For `T : R` (`T <: R`), every lower bound of `T` is a lower bound of
    /// `R`, while every upper bound of `R` is an upper bound of `T`.
    fn propagate_declaration_bounds(&mut self, source: &dyn SymbolSource, signature: &GenericSig) {
        loop {
            let mut changed = false;
            let lower = self.lower_inputs.clone();
            let upper = self.upper.clone();
            for (formal, bounds) in signature.formals.iter().zip(&signature.formal_bounds) {
                for bound in bounds {
                    let Ty::TyParam(bound_formal, _) = bound.non_null() else {
                        continue;
                    };
                    if !signature
                        .formals
                        .iter()
                        .any(|candidate| candidate == bound_formal)
                    {
                        continue;
                    }
                    for &(actual, origin) in
                        lower.get(formal).map(Vec::as_slice).unwrap_or_default()
                    {
                        let prior = self
                            .lower_inputs
                            .get(bound_formal)
                            .is_some_and(|inputs| inputs.contains(&(actual, origin)));
                        self.insert(
                            source,
                            bound_formal,
                            actual,
                            ConstraintPosition::Lower,
                            origin,
                        );
                        changed |= !prior;
                    }
                    for &actual in upper
                        .get(bound_formal)
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                    {
                        let prior = self
                            .upper
                            .get(formal)
                            .is_some_and(|bounds| bounds.contains(&actual));
                        self.insert(
                            source,
                            formal,
                            actual,
                            ConstraintPosition::Upper,
                            ConstraintOrigin::Receiver,
                        );
                        changed |= !prior;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn solve(mut self, source: &dyn SymbolSource, signature: &GenericSig) -> InferredCallBindings {
        self.propagate_declaration_bounds(source, signature);
        let mut bound_violation = None;
        // Lower bounds and the declaration's upper bounds are one constraint system. A raw join may
        // be wider than the declared bound (`Int` + `Double` joins to `Any` in the nominal fallback,
        // while `<T : Number>` has the valid and more precise solution `Number`). Retain every lower
        // input until this point and select a declared bound only when it admits them all and the
        // complete declared-bound graph accepts that substitution.
        for (index, formal) in signature.formals.iter().enumerate() {
            let Some(current) = self.lower.get(formal).copied() else {
                continue;
            };
            let bounds = signature
                .formal_bounds
                .get(index)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if bounds.is_empty()
                || bounds.iter().all(|bound| {
                    resolution_subtype(source, current, ty_subst_keep_unbound(*bound, &self.lower))
                })
            {
                continue;
            }
            let lowers = self
                .lower_inputs
                .get(formal)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut candidates = bounds
                .iter()
                .map(|bound| ty_subst_keep_unbound(*bound, &self.lower))
                .filter(|candidate| !candidate.mentions_ty_param())
                .filter(|candidate| {
                    lowers
                        .iter()
                        .all(|(actual, _)| resolution_subtype(source, *actual, *candidate))
                })
                .filter(|candidate| {
                    let mut trial = self.lower.clone();
                    trial.insert(formal.clone(), *candidate);
                    bounds.iter().all(|bound| {
                        resolution_subtype(
                            source,
                            *candidate,
                            ty_subst_keep_unbound(*bound, &trial),
                        )
                    })
                })
                .collect::<Vec<_>>();
            candidates.dedup();
            if let [candidate] = candidates.as_slice() {
                self.lower.insert(formal.clone(), *candidate);
                continue;
            }
            let mut prior = None;
            for &(actual, origin) in lowers {
                let trial = prior
                    .map(|known| merge_inferred_ty_from_symbols(Some(source), known, actual))
                    .unwrap_or(actual);
                let mut trial_bindings = self.lower.clone();
                trial_bindings.insert(formal.clone(), trial);
                let within_bounds = bounds.iter().all(|bound| {
                    resolution_subtype(
                        source,
                        actual,
                        ty_subst_keep_unbound(*bound, &trial_bindings),
                    )
                });
                if !within_bounds {
                    let ConstraintOrigin::Argument(argument) = origin else {
                        prior = Some(trial);
                        continue;
                    };
                    let expected = prior.unwrap_or_else(|| {
                        bounds
                            .first()
                            .copied()
                            .map(|bound| ty_subst_keep_unbound(bound, &trial_bindings))
                            .unwrap_or(current)
                    });
                    let violation = GenericBoundViolation {
                        argument,
                        expected,
                        actual,
                    };
                    if bound_violation
                        .is_none_or(|known: GenericBoundViolation| argument < known.argument)
                    {
                        bound_violation = Some(violation);
                    }
                    break;
                }
                prior = Some(trial);
            }
        }
        let upper_only = self
            .upper
            .iter()
            .filter_map(|(formal, upper)| {
                (!self.lower.contains_key(formal)
                    && upper.iter().any(|constraint| *constraint != Ty::Error))
                .then_some(formal.clone())
            })
            .collect();
        InferredCallBindings {
            bindings: self.lower,
            lower_inputs: self
                .lower_inputs
                .into_iter()
                .map(|(formal, inputs)| {
                    (
                        formal,
                        inputs.into_iter().map(|(actual, _)| actual).collect(),
                    )
                })
                .collect(),
            upper_only,
            upper_bounds: self.upper,
            bound_violation,
        }
    }
}

fn collect_call_inference_constraints(
    source: &dyn SymbolSource,
    shape: Ty,
    actual: Ty,
    position: ConstraintPosition,
    origin: ConstraintOrigin,
    constraints: &mut CallInferenceConstraints,
) {
    match shape {
        Ty::TyParam(name, _) => constraints.insert(source, name, actual, position, origin),
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => {
            collect_call_inference_constraints(
                source,
                *inner,
                nullable_generic_actual(actual),
                position,
                origin,
                constraints,
            );
        }
        Ty::InProjection(inner) => collect_call_inference_constraints(
            source,
            *inner,
            match actual {
                Ty::OutProjection(_) | Ty::StarProjection(_) => Ty::Nothing,
                _ => actual.projection_inner().unwrap_or(actual),
            },
            position.through(crate::types::TypeVariance::In),
            origin,
            constraints,
        ),
        Ty::OutProjection(inner) => collect_call_inference_constraints(
            source,
            *inner,
            actual.projection_inner().unwrap_or(actual),
            position,
            origin,
            constraints,
        ),
        Ty::StarProjection(_) => {}
        Ty::Fun(function) => {
            let Ty::Fun(actual) = actual.non_null() else {
                return;
            };
            for (&parameter, &actual) in function.params.iter().zip(&actual.params) {
                collect_call_inference_constraints(
                    source,
                    parameter,
                    actual,
                    position.through(crate::types::TypeVariance::In),
                    origin,
                    constraints,
                );
            }
            collect_call_inference_constraints(
                source,
                function.ret,
                actual.ret,
                position,
                origin,
                constraints,
            );
        }
        Ty::Obj(owner, arguments) => {
            let mut actual = actual.projection_inner().unwrap_or(actual).non_null();
            // A caller-owned type parameter contributes the applied shape of its upper bound to a
            // classifier-shaped constraint. Treating the variable itself as the callee's element
            // argument makes `T : Iterable<*>` bind `Iterable<E>.withIndex` as `E = T`; project
            // through the bound so it correctly binds `E` to the star's readable upper type.
            while let Ty::TyParam(_, bound) = actual {
                actual = bound.projection_inner().unwrap_or(*bound).non_null();
            }
            let projected = match actual {
                Ty::Obj(actual_owner, _) if owner == actual_owner => Some(actual),
                Ty::Obj(_, _) => receiver_hierarchy(source, actual)
                    .into_iter()
                    .map(|(ty, _)| ty)
                    .find(|ty| {
                        ty.obj_internal()
                            .is_some_and(|actual_owner| owner == actual_owner)
                    }),
                _ => None,
            };
            let Some(Ty::Obj(_, actual_arguments)) = projected else {
                return;
            };
            let classifier = source.classifier(owner);
            let variances = classifier
                .as_ref()
                .map(|classifier| classifier.type_param_variances.clone())
                .unwrap_or_default();
            for (index, (&argument, &actual)) in arguments.iter().zip(actual_arguments).enumerate()
            {
                let declared_variance = variances
                    .get(index)
                    .copied()
                    .unwrap_or(crate::types::TypeVariance::Invariant);
                // A matching use-site projection is redundant on a variant declaration. `A<*>`
                // for `A<out T : Any>` exposes the readable upper `Any`; retaining the star as a
                // call-formal binding would later instantiate `A<E>` in input position as
                // `A<Nothing>` and reject the very argument that supplied the capture. Invariant
                // classifiers must keep the projection because their write side really is closed.
                let actual = match (declared_variance, actual) {
                    (crate::types::TypeVariance::Out, Ty::OutProjection(inner))
                    | (crate::types::TypeVariance::Out, Ty::StarProjection(inner))
                    | (crate::types::TypeVariance::In, Ty::InProjection(inner)) => *inner,
                    _ => actual,
                };
                let actual = if matches!(argument, Ty::OutProjection(_))
                    && matches!(actual, Ty::InProjection(_))
                {
                    let upper = classifier
                        .as_ref()
                        .and_then(|classifier| classifier.type_param_bounds.get(index))
                        .and_then(|bounds| bounds.first())
                        .copied()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                    crate::trace_compiler!(
                        "resolve",
                        "projected inference owner={owner:?} index={index} shape={argument:?} actual={actual:?} readable={upper:?}"
                    );
                    upper
                } else {
                    actual
                };
                // An invariant classifier argument is an equality constraint even when the
                // classifier itself occurs contravariantly. For `(Box<T>) -> Unit` matched against
                // `(Box<Value>) -> Unit`, `T` is `Value`, not an upper-only variable defaulting to
                // `Nothing`. Use-site projections still carry their own direction in `argument`.
                let argument_position = match declared_variance {
                    crate::types::TypeVariance::Invariant => ConstraintPosition::Lower,
                    variance => position.through(variance),
                };
                collect_call_inference_constraints(
                    source,
                    argument,
                    actual,
                    argument_position,
                    origin,
                    constraints,
                );
            }
        }
        _ => {}
    }
}
