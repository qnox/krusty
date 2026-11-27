//! The type statements a condition proves for the region it guards.
//!
//! One collection walks the condition's shape (`!`, `&&`/`||`, a comparison with a Boolean
//! literal) down to the leaves each proof kind answers for: a contract effect of a call, the
//! implications of a stable value the condition reads, a smart cast and an `as` cast. The result
//! keeps only the minimal set of types proved for each path.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    pub(super) fn condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        truth: bool,
    ) -> (Vec<FlowNarrowing>, Vec<DeclinedFlowNarrowing>) {
        let mut candidates = Vec::new();
        let mut declined = Vec::new();
        self.collect_condition_narrowings(scope, cond, truth, &mut candidates, &mut declined);
        let nonnull: crate::fx_hash::HashSet<NarrowPath> = candidates
            .iter()
            .filter_map(|(path, ty)| (!ty.is_nullable()).then_some(path.clone()))
            .collect();
        let mut result: Vec<(NarrowPath, Ty)> = Vec::new();
        for (path, ty) in candidates {
            let ty = match ty.kind() {
                TyKind::Nullable(inner) if nonnull.contains(&path) && !inner.is_unsigned() => inner,
                _ => ty,
            };
            let matching = result
                .iter()
                .enumerate()
                .filter_map(|(index, (existing, current))| {
                    (*existing == path).then_some((index, *current))
                })
                .collect::<Vec<_>>();
            if matching.is_empty() {
                result.push((path, ty));
                continue;
            }
            let context = crate::assignable::TyCtx::new();
            // Keep the minimal set of proven types. Comparable facts collapse to the narrower one;
            // unrelated facts form a real flow intersection and must both survive. Treating that
            // case as a conflict discarded `A & B` entirely and made every member unresolved.
            if matching
                .iter()
                .any(|(_, current)| crate::assignable::is_subtype(&context, self, *current, ty))
            {
                continue;
            }
            let dominated = matching
                .into_iter()
                .filter_map(|(index, current)| {
                    crate::assignable::is_subtype(&context, self, ty, current).then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            if !dominated.is_empty() {
                result = result
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, entry)| (!dominated.contains(&index)).then_some(entry))
                    .collect();
            }
            result.push((path, ty));
        }
        (result, declined)
    }

    pub(super) fn collect_condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        truth: bool,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        // `!cond` flips the branch facts: the narrowings of `cond`-false hold when `!cond` is true
        // (and vice versa). Recursing with a flipped `truth` also yields De Morgan for `!(a && b)` /
        // `!(a || b)` through the combine rule below.
        if let Expr::Unary {
            op: UnOp::Not,
            operand,
        } = self.file.expr(cond).clone()
        {
            self.collect_condition_narrowings(scope, operand, !truth, out, declined);
            return;
        }
        if let Expr::Binary { op, lhs, rhs, .. } = self.file.expr(cond).clone() {
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                for (condition, literal) in [(lhs, rhs), (rhs, lhs)] {
                    let Expr::BoolLit(literal) = self.file.expr(literal) else {
                        continue;
                    };
                    if self.expr_types[condition.0 as usize].non_null() != Ty::Boolean {
                        continue;
                    }
                    let condition_truth = if (op == BinOp::Eq) == truth {
                        *literal
                    } else {
                        !*literal
                    };
                    self.collect_condition_narrowings(
                        scope,
                        condition,
                        condition_truth,
                        out,
                        declined,
                    );
                    // Projecting the Boolean value must not discard facts established by the
                    // equality expression itself. In particular, a true `x?.p == true` also
                    // proves that the safe-call receiver `x` was non-null.
                    out.extend(self.smartcast_narrowings(scope, cond, !truth, declined));
                    return;
                }
            }
            let combines = (truth && op == BinOp::And) || (!truth && op == BinOp::Or);
            if combines {
                self.collect_condition_narrowings(scope, lhs, truth, out, declined);
                self.collect_condition_narrowings(scope, rhs, truth, out, declined);
                return;
            }
        }
        self.contract_condition_narrowings(scope, cond, truth, out, declined);
        self.implied_condition_narrowings(scope, cond, truth, out, declined);
        out.extend(self.smartcast_narrowings(scope, cond, !truth, declined));
        self.as_cast_narrowings(scope, cond, out);
    }
}
