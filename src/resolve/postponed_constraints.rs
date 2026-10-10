//! The constraints a postponed call's lambdas contribute to its type variables while they are
//! analyzed before the call is solved (kotlinc's PCLA).

use super::*;

pub(super) type PostponedLambdaTypes = (Vec<Ty>, Option<Ty>, Option<Ty>, Option<Ty>, bool);

#[derive(Clone, Default)]
pub(super) struct PostponedCallConstraints {
    pub(super) formals: Vec<String>,
    pub(super) lower: crate::symbol_resolver::GSigBinds,
    lower_inputs: HashMap<String, Vec<Ty>>,
    pub(super) upper: HashMap<String, Vec<Ty>>,
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
        let merged =
            crate::symbol_resolver::merge_inferred_lower_bounds_from_symbols(source, inputs);
        self.lower.insert(formal.to_string(), merged);
    }

    pub(super) fn constrain_assignable(
        &mut self,
        source: &dyn SymbolSource,
        expected: Ty,
        actual: Ty,
        inferred: &crate::symbol_resolver::AssignabilityConstraints,
        shadowed_formals: &std::collections::HashSet<String>,
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

        for (formal, upper) in inferred.upper.iter().filter(|(formal, _)| {
            self.formals.iter().any(|allowed| allowed == *formal)
                && !shadowed_formals.contains(formal.as_str())
        }) {
            let upper = upper
                .iter()
                .copied()
                .filter(|bound| !self.is_trivial_self_bound(formal, *bound, false))
                .collect::<Vec<_>>();
            self.upper.entry(formal.clone()).or_default().extend(upper);
        }
    }

    pub(super) fn constrain_equal(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
        let formal = self.variables.instantiate_formal(formal);
        if !self.formals.iter().any(|allowed| allowed == &formal) {
            return;
        }
        self.join_lower(source, &formal, actual);
        if !self.is_trivial_self_bound(&formal, actual, false) {
            self.upper.entry(formal).or_default().push(actual);
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
        for (formal, upper) in other.upper {
            self.upper
                .entry(receive_formal(&formal))
                .or_default()
                .extend(upper.into_iter().map(receive_type));
        }
        for diagnostic in other.deferred_member_errors {
            if !self.deferred_member_errors.contains(&diagnostic) {
                self.deferred_member_errors.push(diagnostic);
            }
        }
    }
}

impl Checker<'_> {
    /// Close one lambda's call-owned inference namespace. Its collected constraints and its checked
    /// function type cross the call boundary together; allowing only the constraints through would
    /// leave the argument carrying temporary identities into final overload selection.
    pub(super) fn merge_postponed_lambda(
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

    pub(super) fn postponed_call_mentions(&self, ty: Ty) -> bool {
        self.postponed_call_constraints
            .iter()
            .any(|constraints| constraints.mentions_formal(ty))
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
}
