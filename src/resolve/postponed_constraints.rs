//! The constraints a postponed call's lambdas contribute to its type variables while they are
//! analyzed before the call is solved (kotlinc's PCLA).

use super::*;

#[derive(Clone, Default)]
pub(super) struct PostponedCallConstraints {
    pub(super) formals: Vec<String>,
    pub(super) lower: crate::symbol_resolver::GSigBinds,
    lower_inputs: HashMap<String, Vec<Ty>>,
    pub(super) upper: HashMap<String, Vec<Ty>>,
    /// Diagnostics whose validity depends on the call solution. A lambda parameter still expressed
    /// as the callee's type variable cannot answer member lookup yet; the finalized recheck either
    /// resolves the expression or emits the error from its concrete receiver.
    pub(super) deferred_member_errors: Vec<(ExprId, Span, String)>,
}

impl PostponedCallConstraints {
    pub(super) fn for_formals(formals: &[String]) -> Self {
        Self {
            formals: formals.to_vec(),
            ..Self::default()
        }
    }

    pub(super) fn mentions_formal(&self, ty: Ty) -> bool {
        ty_mentions_param(ty, &self.formals)
    }

    /// Join a new lower bound into `formal`'s current one. Sibling subtypes join to their nearest
    /// common supertype through the class hierarchy (kotlinc's common-supertype calculation for a
    /// type variable's lower constraints), not to `Any`: `put("a", Dot()); put("b", Line())` in a
    /// builder fixes `V` to their sealed parent.
    fn join_lower(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
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
            self.upper
                .entry(formal.clone())
                .or_default()
                .extend(upper.iter().copied());
        }
    }

    pub(super) fn constrain_equal(&mut self, source: &dyn SymbolSource, formal: &str, actual: Ty) {
        if !self.formals.iter().any(|allowed| allowed == formal) {
            return;
        }
        self.join_lower(source, formal, actual);
        self.upper
            .entry(formal.to_string())
            .or_default()
            .push(actual);
    }

    pub(super) fn merge(&mut self, source: &dyn SymbolSource, other: Self) {
        for formal in other.formals {
            if !self.formals.contains(&formal) {
                self.formals.push(formal);
            }
        }
        for (formal, inputs) in other.lower_inputs {
            for actual in inputs {
                self.join_lower(source, &formal, actual);
            }
        }
        for (formal, upper) in other.upper {
            self.upper.entry(formal).or_default().extend(upper);
        }
        for diagnostic in other.deferred_member_errors {
            if !self.deferred_member_errors.contains(&diagnostic) {
                self.deferred_member_errors.push(diagnostic);
            }
        }
    }
}
