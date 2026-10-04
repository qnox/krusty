//! Casts that have already run, for a later operand of the same expression.
//!
//! An eager operator evaluates its left operand completely before the right, so a non-null `as`
//! there has run. `&&` and `||` do not: their right operands can be skipped.

use crate::ast::{Expr, ExprId, StmtId};
use crate::types::{FnSig, Ty};

use super::scope::{NarrowPath, ScopeKind};
use super::{Checker, CheckerScope};

/// The ordinary `FunctionN+1` shape a suspend function value implements at run time: the value
/// parameters, a trailing `Continuation` of the suspend result, and a nullable `Any` result.
pub(super) fn continuation_function_type(signature: &FnSig) -> Ty {
    let mut parameters = signature.params.to_vec();
    parameters.push(Ty::obj_args(
        "kotlin/coroutines/Continuation",
        &[signature.ret],
    ));
    Ty::fun(parameters, Ty::nullable(Ty::obj("kotlin/Any")))
}

impl Checker<'_> {
    /// Collect checked-cast facts from subexpressions that certainly ran when `expression` ran.
    pub(super) fn as_cast_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        let mut evaluated = Vec::new();
        crate::ast::definitely_evaluated::for_each_in_expression(
            self.file,
            expression,
            &mut |expression| evaluated.push(expression),
        );
        for expression in evaluated {
            self.append_as_cast_narrowing(scope, expression, out);
        }
    }

    /// Checked-cast facts established by every normally completing path through `statement`.
    pub(super) fn as_cast_narrowings_after_statement(
        &self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        let mut evaluated = Vec::new();
        crate::ast::definitely_evaluated::for_each_in_statement(
            self.file,
            statement,
            &mut |expression| evaluated.push(expression),
        );
        for expression in evaluated {
            self.append_as_cast_narrowing(scope, expression, out);
        }
    }

    fn append_as_cast_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        let Expr::As {
            operand,
            ty,
            nullable: false,
        } = self.file.expr(expression)
        else {
            return;
        };
        let Some(path) = self.expr_access_path(*operand) else {
            return;
        };
        let Some(stable_ty) = self.stable_path_ty(scope, &path, self.span(expression)) else {
            return;
        };
        if let Some(narrowed) = self.proven_narrowed_ty(scope, Some(stable_ty), ty) {
            out.push((path, narrowed));
        }
    }

    /// Scope for the right operand of an eager operator. `None` when the left operand ran no
    /// non-null `as`.
    pub(super) fn evaluated_cast_scope<'a>(
        &mut self,
        scope: &'a CheckerScope<'_>,
        lhs: ExprId,
    ) -> Option<CheckerScope<'a>> {
        let mut casts = Vec::new();
        self.as_cast_narrowings(scope, lhs, &mut casts);
        if casts.is_empty() {
            return None;
        }
        let rhs_scope = scope.child(ScopeKind::Block);
        self.apply_narrowings(&rhs_scope, &casts, &[], false);
        Some(rhs_scope)
    }

    /// Exact function values proved for `expression` besides its current read type.
    ///
    /// A cast to the continuation-passing carrier leaves that carrier as the read projection and
    /// keeps the original suspend value in the intersection. The local's callable-reference
    /// signature is the same kind of fact. A nominal classifier's `operator invoke` is not: this
    /// walk never asks a classifier for callable signatures.
    pub(super) fn proven_function_value_facts(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> Vec<Ty> {
        let mut facts = Vec::new();
        let mut push = |ty: Ty| {
            let ty = ty.non_null();
            if matches!(ty, Ty::Fun(_)) && !facts.contains(&ty) {
                facts.push(ty);
            }
        };
        let Some(path) = self.expr_access_path(expression) else {
            return facts;
        };
        for constituent in self.lookup_intersection_narrowing(scope, &path) {
            push(constituent);
        }
        if path.segments.is_empty() {
            if let super::scope::PathRoot::Value(identity) = path.root {
                if let Some((_, local)) = self.visible_flow_value(scope, identity) {
                    if let Some(function) = local.callable_reference_type {
                        push(function);
                    }
                }
            }
        }
        facts
    }

    /// `fact` is one exact function value. A suspend value also implements its continuation-passing
    /// carrier; that carrier is not a subtype, so both directions are spelled here.
    pub(super) fn function_value_fact_matches(&self, fact: Ty, proof_target: Ty) -> bool {
        let Ty::Fun(signature) = fact.non_null() else {
            return false;
        };
        self.receiver_is_assignable(fact.non_null(), proof_target)
            || (signature.suspend
                && self.receiver_is_assignable(
                    continuation_function_type(signature),
                    proof_target.non_null(),
                ))
    }

    /// A later `is SuspendFunction` / `is KSuspendFunction`, or a use that expects the suspend
    /// type, sees every constituent of the cast intersection. The CPS read projection alone would
    /// reject the suspend check as erased.
    pub(super) fn suspend_intersection_proves_check(
        &self,
        scope: &CheckerScope<'_>,
        operand: ExprId,
        target: Ty,
        proof_target: Option<Ty>,
    ) -> bool {
        let constituents = self
            .expr_access_path(operand)
            .map(|path| self.lookup_intersection_narrowing(scope, &path))
            .unwrap_or_default();
        if constituents
            .iter()
            .any(|constituent| self.receiver_is_assignable(constituent.non_null(), target))
        {
            return true;
        }
        let Some(proof_target) = proof_target else {
            return false;
        };
        self.proven_function_value_facts(scope, operand)
            .into_iter()
            .any(|fact| self.function_value_fact_matches(fact, proof_target))
    }
}
