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

impl Checker<'_> {
    /// The outer expectation that fixes each branch's own generic result, so a sibling branch
    /// does not rebind it.
    ///
    /// kotlinc gives a declared expectation to every branch of an `if`, `when` or elvis:
    /// `val i: I = if (c) materialize() else B()` fixes `materialize`'s `T` to `I`, not to the
    /// sibling's `B`. An expectation of `Any` or `Any?` constrains nothing and leaves the sibling to
    /// decide, as without one (`val x: Any = if (c) materialize() else B()` binds `T` to `B`). So
    /// does a call argument's: the conditional is then part of the enclosing call's inference, and
    /// `sink(if (c) materialize() else B())` binds `T` to `B` whatever `sink`'s parameter type.
    pub(super) fn expectation_fixes_branches(
        &mut self,
        scope: &CheckerScope<'_>,
        conditional: ExprId,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        usable_expected(scope, expected)
            .filter(|ty| {
                !matches!(ty.non_null(), Ty::Obj(name, _)
                    if crate::types::same(name, crate::types::wk::any()))
            })
            .filter(|_| !self.is_call_argument(conditional))
    }

    /// Whether `expression` is written as a value argument of a call.
    fn is_call_argument(&mut self, expression: ExprId) -> bool {
        let file = self.file;
        self.call_arguments
            .get_or_insert_with(|| {
                file.expr_arena
                    .iter()
                    .flat_map(|candidate| match candidate {
                        Expr::Call { args, .. }
                        | Expr::SafeCall {
                            args: Some(args), ..
                        } => args.as_slice(),
                        _ => &[],
                    })
                    .copied()
                    .collect()
            })
            .contains(&expression)
    }

    /// Recheck a branch whose selected generic call has an unbound result formal, using a sibling's
    /// result as its expectation. An outer expectation that fixes the branch (see
    /// [`expectation_fixes_branches`]) keeps it while the sibling conforms to that expectation; a
    /// sibling that does not (`val s: String = if (c) from(t) else 0`) still decides, so kotlinc's
    /// mismatch names the sibling's `Int`.
    pub(super) fn rebind_conditional_branch(
        &mut self,
        branch: ExprId,
        sibling: Ty,
        current: Ty,
        fixing_expectation: Option<Ty>,
        recheck: impl FnOnce(&mut Self, Ty) -> Ty,
    ) -> Ty {
        if fixing_expectation.is_some_and(|expected| self.receiver_is_assignable(sibling, expected))
            || current == Ty::Error
            || matches!(sibling, Ty::Error | Ty::Nothing)
            || sibling.mentions_pending()
        {
            return current;
        }
        let Some(signature) = self.conditional_call_result_signature(branch).cloned() else {
            return current;
        };
        let infer = |expected| {
            crate::symbol_resolver::infer_generic_return_bindings(
                &signature,
                expected,
                |actual, bound| self.receiver_is_assignable(actual, bound),
            )
        };
        // The sibling may be MORE specific than the generic result classifier: a
        // `MutableList<String>` branch constrains `listOf<T>()` through its applied `List<String>`
        // supertype. Project the sibling to the selected call's result classifier before giving up;
        // this is ordinary subtype information, not collection-specific approximation.
        let expectation = if infer(sibling).is_some() {
            sibling
        } else {
            let source = self.fed_source();
            let Some(applied) = crate::assignable::applied_supertype(
                &crate::symbol_resolver::SourceOracle(&source),
                sibling,
                signature.ret,
            ) else {
                return current;
            };
            if infer(applied).is_none() {
                return current;
            }
            applied
        };
        crate::trace_compiler!(
            "expected_call",
            "conditional branch {branch:?} rebinds against sibling {sibling:?} as {expectation:?}"
        );
        recheck(self, expectation)
    }
}
