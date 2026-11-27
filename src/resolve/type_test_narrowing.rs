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
        let mut narrowed = self.type_test_target(stable_ty, tt, nullable, ty.targs.is_empty());
        // Only stable values smart-cast soundly — a `var`/computed property could change. A local
        // only a capturing closure mutates stays stable for a target every such write conforms to.
        let closure_stable = stable_ty.is_none()
            && match self.closure_mutated_decline(scope, &path, site) {
                Some((name, declared)) => {
                    let target =
                        self.type_test_target(Some(declared), tt, nullable, ty.targs.is_empty());
                    match target {
                        Some(target) if self.closure_writes_conform(&name, site, target) => {
                            narrowed = Some(target);
                            true
                        }
                        _ => {
                            if let Some(narrowed) = narrowed {
                                declined.push((name, narrowed));
                            }
                            false
                        }
                    }
                }
                None => false,
            };
        if stable_ty.is_none() && !closure_stable {
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

impl Checker<'_> {
    /// The declared constituent a type-test proof keeps beside the proven type, if any. This is
    /// the single comparability rule for every stable value, `this` included.
    ///
    /// kotlinc types a smart cast as the intersection of the declared type and the proof, then
    /// drops a constituent that is a supertype of another. So `is B` on a value declared `A`
    /// keeps `A & B` unless one is a subtype of the other: a type-parameter declaration `M : A`
    /// stays `M & B`, and `Source<T>` proven `IntBox : Source<Int>` stays `Source<T> & IntBox`
    /// (its `value` is both a `T` and an `Int`). A non-null proof strips the declaration's `?`;
    /// a nullable test (`is B?`) still admits null, so the declaration keeps it. `Nothing?` is
    /// the exact null branch, never widened by re-attaching the declared classifier.
    pub(super) fn declared_constituent_beside_proof(&self, declared: Ty, proof: Ty) -> Option<Ty> {
        if proof.non_null() == Ty::Nothing {
            return None;
        }
        let declared = if proof.is_nullable() {
            declared
        } else {
            declared.non_null()
        };
        let context = crate::assignable::TyCtx::new();
        let comparable = crate::assignable::is_subtype(&context, self, declared, proof)
            || crate::assignable::is_subtype(&context, self, proof, declared);
        (!comparable).then_some(declared)
    }
}
