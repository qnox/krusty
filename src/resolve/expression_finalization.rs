//! Validate a finalized expression and record its checked value facts before lexical cleanup.

use super::{Checker, CheckerScope, ExprId, Ty};

pub(super) fn finalize(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expression: ExprId,
    ty: Ty,
) {
    checker.check_expression_usage(scope, expression);
    checker.check_static_access_type_arguments(expression);
    checker.check_expression_qualifier_tree(expression);
    checker.check_reified_type_arguments(scope, expression);
    checker.check_array_of_nothing_expression(expression, ty);
    checker.check_expression_missing_supertypes(expression);
    checker.record_flexible_value(expression);
}
