//! Smart casts proven by a successful type test: `value is T`, and a non-null `value as? T`.
//!
//! A safe cast is `null` exactly when its operand is not a `T`, so any proof that its result is
//! non-null (`!= null`, `?: return`, a completed safe call on it) proves `value is T` as well.

use super::*;

/// One type test whose success the caller has proven.
pub(super) struct TypeTest<'a> {
    /// The tested value.
    pub(super) operand: ExprId,
    /// The written target type.
    pub(super) ty: &'a TypeRef,
    /// Whether the proven test admits `null` (`is T?`).
    pub(super) nullable: bool,
    /// The source span of the proof, for the stability check.
    pub(super) site: Span,
}

impl Checker<'_> {
    /// Narrowings from a successful `test`: the operand's stable access path becomes the tested
    /// type. A proof declined only because a capturing closure mutates the variable is recorded in
    /// `declined` so the use that needed it can report kotlinc's SMARTCAST_IMPOSSIBLE.
    pub(super) fn type_test_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        test: TypeTest<'_>,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        let TypeTest {
            operand,
            ty,
            nullable,
            site,
        } = test;
        let Some(path) = self.expr_access_path(operand) else {
            crate::trace_compiler!(
                "smartcast",
                "type test operand={} rejected: operand has no stable access path",
                operand.0,
            );
            return;
        };
        // The target is computed before the stability gate so a DECLINED proof can still name it.
        let stable_ty = self.stable_path_ty(scope, &path, site);
        let tt = self
            .resolved_type_tys
            .get(&(ty.span.lo, ty.span.hi))
            .copied()
            .unwrap_or_else(|| self.type_ref_ty_silent(scope, ty));
        // Only a safe cast proves a non-null test against a nullable written type (`x as? T?`).
        let tt = if !nullable && ty.nullable() {
            tt.non_null()
        } else {
            tt
        };
        let narrowed = self.type_test_target(stable_ty, tt, nullable, ty.targs.is_empty());
        // Only stable values smart-cast soundly — a `var`/computed property could change.
        if stable_ty.is_none() {
            if let Some(narrowed) = narrowed {
                if let Some((name, _)) = self.closure_mutated_decline(scope, &path, site) {
                    declined.push((name, narrowed));
                }
            }
            crate::trace_compiler!(
                "smartcast",
                "type test operand={} path={path:?} rejected: unstable",
                operand.0,
            );
            return;
        }
        if let Some(narrowed) = narrowed {
            crate::trace_compiler!(
                "smartcast",
                "type test operand={} path={path:?} target={narrowed:?}",
                operand.0,
            );
            out.push((path, narrowed));
            // A non-null type test can succeed on a safe-call chain only when every guarded
            // receiver was non-null. Publish those prefix facts as part of the same proof:
            // `holder?.value is T` permits an ordinary `holder.value` read in the guarded branch.
            // Nullable tests (`is T?`) deliberately do not imply this because `null is T?` holds.
            if !nullable && matches!(self.file.expr(operand), Expr::SafeCall { .. }) {
                self.null_check_narrowings(scope, operand, out, declined);
            }
        } else {
            crate::trace_compiler!(
                "smartcast",
                "type test operand={} path={path:?} rejected: target={tt:?}",
                operand.0,
            );
        }
    }

    /// The type test a non-null `value` proves when it is a safe cast `operand as? T`: the cast
    /// yields `null` for every operand that is not a `T`, including `null` itself, so the proof is
    /// the non-null test `operand is T` even when `T` is written nullable.
    pub(super) fn safe_cast_type_test(&self, value: ExprId) -> Option<TypeTest<'_>> {
        match self.file.expr(value) {
            Expr::As {
                operand,
                ty,
                nullable: true,
            } => Some(TypeTest {
                operand: *operand,
                ty,
                nullable: false,
                site: self.span(value),
            }),
            _ => None,
        }
    }
}
