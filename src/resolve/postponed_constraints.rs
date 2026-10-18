//! The constraints a postponed call's lambdas contribute to its type variables while they are
//! analyzed before the call is solved (kotlinc's PCLA).

use super::*;

pub(super) type PostponedLambdaTypes = (Vec<Ty>, Option<Ty>, Option<Ty>, Option<Ty>, bool);

pub(super) struct PostponedRefinementSite<'a, 'scope> {
    pub(super) scope: &'a CheckerScope<'scope>,
    pub(super) call_name: &'a str,
    pub(super) call_label: Option<&'a str>,
    pub(super) arguments: &'a [ExprId],
    pub(super) selected_shape: Option<&'a crate::symbol_resolver::LambdaCallShape>,
    pub(super) known_signature: Option<&'a Signature>,
    pub(super) parameter_by_argument: Option<&'a [usize]>,
}

#[derive(Clone, Default)]
pub(super) struct PostponedCallConstraints {
    pub(super) formals: Vec<String>,
    pub(super) lower: crate::symbol_resolver::GSigBinds,
    lower_inputs: HashMap<String, Vec<Ty>>,
    pub(super) upper: HashMap<String, Vec<Ty>>,
    upper_inputs: HashMap<String, Vec<Ty>>,
    /// Result constraints belong to the postponed function argument that produced them. Refinement
    /// rechecks replace that argument's provisional contribution without weakening independent
    /// constraints from the call or another argument.
    function_result_lower: HashMap<ExprId, crate::symbol_resolver::GSigBinds>,
    function_result_upper: HashMap<ExprId, HashMap<String, Vec<Ty>>>,
    variables: crate::symbol_resolver::CallSiteVariableMap,
    /// Diagnostics whose validity depends on the call solution. A lambda parameter still expressed
    /// as the callee's type variable cannot answer member lookup yet; the finalized recheck either
    /// resolves the expression or emits the error from its concrete receiver.
    pub(super) deferred_member_errors: Vec<(ExprId, Span, String)>,
}

impl PostponedCallConstraints {
    pub(super) fn for_call(compilation: u64, file: u32, call: ExprId, formals: &[String]) -> Self {
        let variables = crate::symbol_resolver::CallSiteVariableMap::for_postponed_call(
            formals,
            compilation,
            file,
            call.0,
        );
        Self {
            formals: formals
                .iter()
                .map(|formal| variables.instantiate_formal(formal))
                .collect(),
            variables,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn for_formals(formals: &[String], lexically_visible: impl Fn(&str) -> bool) -> Self {
        let variables =
            crate::symbol_resolver::CallSiteVariableMap::new(formals, lexically_visible);
        Self {
            formals: formals
                .iter()
                .map(|formal| variables.instantiate_formal(formal))
                .collect(),
            variables,
            ..Self::default()
        }
    }

    pub(super) fn instantiate_type(&self, ty: Ty) -> Ty {
        self.variables.instantiate_type(ty)
    }

    fn declared_type(&self, ty: Ty) -> Ty {
        self.variables.declared_type(ty)
    }

    pub(super) fn enter_for_type(
        frames: &mut Vec<Self>,
        compilation: u64,
        file: u32,
        call: ExprId,
        formals: &[String],
        ty: Ty,
    ) -> (Ty, bool) {
        let constraints = Self::for_call(compilation, file, call, formals);
        let ty = constraints.instantiate_type(ty);
        let entered = matches!(ty.non_null(), Ty::Fun(_)) && constraints.mentions_formal(ty);
        if entered {
            frames.push(constraints);
        }
        (ty, entered)
    }

    pub(super) fn instantiate_optional(&self, ty: Option<Ty>) -> Option<Ty> {
        ty.map(|ty| self.instantiate_type(ty))
    }

    pub(super) fn instantiate_types(&self, types: Vec<Ty>) -> Vec<Ty> {
        types
            .into_iter()
            .map(|ty| self.instantiate_type(ty))
            .collect()
    }

    pub(super) fn enter_for_function(
        frames: &mut Vec<Self>,
        compilation: u64,
        file: u32,
        call: ExprId,
        formals: &[String],
        function: &'static crate::types::FnSig,
    ) -> (&'static crate::types::FnSig, bool) {
        match Self::enter_for_type(frames, compilation, file, call, formals, Ty::Fun(function)) {
            (Ty::Fun(function), entered) => (function, entered),
            _ => unreachable!("a function type remains a function type"),
        }
    }

    pub(super) fn enter_lambda(
        frames: &mut Vec<Self>,
        compilation: u64,
        file: u32,
        call: ExprId,
        formals: &[String],
        parameters: Vec<Ty>,
        expected: Option<Ty>,
        fixed: Option<&[Option<Ty>]>,
        parameter: usize,
        receiver: Option<Ty>,
    ) -> PostponedLambdaTypes {
        let constraints = Self::for_call(compilation, file, call, formals);
        let parameters = constraints.instantiate_types(parameters);
        let expected = constraints.instantiate_optional(expected);
        let fixed = constraints.instantiate_optional(
            fixed
                .and_then(|types| types.get(parameter))
                .copied()
                .flatten(),
        );
        let receiver = constraints.instantiate_optional(receiver);
        let entered = expected.is_some_and(|expected| constraints.mentions_formal(expected));
        if entered {
            frames.push(constraints);
        }
        (parameters, expected, fixed, receiver, entered)
    }

    /// Whether `bound` is `formal`'s own variable in a position that always holds: `V` or `V & Any`
    /// below `V`, `V` or `V?` above it. Such a constraint carries no information about the variable
    /// (kotlinc's subtyping answers it without recording one), but recording it would join the
    /// variable into its own solution. A nested lambda whose result is a builder member's `V?`
    /// relates `() -> V?` to the same type once its callee's result variable has been replaced by
    /// `V?`; that must not turn `V`'s concrete `Int` evidence into `V & Any`.
    fn is_trivial_self_bound(&self, formal: &str, bound: Ty, lower: bool) -> bool {
        let variable = match (bound, lower) {
            (Ty::DefinitelyNotNull(inner), true) | (Ty::Nullable(inner), false) => *inner,
            (bound, _) => bound,
        };
        matches!(variable, Ty::TyParam(name, _) if name == formal)
    }

    pub(super) fn mentions_formal(&self, ty: Ty) -> bool {
        ty_mentions_param(ty, &self.formals)
    }

    /// Join a new lower bound into `formal`'s current one. Sibling subtypes join to their nearest
    /// common supertype through the class hierarchy (kotlinc's common-supertype calculation for a
    /// type variable's lower constraints), not to `Any`: `put("a", Dot()); put("b", Line())` in a
    /// builder fixes `V` to their sealed parent.
    fn join_lower(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
        if self.is_trivial_self_bound(formal, actual, true) {
            return;
        }
        self.record_lower(source, formal, actual);
    }

    /// Record evidence already validated in its call-owned namespace. Translating a recursive
    /// call's `T(call) := T(enclosing)` back to the declaration boundary makes both identities
    /// spell alike; re-running the self-bound filter after that translation would discard the real
    /// enclosing-type evidence the distinct call-owned identity preserved.
    fn record_lower(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
        let inputs = self.lower_inputs.entry(formal.to_string()).or_default();
        let actual = crate::symbol_resolver::inference_actual(actual);
        if !inputs.contains(&actual) {
            inputs.push(actual);
        }
        self.rebuild_bounds(source);
    }

    fn record_upper(&mut self, formal: String, actual: Ty) {
        self.upper_inputs
            .entry(formal.clone())
            .or_default()
            .push(actual);
        self.upper.entry(formal).or_default().push(actual);
    }

    fn rebuild_bounds(&mut self, source: &dyn SymbolSource) {
        let mut lower = self.lower_inputs.clone();
        for contribution in self.function_result_lower.values() {
            for (formal, actual) in contribution {
                let inputs = lower.entry(formal.clone()).or_default();
                if !inputs.contains(actual) {
                    inputs.push(*actual);
                }
            }
        }
        self.lower = lower
            .into_iter()
            .map(|(formal, inputs)| {
                let merged = crate::symbol_resolver::merge_inferred_lower_bounds_from_symbols(
                    source, &inputs,
                );
                (formal, merged)
            })
            .collect();

        self.upper = self.upper_inputs.clone();
        for contribution in self.function_result_upper.values() {
            for (formal, inputs) in contribution {
                self.upper.entry(formal.clone()).or_default().extend(inputs);
            }
        }
    }

    pub(super) fn constrain_assignable(
        &mut self,
        source: &dyn SymbolSource,
        expected: Ty,
        actual: Ty,
        inferred: &crate::symbol_resolver::AssignabilityConstraints,
        shadowed_formals: &crate::fx_hash::HashSet<String>,
    ) {
        crate::trace_compiler!(
            "lambda_apply",
            "postponed constrain expected={expected:?} actual={actual:?}"
        );
        for (formal, actual) in inferred.lower.iter() {
            if self.formals.iter().any(|allowed| allowed == formal)
                && !shadowed_formals.contains(formal.as_str())
            {
                self.join_lower(source, formal, *actual);
            }
        }

        let upper = inferred
            .upper
            .iter()
            .filter(|(formal, _)| {
                self.formals.iter().any(|allowed| allowed == *formal)
                    && !shadowed_formals.contains(formal.as_str())
            })
            .flat_map(|(formal, upper)| {
                upper
                    .iter()
                    .copied()
                    .filter(|bound| !self.is_trivial_self_bound(formal, *bound, false))
                    .map(|bound| (formal.clone(), bound))
            })
            .collect::<Vec<_>>();
        for (formal, upper) in upper {
            self.record_upper(formal, upper);
        }
    }

    /// Record the result subtype relation a checked lambda contributes to the call's open
    /// `formals`. `selector = { it }` against `(String) -> R` gives `String <: R`; a produced
    /// `(Number) -> Unit` against `(R) -> Unit` gives `R <: Number` through parameter
    /// contravariance. Both sides use resolved identities and the common assignability collector.
    pub(super) fn constrain_function_result(
        &mut self,
        source: &dyn SymbolSource,
        argument: ExprId,
        formals: &[String],
        declared: Ty,
        checked: Ty,
    ) {
        let (Ty::Fun(declared), Ty::Fun(checked)) = (declared.non_null(), checked.non_null())
        else {
            return;
        };
        let inferred = crate::symbol_resolver::collect_assignability_constraints_from_symbols(
            source,
            declared.ret,
            checked.ret,
        );
        self.record_function_argument_constraints(source, argument, formals, inferred);
    }

    /// A callable reference is selected from its complete expected function type. Unlike a lambda,
    /// its resolved input types are also declaration evidence: specializing `R = String` can select
    /// `convert(Int): String` and thereby constrain the still-open input formal to `Int`.
    pub(super) fn constrain_callable_reference(
        &mut self,
        source: &dyn SymbolSource,
        argument: ExprId,
        formals: &[String],
        declared: Ty,
        checked: Ty,
    ) {
        let inferred = crate::symbol_resolver::collect_assignability_constraints_from_symbols(
            source, declared, checked,
        );
        self.record_function_argument_constraints(source, argument, formals, inferred);
    }

    fn record_function_argument_constraints(
        &mut self,
        source: &dyn SymbolSource,
        argument: ExprId,
        formals: &[String],
        inferred: crate::symbol_resolver::AssignabilityConstraints,
    ) {
        // A call's owned variables are the only writable identities here. Any enclosing variables
        // present in the function shape are fixed expectations, not constraints on this call.
        let excluded = self
            .formals
            .iter()
            .filter(|formal| !formals.contains(formal))
            .cloned()
            .collect::<crate::fx_hash::HashSet<_>>();
        let lower: HashMap<String, Ty> = inferred
            .lower
            .into_iter()
            .filter(|(formal, actual)| {
                self.formals.contains(formal)
                    && !excluded.contains(formal)
                    && !self.is_trivial_self_bound(formal, *actual, true)
            })
            .map(|(formal, actual)| (formal, crate::symbol_resolver::inference_actual(actual)))
            .collect();
        let upper: HashMap<String, Vec<Ty>> = inferred
            .upper
            .into_iter()
            .filter(|(formal, _)| self.formals.contains(formal) && !excluded.contains(formal))
            .map(|(formal, inputs)| {
                let inputs = inputs
                    .into_iter()
                    .filter(|bound| !self.is_trivial_self_bound(&formal, *bound, false))
                    .collect();
                (formal, inputs)
            })
            .collect();
        self.function_result_lower.insert(argument, lower);
        self.function_result_upper.insert(argument, upper);
        self.rebuild_bounds(source);
    }

    fn discard_deferred_member_errors_within(&mut self, span: Span) {
        self.deferred_member_errors
            .retain(|(_, diagnostic, _)| diagnostic.lo < span.lo || diagnostic.hi > span.hi);
    }

    /// Specialize a lambda expectation for a fixed-point recheck. Its inputs consume every
    /// solution known so far, but a result owned by this postponed call remains open so the
    /// rechecked body can refine an earlier provisional solution. Bindings outside this call stay
    /// fixed in the result; explicit type arguments and enclosing declaration variables must not
    /// be reopened as inference targets.
    pub(super) fn refinement_lambda_expectation(
        &self,
        declared: Ty,
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> Ty {
        let Ty::Fun(function) = declared.non_null() else {
            return crate::symbol_resolver::ty_subst_keep_unbound(declared, bindings);
        };
        let fixed_result_bindings = bindings
            .iter()
            .filter(|(formal, _)| !self.formals.contains(*formal))
            .map(|(formal, actual)| (formal.clone(), *actual))
            .collect();
        let expectation = Ty::fun_with_shape(
            function
                .params
                .iter()
                .map(|parameter| {
                    crate::symbol_resolver::ty_subst_keep_unbound(*parameter, bindings)
                })
                .collect(),
            crate::symbol_resolver::ty_subst_keep_unbound(function.ret, &fixed_result_bindings),
            function.context_count,
            function.has_receiver,
            function.suspend,
        );
        declared.rewrap_nullability(expectation)
    }

    pub(super) fn constrain_equal(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
        let formal = self.variables.instantiate_formal(formal);
        if !self.formals.iter().any(|allowed| allowed == &formal) {
            return;
        }
        self.join_lower(source, &formal, actual);
        if !self.is_trivial_self_bound(&formal, actual, false) {
            self.record_upper(formal, actual);
        }
    }

    pub(super) fn merge(&mut self, source: &dyn SymbolSource, other: Self) {
        let receiving_variables = self.variables.clone();
        let sending_variables = other.variables.clone();
        let receive_formal = |formal: &str| {
            receiving_variables.instantiate_formal(&sending_variables.declared_formal(formal))
        };
        let receive_type =
            |ty: Ty| receiving_variables.instantiate_type(sending_variables.declared_type(ty));
        for formal in other.formals {
            let formal = receive_formal(&formal);
            if !self.formals.contains(&formal) {
                self.formals.push(formal);
            }
        }
        for (formal, inputs) in other.lower_inputs {
            let formal = receive_formal(&formal);
            for actual in inputs {
                self.record_lower(source, &formal, receive_type(actual));
            }
        }
        for (formal, upper) in other.upper_inputs {
            let formal = receive_formal(&formal);
            for actual in upper {
                self.record_upper(formal.clone(), receive_type(actual));
            }
        }
        for (argument, lower) in other.function_result_lower {
            self.function_result_lower.insert(
                argument,
                lower
                    .into_iter()
                    .map(|(formal, actual)| (receive_formal(&formal), receive_type(actual)))
                    .collect(),
            );
        }
        for (argument, upper) in other.function_result_upper {
            self.function_result_upper.insert(
                argument,
                upper
                    .into_iter()
                    .map(|(formal, inputs)| {
                        (
                            receive_formal(&formal),
                            inputs.into_iter().map(receive_type).collect(),
                        )
                    })
                    .collect(),
            );
        }
        self.rebuild_bounds(source);
        for diagnostic in other.deferred_member_errors {
            if !self.deferred_member_errors.contains(&diagnostic) {
                self.deferred_member_errors.push(diagnostic);
            }
        }
    }
}

impl Checker<'_> {
    /// Reach the fixed point of the constraints contributed by postponed function arguments.
    /// Rechecks and their diagnostic ownership stay together here; the call-site facade supplies
    /// only the selected parameter shapes and consumes the finalized argument types.
    pub(super) fn refine_postponed_function_arguments(
        &mut self,
        site: PostponedRefinementSite<'_, '_>,
        argument_types: &mut [Ty],
        known_bindings: &mut crate::symbol_resolver::GSigBinds,
        constraints: &mut PostponedCallConstraints,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut prior_solutions = crate::symbol_resolver::GSigBinds::default();
        // PCLA checks each lambda once, then solves the shared callable variables. Rechecking an
        // affected argument can publish another constraint, including for an already-solved
        // formal, so the monotone constraint set is complete only when every solution is stable.
        loop {
            let constrained_formals = constraints
                .lower
                .keys()
                .chain(constraints.upper.keys())
                .cloned()
                .collect::<crate::fx_hash::HashSet<_>>();
            let mut changed = Vec::new();
            for formal in constrained_formals {
                let Some(solution) = self.postponed_solution(constraints, &formal) else {
                    continue;
                };
                crate::trace_compiler!(
                    "lambda_apply",
                    "call={} postponed solution formal={formal} solution={solution:?} lower={:?} upper={:?}",
                    site.call_name,
                    constraints.lower.get(&formal),
                    constraints.upper.get(&formal),
                );
                if prior_solutions.get(&formal) == Some(&solution) {
                    continue;
                }
                known_bindings.insert(formal.clone(), solution);
                prior_solutions.insert(formal.clone(), solution);
                changed.push(formal);
            }
            if changed.is_empty() {
                break;
            }
            for (argument_index, &argument) in site.arguments.iter().enumerate() {
                let is_lambda = matches!(self.file.expr(argument), Expr::Lambda { .. });
                let is_callable_reference =
                    matches!(self.file.expr(argument), Expr::CallableRef { .. });
                if !is_lambda && !is_callable_reference {
                    continue;
                }
                let selected_parameter = site.selected_shape.and_then(|shape| {
                    let declared = *shape.argument_parameters.get(argument_index)?;
                    let has_receiver = shape
                        .receivers
                        .as_ref()
                        .and_then(|receivers| receivers.get(argument_index))
                        .copied()
                        .flatten()
                        .is_some();
                    let inline = shape.inlines_argument(argument_index).unwrap_or(false);
                    Some((
                        declared,
                        has_receiver,
                        inline,
                        shape.generic_formals.as_slice(),
                    ))
                });
                let known_parameter = || {
                    let signature = site.known_signature?;
                    let generic = signature.generic_sig.as_ref()?;
                    let parameter = site
                        .parameter_by_argument
                        .and_then(|parameters| parameters.get(argument_index))
                        .copied()
                        .unwrap_or(argument_index);
                    Some((
                        *generic.params.get(parameter)?,
                        signature
                            .lambda_recv
                            .get(parameter)
                            .copied()
                            .unwrap_or(false),
                        signature.inlines_lambda_for(parameter),
                        generic.formals.as_slice(),
                    ))
                };
                let Some((declared, has_receiver, inline, result_formals)) =
                    selected_parameter.or_else(known_parameter)
                else {
                    continue;
                };
                // A lambda's own result changing is not new contextual input for its body. Feed
                // back only receiver/context/value-input solutions; a result-only recheck can
                // weaken the stable result it just published. Callable references remain
                // sensitive to their complete function type because the result selects overloads.
                let changed_input = if is_lambda {
                    matches!(declared.non_null(), Ty::Fun(function) if changed.iter().any(|formal| {
                        function.params.iter().any(|parameter| {
                            ty_mentions_param(*parameter, std::slice::from_ref(formal))
                        })
                    }))
                } else {
                    changed
                        .iter()
                        .any(|formal| ty_mentions_param(declared, std::slice::from_ref(formal)))
                };
                if !changed_input {
                    continue;
                }
                let expected = if is_lambda {
                    constraints.refinement_lambda_expectation(declared, known_bindings)
                } else {
                    // A reference's result participates in overload selection; unlike a lambda,
                    // its complete expected signature selects the referenced overload.
                    crate::symbol_resolver::ty_subst_keep_unbound(declared, known_bindings)
                };
                if !matches!(expected.non_null(), Ty::Fun(_)) {
                    continue;
                }
                let checked = if is_lambda {
                    self.with_argument_lambda(argument, inline, |checker| {
                        checker.check_argument_expected(
                            site.scope,
                            argument,
                            expected,
                            has_receiver,
                            site.call_label,
                        )
                    })
                } else {
                    self.postponed_diagnostics.discard(argument);
                    let diagnostic_mark = self.diags.diags.len();
                    let nominal = self.expr_expected(site.scope, argument, expected);
                    let checked = self
                        .expression_function_type(site.scope, argument, nominal)
                        .unwrap_or(nominal);
                    self.postponed_diagnostics
                        .capture_since(argument, self.diags, diagnostic_mark);
                    checked
                };
                if is_lambda
                    && matches!(expected.non_null(), Ty::Fun(function)
                        if function.params.iter().all(|input| !constraints.mentions_formal(*input)))
                {
                    // The concrete-input check is now authoritative. Any unresolved member still
                    // present was emitted directly by that check; the symbolic probe's deferred
                    // copy must not survive merely because its selector expression retained an
                    // `Error` slot from the probe.
                    constraints.discard_deferred_member_errors_within(self.span(argument));
                }
                argument_types[argument_index] = checked;
                if is_lambda {
                    constraints.constrain_function_result(
                        &self.fed_source(),
                        argument,
                        result_formals,
                        declared,
                        checked,
                    );
                } else {
                    constraints.constrain_callable_reference(
                        &self.fed_source(),
                        argument,
                        result_formals,
                        declared,
                        checked,
                    );
                }
            }
        }
        prior_solutions
    }

    /// Close one function argument's call-owned inference namespace. Its collected constraints and
    /// its checked function type cross the call boundary together; allowing only the constraints
    /// through would leave the argument carrying temporary identities into final overload
    /// selection.
    pub(super) fn merge_postponed_function_argument(
        &mut self,
        expression: ExprId,
        checked: Ty,
        aggregate: &mut PostponedCallConstraints,
        frame: PostponedCallConstraints,
    ) -> Ty {
        let checked = frame.declared_type(checked);
        aggregate.merge(&self.fed_source(), frame);
        self.set(expression, checked);
        checked
    }

    /// The solution `constraints` give `formal`: its lower bound when every upper bound admits
    /// it, otherwise an upper bound that every upper bound admits.
    pub(super) fn postponed_solution(
        &self,
        constraints: &PostponedCallConstraints,
        formal: &str,
    ) -> Option<Ty> {
        let upper = constraints
            .upper
            .get(formal)
            .map(Vec::as_slice)
            .unwrap_or_default();
        constraints
            .lower
            .get(formal)
            .copied()
            .filter(|lower| {
                upper
                    .iter()
                    .all(|bound| self.receiver_is_assignable(*lower, *bound))
            })
            .or_else(|| {
                upper.iter().copied().find(|candidate| {
                    upper
                        .iter()
                        .all(|bound| self.receiver_is_assignable(*candidate, *bound))
                })
            })
    }

    /// Fix, before a lambda is analyzed, each of the call's open `formals` that the lambda's
    /// `inputs` (its receiver and parameter types) mention and that the arguments analyzed so far
    /// already solve. kotlinc fixes a lambda's input type variables before it analyzes the body, so
    /// `selector = { it }` solving `R = String` types `printer: (R, R) -> String` as
    /// `(String, String) -> String`; leaving `R` open would type the body against a variable that
    /// is already known. A solution that still mentions one of the call's own formals is not proper
    /// yet and stays open.
    pub(super) fn fix_postponed_lambda_inputs(
        &self,
        formals: &[String],
        inputs: &[Ty],
        constraints: &PostponedCallConstraints,
        bindings: &mut crate::symbol_resolver::GSigBinds,
    ) {
        for formal in formals {
            if bindings.contains_key(formal)
                || !inputs
                    .iter()
                    .any(|input| ty_mentions_param(*input, std::slice::from_ref(formal)))
            {
                continue;
            }
            let Some(solution) = self
                .postponed_solution(constraints, formal)
                .filter(|solution| !ty_mentions_param(*solution, formals))
            else {
                continue;
            };
            crate::trace_compiler!(
                "lambda_apply",
                "fixed lambda input formal={formal} solution={solution:?}",
            );
            bindings.insert(formal.clone(), solution);
        }
    }

    pub(super) fn postponed_call_mentions(&self, ty: Ty) -> bool {
        self.postponed_call_constraints
            .iter()
            .any(|constraints| constraints.mentions_formal(ty))
    }

    /// Test one symbolic assignability relation against cloned postponed-call frames. Selection
    /// may use the answer, but only the ordinary argument-commit path may publish the constraints.
    pub(super) fn postponed_assignability_trial(&self, expected: Ty, actual: Ty) -> bool {
        if !(self.postponed_call_mentions(expected) || self.postponed_call_mentions(actual))
            || !self.postponed_argument_constraint_is_satisfiable(expected, actual)
        {
            return false;
        }
        let source = self.fed_source();
        let inferred = crate::symbol_resolver::collect_assignability_constraints_from_symbols(
            &source, expected, actual,
        );
        let mut bindings = crate::symbol_resolver::GSigBinds::default();
        let mut shadowed = crate::fx_hash::HashSet::default();
        for constraints in self.postponed_call_constraints.iter().rev() {
            let mut trial = constraints.clone();
            trial.constrain_assignable(&source, expected, actual, &inferred, &shadowed);
            for formal in &trial.formals {
                if !shadowed.contains(formal) {
                    if let Some(solution) = self.postponed_solution(&trial, formal) {
                        bindings.insert(formal.clone(), solution);
                    }
                }
            }
            shadowed.extend(constraints.formals.iter().cloned());
        }
        let expected = crate::symbol_resolver::ty_subst_keep_unbound(expected, &bindings);
        let actual = crate::symbol_resolver::ty_subst_keep_unbound(actual, &bindings);
        self.receiver_is_assignable(actual, expected)
    }

    /// Defer an unresolved member on a still-symbolic postponed lambda input. The call solution
    /// rechecks the expression with its concrete input and reports an unresolved error at commit.
    pub(super) fn defer_postponed_member_error(
        &mut self,
        receiver: Ty,
        expression: Option<ExprId>,
        span: Span,
        name: &str,
    ) -> bool {
        let Some(expression) = expression else {
            return false;
        };
        let Some(frame) = self
            .postponed_call_constraints
            .iter_mut()
            .rev()
            .find(|constraints| constraints.mentions_formal(receiver))
        else {
            return false;
        };
        let diagnostic = (expression, span, name.to_string());
        if !frame.deferred_member_errors.contains(&diagnostic) {
            frame.deferred_member_errors.push(diagnostic);
        }
        true
    }

    pub(super) fn apply_postponed_call_bindings(&self, ty: Ty) -> Ty {
        self.postponed_call_constraints
            .iter()
            .rev()
            .fold(ty, |ty, constraints| {
                crate::symbol_resolver::ty_subst_keep_unbound(ty, &constraints.lower)
            })
    }

    /// Whether every symbolic slot belongs to the surrounding declaration rather than to the
    /// inference problem currently being solved.
    pub(super) fn type_is_lexically_fixed(scope: &CheckerScope<'_>, ty: Ty) -> bool {
        let lexical_bindings = scope
            .lexical_tparam_identities()
            .into_iter()
            .map(|formal| (formal, Ty::obj("kotlin/Any")))
            .collect::<crate::symbol_resolver::GSigBinds>();
        !crate::symbol_resolver::ty_subst_keep_unbound(ty, &lexical_bindings).mentions_ty_param()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::EmptySymbolSource;

    fn formal(name: &str) -> Ty {
        Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")))
    }

    #[test]
    fn a_call_owned_variables_own_shapes_are_not_constraints() {
        let name = "example:build:V".to_string();
        let value = formal(&name);
        let mut constraints = PostponedCallConstraints::for_formals(&[name.clone()], |_| false);

        assert!(constraints.is_trivial_self_bound(&name, value, true));
        assert!(constraints.is_trivial_self_bound(
            &name,
            Ty::DefinitelyNotNull(crate::types::intern_ty(value)),
            true
        ));
        assert!(constraints.is_trivial_self_bound(&name, value, false));
        assert!(constraints.is_trivial_self_bound(&name, Ty::nullable(value), false));
        constraints.constrain_equal(&EmptySymbolSource, &name, value);

        assert!(constraints.lower.is_empty());
        assert!(constraints.upper.is_empty());
    }

    #[test]
    fn a_recursive_calls_variable_keeps_the_enclosing_formal_as_evidence() {
        let name = "example:recurse:T".to_string();
        let enclosing = formal(&name);
        let mut call = PostponedCallConstraints::for_formals(&[name.clone()], |_| true);
        let call_owned = call.instantiate_type(enclosing);
        assert_ne!(call_owned, enclosing);

        call.constrain_equal(&EmptySymbolSource, &name, enclosing);
        let call_owned_name = call_owned.ty_param_name().expect("call-owned variable");
        assert_eq!(call.lower[call_owned_name], enclosing);

        let mut declared = PostponedCallConstraints::default();
        declared.merge(&EmptySymbolSource, call);
        assert_eq!(declared.lower[&name], enclosing);
    }

    #[test]
    fn a_rechecked_lambda_replaces_its_provisional_result_constraint() {
        let name = "example:specialize:R".to_string();
        let mut constraints = PostponedCallConstraints::for_formals(&[name], |_| false);
        let formals = constraints.formals.clone();
        let result = formal(&formals[0]);
        let declared = Ty::fun(vec![Ty::obj("example/Base")], result);
        let argument = ExprId(1);

        constraints.constrain_function_result(
            &EmptySymbolSource,
            argument,
            &formals,
            declared,
            Ty::fun(vec![Ty::obj("example/Base")], Ty::obj("kotlin/Any")),
        );
        assert_eq!(constraints.lower[&formals[0]], Ty::obj("kotlin/Any"));

        constraints.constrain_function_result(
            &EmptySymbolSource,
            argument,
            &formals,
            declared,
            Ty::fun(vec![Ty::obj("example/Derived")], Ty::String),
        );
        assert_eq!(constraints.lower[&formals[0]], Ty::String);
    }
}
