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
}
