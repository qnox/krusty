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

/// Join a `try`'s body and catch types where [`join_types`] does not apply: the `try` is a
/// statement, or a value-position one has a primitive branch.
///
/// A diverging (`Nothing`) branch drops out. Two values of the SAME class with differing type
/// arguments (`List<Backup>` from the body vs `List<Nothing>` from a bare `emptyList()` catch) merge
/// to that class with erased arguments (`List<*>`), assignable to a declared `List<Backup>` return.
/// Branches that otherwise disagree give their common supertype where the value is discarded, as
/// kotlinc's FIR types every `try` (`try { sb.append(x) } catch (e: E) { println(e) }` is `Any`);
/// the JVM backend decides from that type whether the `try` holds a result temporary. A statement
/// `try` is not otherwise constrained. A value-position disagreement keeps the lenient `Unit`: the
/// backend stores every branch straight into one merge slot and cannot widen or box per branch.
pub(super) fn try_branch_join(
    checker: &Checker<'_>,
    value_required: bool,
    left: Ty,
    right: Ty,
) -> Ty {
    match (left, right) {
        _ if left == right => left,
        (Ty::Nothing, other) | (other, Ty::Nothing) => other,
        (Ty::Obj(a, _), Ty::Obj(b, _)) if a == b => Ty::obj_name(a),
        _ if value_required => Ty::Unit,
        _ => checker
            .semantic_common_supertype(left, right)
            .unwrap_or(Ty::Unit),
    }
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
