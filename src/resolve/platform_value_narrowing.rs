//! Where a value Java did not check for `null` is committed to a type that rejects it.
//!
//! kotlinc inserts its implicit not-null cast in `Fir2IrImplicitCastInserter.insertSpecialCast`: a
//! value whose type is flexible (`T!`) or carries `EnhancedNullability` is guarded wherever the
//! declared expected type does not accept `null`. The recorded positions become
//! `Intrinsics.checkNotNullExpressionValue` on the JVM.

use super::{Checker, PlatformNarrowing, ResolvedCall, TypeInfo};
use crate::ast::{Expr, ExprId};
use crate::libraries::ResultEnhancement;
use crate::symbol_resolver::ResolvedMember;
use crate::types::Ty;
use std::collections::HashMap;

impl Checker<'_> {
    /// Record that `e`'s PLATFORM type is committed to a declared non-null `expected` here.
    ///
    /// A Java value arrives as `T!`, which is usable as both `T` and `T?`; a declared non-null type
    /// is where the source picks the non-null bound and every later consumer — Kotlin call sites that
    /// skip null handling, Java nullness checkers reading `@NotNull`, krusty's own smart casts — is
    /// entitled to rely on it. kotlinc guards exactly this transition (see
    /// [`super::TypeInfo::platform_narrowings`]), unboxing into a primitive included; positions that
    /// keep the flexibility (`T?`, another `T!`) are left alone. A Java result enhanced to not-null
    /// ([`ResultEnhancement::NotNull`]) is typed `T`, but Java never checked it either, so it is
    /// guarded at the same positions.
    ///
    /// The expression's RECORDED type is read rather than a caller-supplied one: lowering consumes
    /// the same `expr_types` entry, so a caller that has already narrowed the type for a diagnostic
    /// cannot make the two disagree.
    pub(super) fn narrow_platform_value(
        &mut self,
        expected: Ty,
        e: ExprId,
        position: PlatformNarrowing,
    ) {
        if !matches!(self.expr_types[e.0 as usize], Ty::PlatformNullable(_))
            && !self.has_enhanced_result(e)
        {
            return;
        }
        if matches!(expected, Ty::PlatformNullable(_) | Ty::Error)
            || expected.is_nullable()
            || !(expected.is_reference() || expected.is_jvm_scalar())
        {
            return;
        }
        self.platform_narrowings.insert(e, position);
    }

    /// An enhanced Java result initializing a declaration whose type is inferred from it.
    ///
    /// kotlinc's expected type there is the declaration's own type, which is the initializer's
    /// type without its `EnhancedNullability` attribute, so the value is guarded on the way in
    /// (`val s = sb.toString()`). A flexible initializer gives the declaration a flexible type,
    /// which accepts `null`, and a conditional keeps the attribute on the declaration; both stay
    /// unguarded.
    pub(super) fn narrow_enhanced_value(&mut self, expected: Ty, value: ExprId) {
        if self.has_enhanced_call_result(value) {
            self.narrow_platform_value(expected, value, PlatformNarrowing::Declaration);
        }
    }

    /// Whether `e` produces an enhanced result: a call whose selected declaration's result is
    /// enhanced to not-null, or a value conditional one of whose branches produces one (a common
    /// supertype keeps the attribute). A declared type reaching the conditional then guards its
    /// branches where they produce their values.
    fn has_enhanced_result(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                return self.branch_has_enhanced_result(*then_branch)
                    || self.branch_has_enhanced_result(*else_branch);
            }
            Expr::When { arms, .. } => {
                return arms
                    .iter()
                    .any(|arm| self.branch_has_enhanced_result(arm.body));
            }
            // `when (val x = ...)` is a block declaring the subject before the `when`.
            Expr::Block { .. } => {
                let produced = super::conditional_branch::branch_value_expression(self.file, e);
                return produced != e && self.has_enhanced_result(produced);
            }
            _ => {}
        }
        self.has_enhanced_call_result(e)
    }

    /// A branch produces its value through its trailing expression, past any statements and
    /// nested blocks before it.
    fn branch_has_enhanced_result(&self, branch: ExprId) -> bool {
        self.has_enhanced_result(super::conditional_branch::branch_value_expression(
            self.file, branch,
        ))
    }

    fn has_enhanced_call_result(&self, e: ExprId) -> bool {
        enhanced_call_result(&self.resolved_calls, e)
    }

    /// The selected member's value parameters as the member DECLARES them for this receiver: the
    /// class's type arguments applied, the call's own type arguments not.
    ///
    /// kotlinc's implicit not-null cast reads an argument's expected type from the unsubstituted
    /// callee parameter, where a method type parameter with a nullable bound accepts `null`
    /// (`Box<String>().put(v, extra)` with `fun <R> put(value: T, extra: R)` guards `v` but not
    /// `extra`). `visible` is the parameter list after contextual reordering by `indices`; a
    /// non-generic member declares exactly those types.
    pub(super) fn declared_member_params(
        &self,
        selected: &ResolvedMember,
        indices: &[usize],
        visible: &[Ty],
    ) -> Vec<Ty> {
        let Some(signature) = selected
            .member
            .generic_sig
            .as_ref()
            .filter(|signature| signature.params.len() == selected.member.params.len())
        else {
            return visible.to_vec();
        };
        let bindings = self.member_receiver_type_bindings(selected.receiver, &selected.member);
        indices
            .iter()
            .zip(visible)
            .map(|(&parameter, &visible)| {
                signature
                    .params
                    .get(parameter)
                    .map_or(visible, |&declared| {
                        crate::symbol_resolver::ty_subst_keep_unbound(declared, &bindings)
                    })
            })
            .collect()
    }
}

impl TypeInfo {
    /// Whether `e` itself produces a value Java never checked: a flexible `T!` or a call result
    /// enhanced to not-null.
    ///
    /// A conditional committed to a declared type is guarded branch by branch, and kotlinc's
    /// implicit cast lands only where a branch's own value is such a value: a sibling branch that
    /// produces a Kotlin `String` stays unchecked.
    pub(crate) fn produces_unchecked_java_value(&self, e: ExprId) -> bool {
        matches!(self.expr_types[e.0 as usize], Ty::PlatformNullable(_))
            || enhanced_call_result(&self.resolved_calls, e)
    }
}

fn enhanced_call_result(resolved_calls: &HashMap<ExprId, ResolvedCall>, e: ExprId) -> bool {
    let call_sig = match resolved_calls.get(&e) {
        Some(ResolvedCall::Member(member)) => &member.member.call_sig,
        Some(ResolvedCall::Companion(member)) => &member.call_sig,
        Some(ResolvedCall::TopLevel(call)) => &call.call_sig,
        _ => return false,
    };
    call_sig.result_enhancement == ResultEnhancement::NotNull
}
