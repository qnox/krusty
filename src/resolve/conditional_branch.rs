//! Conditional-result joining and branch-value discovery.

use super::{Checker, CheckerScope, Wanted};
use crate::ast::{Expr, ExprId, File};
use crate::integer_constant::IntegerConstant;
use crate::libraries::GenericSig;
use crate::symbol_resolver::CallArgKind;
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

/// Retype integer-constant branches to the non-null primitive of the other branches.
///
/// A conditional with no expected type still adapts a constant that fits: `0` beside a `Long` is
/// `Long`, `0` beside a `Byte` is `Byte`, and `0u` beside a `ULong` is `ULong`. Branches that are
/// all integer constants stay constants, so `if (c) 1 else 2` remains `Int`. A constant that does
/// not fit, or a non-constant of another primitive, is left unchanged.
pub(super) fn integer_constant_branch_types(
    checker: &Checker<'_>,
    branches: &[(ExprId, Ty)],
) -> Vec<Ty> {
    let constants = branches
        .iter()
        .map(|(expression, _)| checker.integer_constant_provenance(*expression))
        .collect::<Vec<_>>();
    let types = branches.iter().map(|(_, ty)| *ty).collect::<Vec<_>>();
    let Some(target) = integer_constant_sibling_primitive(checker, &types, &constants) else {
        return types;
    };
    types
        .into_iter()
        .zip(constants)
        .map(|(ty, constant)| if constant.is_some() { target } else { ty })
        .collect()
}

/// The primitive every integer-constant branch fits, taken from the non-constant branches.
fn integer_constant_sibling_primitive(
    checker: &Checker<'_>,
    types: &[Ty],
    constants: &[Option<IntegerConstant>],
) -> Option<Ty> {
    let mut concrete = None;
    for (ty, constant) in types.iter().zip(constants) {
        if constant.is_some() {
            continue;
        }
        concrete = Some(match concrete {
            None => *ty,
            Some(so_far) => checker.semantic_common_supertype(so_far, *ty)?,
        });
    }
    let target = concrete?.non_null();
    let fits = constants.iter().all(|constant| {
        let Some(constant) = constant else {
            return true;
        };
        let natural = match constant {
            IntegerConstant::Signed(_) | IntegerConstant::DivisionByZero => Ty::Int,
            IntegerConstant::Unsigned(_) => Ty::UInt,
        };
        CallArgKind::integer_constant(natural, *constant).adapts_integer_literal_to(target)
    });
    let has_constant = constants.iter().any(Option::is_some);
    (fits && has_constant).then_some(target)
}

/// Adapt integer-constant branches, then join the pair the way `if` and elvis do.
pub(super) fn join_adapted_branches(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expected: Option<Ty>,
    left_expr: ExprId,
    left: Ty,
    right_expr: ExprId,
    right: Ty,
    expression: ExprId,
) -> Ty {
    let adapted = integer_constant_branch_types(checker, &[(left_expr, left), (right_expr, right)]);
    join_types(checker, scope, expected, adapted[0], adapted[1], expression)
}

/// Replace a `try` result when an integer constant was adapted to a sibling primitive.
///
/// The provisional result is the per-catch join. Adaptation re-joins only when a constant branch
/// changed, and that second join uses [`try_branch_join`] so a non-constant primitive disagreement
/// keeps the lenient result.
pub(super) fn adapted_try_result(
    checker: &Checker<'_>,
    value_required: bool,
    branches: &[(ExprId, Ty)],
    provisional: Ty,
) -> Ty {
    let adapted = integer_constant_branch_types(checker, branches);
    let changed = adapted
        .iter()
        .zip(branches.iter())
        .any(|(adapted_ty, (_, original))| *adapted_ty != *original);
    if !changed {
        return provisional;
    }
    adapted
        .into_iter()
        .reduce(|left, right| try_branch_join(checker, value_required, left, right))
        .unwrap_or(provisional)
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

/// Fold conditional results in source order behind the conditional-typing boundary. A declared
/// expectation stands in for an intersection [`Ty`] does not synthesize, as it does for `if`.
///
/// `resolve.rs` owns expression dispatch; the common-supertype policy and its declared-expectation
/// handling belong here with the other conditional result rules.
pub(super) fn join_results(
    checker: &mut Checker<'_>,
    scope: &CheckerScope<'_>,
    expected: Option<Ty>,
    results: impl IntoIterator<Item = (Ty, ExprId)>,
) -> Option<Ty> {
    let results = results.into_iter().collect::<Vec<_>>();
    let adapted = integer_constant_branch_types(
        checker,
        &results
            .iter()
            .map(|(ty, expression)| (*expression, *ty))
            .collect::<Vec<_>>(),
    );
    results
        .into_iter()
        .zip(adapted)
        .fold(None, |result, ((_, expression), ty)| {
            Some(match result {
                Some(current) => join_types(checker, scope, expected, current, ty, expression),
                None => ty,
            })
        })
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
/// `try` is not otherwise constrained. Integer-constant branches are adapted to a sibling primitive
/// before this function runs, so `try { longValue } catch (e: Exception) { 0 }` arrives as `Long`
/// on every branch and the backend widens the stored value. A value-position disagreement that is
/// not that adaptation keeps the lenient `Unit`: an `Int` expression beside a `Long` has no single
/// primitive the backend can widen both branches to.
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

/// The expressions whose values `expression` can produce: a block's trailing value, each branch of
/// an `if` with an `else` or a `when` with an `else` arm, both sides of an elvis, and a `try`'s body
/// and catches, recursively. Any other expression is its own value.
pub(super) fn terminal_value_expressions(file: &File, expression: ExprId, out: &mut Vec<ExprId>) {
    match file.expr(expression) {
        Expr::Block {
            trailing: Some(trailing),
            ..
        } => terminal_value_expressions(file, *trailing, out),
        Expr::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => {
            terminal_value_expressions(file, *then_branch, out);
            terminal_value_expressions(file, *else_branch, out);
        }
        Expr::When { arms, .. } if arms.iter().any(|arm| arm.conditions.is_empty()) => {
            for arm in arms {
                terminal_value_expressions(file, arm.body, out);
            }
        }
        Expr::Elvis { lhs, rhs } => {
            terminal_value_expressions(file, *lhs, out);
            terminal_value_expressions(file, *rhs, out);
        }
        Expr::Try { body, catches, .. } => {
            terminal_value_expressions(file, *body, out);
            for catch in catches {
                terminal_value_expressions(file, catch.body, out);
            }
        }
        _ => out.push(expression),
    }
}

impl Checker<'_> {
    /// Which expression owns completion of a conditional's still-open branch calls.
    ///
    /// A branch call owns an otherwise open result only while the conditional itself remains a
    /// viable value for its context. A selected callable also owns the terminal result of a lambda
    /// it is still shaping: its argument commitment reports any final mismatch or inference failure.
    /// Adding a branch-local diagnostic during that earlier probe would report the same failed
    /// completion twice.
    fn conditional_completion_owner(
        &self,
        scope: &CheckerScope<'_>,
        conditional: ExprId,
        expected: Option<Ty>,
        result: Ty,
    ) -> ConditionalCompletionOwner {
        if let Some((lambda, body, constraint)) = self.lambda_returns.active_result() {
            let is_lambda_result = branch_value_expression(self.file, body) == conditional;
            let owner = match constraint {
                super::lambda_returns::LambdaResultConstraint::CallOwned => {
                    ConditionalCompletionOwner::SelectedCall(lambda)
                }
                super::lambda_returns::LambdaResultConstraint::Open => {
                    if self.postponed_call_constraints.is_empty() {
                        ConditionalCompletionOwner::BranchResults
                    } else {
                        ConditionalCompletionOwner::EnclosingExpression
                    }
                }
                super::lambda_returns::LambdaResultConstraint::Fixed(_) => {
                    ConditionalCompletionOwner::BranchResults
                }
            };
            if is_lambda_result && owner != ConditionalCompletionOwner::BranchResults {
                return owner;
            }
        }
        match usable_expected(scope, expected) {
            Some(expected)
                if result == Ty::Error || !self.receiver_is_assignable(result, expected) =>
            {
                ConditionalCompletionOwner::EnclosingExpression
            }
            _ => ConditionalCompletionOwner::BranchResults,
        }
    }

    fn conditional_call_result_signature(
        &self,
        expression: ExprId,
    ) -> Option<std::borrow::Cow<'_, GenericSig>> {
        let expression = branch_value_expression(self.file, expression);
        if let Some(signature) = self.unbound_call_result_signature(expression) {
            return Some(signature);
        }
        if self
            .file
            .call_type_args
            .get(&expression.0)
            .is_some_and(|arguments| !arguments.is_empty())
        {
            return None;
        }
        let signature = self.selected_generic_call_signature(expression)?;
        signature
            .formals
            .iter()
            .any(|formal| {
                let formal = std::slice::from_ref(formal);
                crate::types::ty_mentions_param(signature.ret, formal)
                    && signature
                        .receiver
                        .is_none_or(|receiver| !crate::types::ty_mentions_param(receiver, formal))
                    && signature
                        .params
                        .iter()
                        .all(|parameter| !crate::types::ty_mentions_param(*parameter, formal))
            })
            .then_some(std::borrow::Cow::Borrowed(signature))
    }

    /// Whether the call selected at `call` recorded no solution for its formal at `index`.
    fn recorded_solution_is_absent(&self, call: ExprId, index: usize) -> bool {
        self.selected_call_type_arguments(call)
            .and_then(|arguments| arguments.get(index))
            .is_some_and(Option::is_none)
    }

    /// Whether a conditional branch's selected call still has a result formal without a recorded
    /// solution while its checked result substituted a provisional completion for it. That
    /// completion (`D<Any?>` for an unconstrained `D(1)`) is not a constraint supplied by source.
    pub(super) fn branch_result_is_open(&self, branch: ExprId) -> bool {
        let call = branch_value_expression(self.file, branch);
        let actual = self.expr_types[call.0 as usize];
        self.unbound_call_result_signature(call)
            .is_some_and(|signature| {
                signature.formals.iter().enumerate().any(|(index, formal)| {
                    let formal = std::slice::from_ref(formal);
                    crate::types::ty_mentions_param(signature.ret, formal)
                        && !crate::types::ty_mentions_param(actual, formal)
                        && self.recorded_solution_is_absent(call, index)
                })
            })
    }

    /// A branch's type as evidence for its siblings. An open branch offers no type to rebind a
    /// sibling against (see [`Self::branch_result_is_open`]).
    pub(super) fn branch_sibling_type(&self, branch: ExprId, ty: Ty) -> Ty {
        if self.branch_result_is_open(branch) {
            Ty::Error
        } else {
            ty
        }
    }

    /// Check one branch of an `if` with an `else` or a `when` with an `else` arm. kotlinc completes
    /// such a conditional as one synthetic call even in statement position, so a sibling branch can
    /// fix a type parameter this branch leaves open (`when (s) { "a" -> build { set(T()) }; else ->
    /// Buildee() }`). An open branch stays open until every branch has been checked; a sibling
    /// rebinds it and the conditional's completion reports anything still open.
    pub(super) fn joined_branch_result(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
        wanted: Wanted,
        joined: bool,
    ) -> Ty {
        if !joined || wanted.expected.is_some() || wanted.value_required {
            return self.expr_result(scope, branch, wanted.expected, wanted.value_required);
        }
        self.discarded_exprs.insert(branch);
        let ty = self.expr_with_context(scope, branch, false);
        let result = branch_value_expression(self.file, branch);
        if !self.branch_result_is_open(branch)
            && self.report_unbound_call_result_type_parameter(scope, result)
        {
            Ty::Error
        } else {
            ty
        }
    }

    /// Complete all selected result calls owned by one conditional expression.
    ///
    /// Siblings have already rebound each other's open calls. The completion boundary is shared by
    /// value and statement positions; an enclosing postponed argument keeps ownership because
    /// [`Self::report_unbound_conditional_branch`] declines while that argument is still probed.
    pub(super) fn complete_conditional_branches(
        &mut self,
        scope: &CheckerScope<'_>,
        conditional: ExprId,
        expected: Option<Ty>,
        result: Ty,
        branches: impl IntoIterator<Item = (ExprId, Ty)>,
    ) {
        let owner = self.conditional_completion_owner(scope, conditional, expected, result);
        for (branch, _) in branches {
            self.complete_conditional_branch(scope, branch, owner);
        }
    }

    fn complete_conditional_branch(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
        owner: ConditionalCompletionOwner,
    ) {
        if self.postponed_argument_depth != 0 {
            return;
        }
        match owner {
            ConditionalCompletionOwner::BranchResults => {
                self.report_unbound_conditional_branch(scope, branch);
            }
            ConditionalCompletionOwner::EnclosingExpression => {}
            ConditionalCompletionOwner::SelectedCall(lambda) => {
                let branches = self
                    .call_owned_conditional_branches
                    .entry(lambda)
                    .or_default();
                if !branches.contains(&branch) {
                    branches.push(branch);
                }
            }
        }
    }

    /// Commit branch-call completion retained while a selected callable shaped `lambda`.
    ///
    /// A fixed selected result owns any final mismatch, so its conditional branches must not add a
    /// second inference diagnostic. When the selected result itself is still a call-owned variable,
    /// however, kotlinc reports each terminal branch call that remains open. This is the first point
    /// at which those cases are distinguishable: candidate probing deliberately records both as
    /// [`LambdaResultConstraint::CallOwned`](super::lambda_returns::LambdaResultConstraint::CallOwned).
    pub(super) fn commit_call_owned_conditional_branches(
        &mut self,
        scope: &CheckerScope<'_>,
        lambda: ExprId,
        selected_parameter: Ty,
    ) {
        let Some(branches) = self.call_owned_conditional_branches.remove(&lambda) else {
            return;
        };
        let selected_shape = self.functional_parameter_shape(selected_parameter);
        let Some(Ty::Fun(selected_shape)) = selected_shape.map(Ty::non_null) else {
            debug_assert!(
                false,
                "call-owned lambda completion requires a selected functional parameter"
            );
            for branch in branches {
                self.report_unbound_conditional_branch(scope, branch);
            }
            return;
        };
        if !Self::type_is_lexically_fixed(scope, selected_shape.ret) {
            for branch in branches {
                self.report_unbound_conditional_branch(scope, branch);
            }
        }
    }

    /// Report a conditional branch whose selected call remains open after sibling rebinding: its
    /// result is still symbolic, or a result formal has no recorded solution (`D(1)` completed to
    /// `D<Any?>` only provisionally).
    pub(super) fn report_unbound_conditional_branch(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
    ) {
        if self.postponed_argument_depth != 0 {
            return;
        }
        let branch = branch_value_expression(self.file, branch);
        let Some(signature) = self
            .unbound_call_result_signature(branch)
            .map(std::borrow::Cow::into_owned)
        else {
            return;
        };
        let actual = self.expr_types[branch.0 as usize];
        let symbolic = !Self::type_is_lexically_fixed(scope, actual);
        let Some(formal) = signature
            .formals
            .iter()
            .enumerate()
            .find(|(index, formal)| {
                let formal = std::slice::from_ref(*formal);
                if crate::types::ty_mentions_param(actual, formal) {
                    symbolic
                } else {
                    crate::types::ty_mentions_param(signature.ret, formal)
                        && self.recorded_solution_is_absent(branch, *index)
                }
            })
            .map(|(_, formal)| formal)
        else {
            return;
        };
        self.diags.error(
            self.call_callee_name_span(branch),
            format!(
                "cannot infer type for type parameter '{}'. Specify it explicitly.",
                crate::types::type_parameter_source_name(formal)
            ),
        );
    }

    /// The outer expectation that fixes each branch's own generic result, so a sibling branch
    /// does not rebind it.
    ///
    /// kotlinc gives a declared expectation to every branch of an `if`, `when` or elvis:
    /// `val i: I = if (c) materialize() else B()` fixes `materialize`'s `T` to `I`, not to the
    /// sibling's `B`. A declared expectation is a declaration's, an assignment target's, a return's
    /// or a lambda body's type (see [`Checker::expr_declared`]), given to the conditional itself or
    /// through a block's result. A conditional nested in another's branch is part of the outer
    /// one's inference, as a call argument is, so kotlinc lets its sibling decide, as it does under
    /// a call argument's expectation (`sink(if (c) materialize() else B())` binds `T` to `B`) or
    /// none. An expectation of `Any` or `Any?` constrains nothing and leaves the sibling to decide
    /// too.
    pub(super) fn expectation_fixes_branches(
        &self,
        scope: &CheckerScope<'_>,
        conditional: ExprId,
        expected: Option<Ty>,
    ) -> Option<Ty> {
        let declared = self
            .expectation_frames
            .last()
            .is_some_and(|frame| frame.expression == conditional && frame.declared);
        usable_expected(scope, expected).filter(|ty| {
            declared
                && !matches!(ty.non_null(), Ty::Obj(name, _)
                    if crate::types::same(name, crate::types::wk::any()))
        })
    }

    /// Check `e` against a declared expectation: a declaration's type, an assignment target's, or
    /// a return's.
    pub(super) fn expr_declared(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        expected: Ty,
    ) -> Ty {
        self.expected_declared = true;
        let checked = self.expr_expected(scope, e, expected);
        // A lambda converted to a SAM type is checked without consuming the mark.
        self.expected_declared = false;
        checked
    }

    /// Whether a block being checked against a declared expectation forwards it to its result
    /// expression: a lambda body's `run { decode() ?: fallback }` elvis keeps the declared type.
    /// A conditional's branch is not declared itself, so a block there forwards nothing.
    pub(super) fn block_forwards_declared_expectation(&self) -> bool {
        self.expectation_frames.last().is_some_and(|frame| {
            frame.declared && matches!(self.file.expr(frame.expression), Expr::Block { .. })
        })
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
        let Some(signature) = self
            .conditional_call_result_signature(branch)
            .map(std::borrow::Cow::into_owned)
        else {
            return current;
        };
        // Equal constructors, a subtype result (`linkedSetOf()` beside `hashSetOf<T>()`), a more
        // specific sibling (`emptyList()` beside `mutableListOf("a")`), or a shared generic
        // supertype (`linkedSetOf()` beside `arrayListOf<T>()`). The expectation is a face of the
        // sibling, not a collection-specific approximation.
        let expectation = {
            let source = self.fed_source();
            let Some(expectation) = crate::symbol_resolver::generic_return_expectation_from_sibling(
                &source,
                &signature,
                sibling,
                |actual, bound| self.receiver_is_assignable(actual, bound),
            ) else {
                return current;
            };
            expectation
        };
        crate::trace_compiler!(
            "expected_call",
            "conditional branch {branch:?} rebinds against sibling {sibling:?} as {expectation:?}"
        );
        recheck(self, expectation)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConditionalCompletionOwner {
    BranchResults,
    EnclosingExpression,
    SelectedCall(ExprId),
}

/// One expression being checked, and whether its expectation is declared.
pub(super) struct ExpectationFrame {
    expression: ExprId,
    declared: bool,
}

impl ExpectationFrame {
    pub(super) fn new(expression: ExprId, declared: bool) -> Self {
        Self {
            expression,
            declared,
        }
    }
}
