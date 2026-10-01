//! Conditional-result joining and branch-value discovery.

use super::{Checker, CheckerScope};
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
            IntegerConstant::Signed(_) => Ty::Int,
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

impl Checker<'_> {
    fn conditional_call_result_signature(&self, expression: ExprId) -> Option<&GenericSig> {
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
            .then_some(signature)
    }

    /// Report a conditional branch whose selected generic call remains symbolic after sibling
    /// rebinding. A call defaulted to its formal's bound has no symbolic remainder and is not
    /// diagnosed here.
    pub(super) fn report_unbound_conditional_branch(
        &mut self,
        scope: &CheckerScope<'_>,
        branch: ExprId,
    ) {
        if self.postponed_argument_depth != 0 {
            return;
        }
        let Some(signature) = self.unbound_call_result_signature(branch).cloned() else {
            return;
        };
        let actual = self.expr_types[branch.0 as usize];
        if Self::type_is_lexically_fixed(scope, actual) {
            return;
        }
        let Some(formal) = signature
            .formals
            .iter()
            .find(|formal| crate::types::ty_mentions_param(actual, std::slice::from_ref(formal)))
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
        let Some(signature) = self.conditional_call_result_signature(branch).cloned() else {
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
