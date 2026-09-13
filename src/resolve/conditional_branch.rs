//! Conditional-result joining and branch-value discovery.

use super::{Checker, CheckerScope};
use crate::ast::{Expr, ExprId, File};
use crate::types::Ty;

/// Return an outer expectation only when it is a fixed type in this lexical scope.
///
/// A pending variable owned by the surrounding call cannot constrain a branch. A caller-owned
/// type parameter can: it is a stable semantic identity at this call site.
pub(super) fn usable_expected(scope: &CheckerScope<'_>, expected: Option<Ty>) -> Option<Ty> {
    expected
        .filter(|ty| *ty != Ty::Error && !ty.mentions_pending())
        .filter(|ty| Checker::type_is_lexically_fixed(scope, *ty))
}

/// Join the values produced by two conditional branches.
///
/// A fixed outer expectation is a representable upper bound when both branches satisfy it. This
/// matters when their exact Kotlin common type is an intersection that [`Ty`] deliberately does
/// not synthesize. `if`, `when`, elvis and `try` all share this rule.
pub(super) fn join_types(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expected: Option<Ty>,
    left: Ty,
    right: Ty,
    expression: ExprId,
) -> Ty {
    let span = checker.span(expression);
    let semantic_join = checker.semantic_common_supertype(left, right);
    if left != Ty::Nothing && right != Ty::Nothing {
        if let Some(expected) = usable_expected(scope, expected) {
            if semantic_join.is_some_and(|joined| checker.receiver_is_assignable(joined, expected))
            {
                return semantic_join.expect("checked semantic join");
            }
            if checker.receiver_is_assignable(left, expected)
                && checker.receiver_is_assignable(right, expected)
            {
                return expected;
            }
        }
    }
    semantic_join.unwrap_or_else(|| checker.join(left, right, span))
}

/// Return the expression whose value a conditional branch produces.
///
/// A branch written as a block (`if (c) { … } else { … }`) yields its trailing expression, and it
/// is that expression's call—not the block—that carries the generic signature a sibling branch can
/// rebind. Nested blocks are transparent; a block with no trailing expression produces no value and
/// remains the returned expression.
pub(super) fn branch_value_expression(file: &File, expression: ExprId) -> ExprId {
    let mut current = expression;
    loop {
        let Expr::Block { trailing, .. } = file.expr(current) else {
            return current;
        };
        match trailing {
            Some(trailing) => current = *trailing,
            None => return current,
        }
    }
}
