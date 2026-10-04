//! Where a value Java did not check for `null` is committed to a type that rejects it.
//!
//! kotlinc inserts its implicit not-null cast in `Fir2IrImplicitCastInserter.insertSpecialCast`: a
//! value whose type is flexible (`T!`) or carries `EnhancedNullability` is guarded wherever the
//! declared expected type does not accept `null`. The recorded positions become
//! `Intrinsics.checkNotNullExpressionValue` on the JVM.

use super::{Checker, PlatformNarrowing, ResolvedCall};
use crate::ast::{Expr, ExprId};
use crate::libraries::ResultEnhancement;
use crate::types::Ty;

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
            _ => {}
        }
        self.has_enhanced_call_result(e)
    }

    fn branch_has_enhanced_result(&self, branch: ExprId) -> bool {
        match self.file.expr(branch) {
            Expr::Block {
                stmts,
                trailing: Some(trailing),
            } if stmts.is_empty() => self.has_enhanced_result(*trailing),
            Expr::Block { .. } => false,
            _ => self.has_enhanced_result(branch),
        }
    }

    fn has_enhanced_call_result(&self, e: ExprId) -> bool {
        let call_sig = match self.resolved_calls.get(&e) {
            Some(ResolvedCall::Member(member)) => &member.member.call_sig,
            Some(ResolvedCall::Companion(member)) => &member.call_sig,
            Some(ResolvedCall::TopLevel(call)) => &call.call_sig,
            _ => return false,
        };
        call_sig.result_enhancement == ResultEnhancement::NotNull
    }
}
