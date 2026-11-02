//! Zero-element reference-array factories whose element type only a selected parameter supplies.
//!
//! `arrayOf()` carries no element evidence of its own. Every call shape that selects a callable,
//! including constructor delegation (`: Base(arrayOf())`, `super(*arrayOf())`, `this(...)`, enum
//! entries), keeps it uncommitted until the selected parameter supplies the concrete `Array<T>`.

use super::*;

impl Checker<'_> {
    /// A zero-element reference-array factory has no element evidence until an enclosing selected
    /// parameter supplies `Array<T>`. Keep that call uncommitted during overload probing, exactly as
    /// an unshaped lambda is kept uncommitted; the selected-argument seam re-enters the ordinary
    /// synthetic call checker with the concrete array expectation. This predicate identifies the
    /// compiler declaration after normal lexical/module/classifier shadowing, rather than reserving
    /// the source spelling.
    pub(super) fn postponed_empty_reference_array_factory(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> bool {
        let Expr::Call { callee, args } = self.file.expr(expression) else {
            return false;
        };
        if !args.is_empty()
            || self
                .file
                .call_type_args
                .get(&expression.0)
                .is_some_and(|arguments| !arguments.is_empty())
        {
            return false;
        }
        let Expr::Name(name) = self.file.expr(*callee) else {
            return false;
        };
        let Some(synthetic) = crate::synthetics::lookup(name) else {
            return false;
        };
        if synthetic.kind != crate::synthetics::SyntheticKind::ReferenceVararg
            || self.lexical_value_claims_call_with_arguments(scope, name, args)
            || self.module_declares(name)
        {
            return false;
        }
        !matches!(
            self.select_classifier(scope, name),
            InheritedNestedClassifier::Found(internal) if !internal.matches(synthetic.fqn)
        )
    }

    /// Check a factory still postponed (its `Error` placeholder is not a checked result) under its
    /// selected parameter; `None` when `argument` is not one. A parameter that is not an array
    /// supplies no element type: the factory reports its uninferred type parameter, and kotlinc
    /// then rejects the argument against that parameter as well.
    pub(super) fn commit_postponed_array_factory(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        expected: Ty,
    ) -> Option<Ty> {
        if self.expr_types[argument.0 as usize] != Ty::Error
            || !self.postponed_empty_reference_array_factory(scope, argument)
        {
            return None;
        }
        let actual = self.expr_expected(scope, argument, expected);
        let Expr::Call { callee, .. } = self.file.expr(argument) else {
            return Some(actual);
        };
        let Expr::Name(name) = self.file.expr(*callee) else {
            return Some(actual);
        };
        if actual != Ty::Error || expected.non_null().is_reference_array() {
            return Some(actual);
        }
        let Some((formal, result)) = crate::synthetics::lookup(name)
            .and_then(|synthetic| self.array_factory_formal(synthetic))
        else {
            return Some(actual);
        };
        // An open call variable (`fun <T> take(x: T)`) and a supertype of every array (`Any`)
        // accept the uninferred array; only the uninferred type parameter is reported then.
        let uninferred = Ty::obj_args_name(
            result
                .obj_internal()
                .expect("an array factory produces a classifier"),
            &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
        );
        if !Self::type_is_lexically_fixed(scope, expected)
            || self.receiver_is_assignable(uninferred, expected)
        {
            return Some(actual);
        }
        let expected_name = self.diagnostic_type_name(expected, &[expected]);
        let array = self.diagnostic_type_name(result, &[result]);
        self.diags.error(
            self.span(argument),
            format!(
                "argument type mismatch: actual type is '{array}<uninferred {formal} (of fun <{formal}> {name})>', but '{expected_name}' was expected."
            ),
        );
        Some(actual)
    }

    /// kotlinc's report for a reference-array factory that has no element evidence: its own type
    /// parameter cannot be inferred, at the callee name.
    pub(super) fn report_uninferred_array_factory(
        &mut self,
        call: ExprId,
        synthetic: &crate::synthetics::Synthetic,
    ) {
        match self.array_factory_formal(synthetic) {
            Some((formal, _)) => self.diags.error(
                self.call_callee_name_span(call),
                format!("cannot infer type for type parameter '{formal}'. Specify it explicitly."),
            ),
            None => self.diags.error(
                self.span(call),
                "krusty: empty arrayOf() needs an explicit type (unsupported)".to_string(),
            ),
        }
    }

    /// The factory declaration's sole type parameter, by its source name, and its result
    /// classifier without arguments. The registry entry names the declaration; its formal and
    /// result come from the declaration itself.
    fn array_factory_formal(
        &self,
        synthetic: &crate::synthetics::Synthetic,
    ) -> Option<(String, Ty)> {
        let (package, _) = synthetic.fqn.rsplit_once('/')?;
        let package = crate::types::type_name(package);
        self.resolver()
            .top_level_candidates(synthetic.name)
            .into_iter()
            .find(|function| function.callable.owner == package)
            .and_then(|function| {
                let signature = function.generic_sig.as_ref()?;
                let [formal] = signature.formals.as_slice() else {
                    return None;
                };
                let result = Ty::obj_name(function.callable.ret.non_null().obj_internal()?);
                Some((
                    crate::types::type_parameter_source_name(formal.as_str()).to_string(),
                    result,
                ))
            })
    }
}
