//! Narrowings proven by a null check on an expression.

use crate::ast::{Expr, ExprId};
use crate::types::Ty;

use super::scope::NarrowPath;
use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// Narrowings from a `!= null`/`== null` proof on `operand`: the operand's whole ACCESS PATH
    /// (`a.b != null` proves `a.b` non-null) — and, for a SAFE-CALL chain (`a?.b?.c != null`),
    /// every prefix too, since a non-null safe-call result means every link held (kotlinc narrows
    /// the root receiver in the branch). Only stable paths narrow soundly.
    pub(super) fn null_check_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        operand: ExprId,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        let Some(path) = self.expr_access_path(operand) else {
            // Not a property path. A SAFE-CALL chain ending in a method (`u?.f() != null`,
            // `u?.f() ?: return`) is no property read, but a non-null result still proves the
            // chain's ROOT receiver non-null — it only completes when every `?.` held.
            let mut root = operand;
            while let Expr::SafeCall { receiver, .. } = self.file.expr(root) {
                root = *receiver;
            }
            if let Some(root_path) = self
                .expr_access_path(root)
                .filter(|path| path.segments.is_empty())
            {
                self.null_proof_or_decline(scope, &root_path, out, declined, self.span(operand));
            }
            // `x as? T` is non-null exactly when the cast succeeded, so a proof on the chain's
            // root narrows `x` to `T`.
            self.safe_cast_non_null_narrowing(scope, root, out, declined);
            return;
        };
        // A plain chain's proof covers only the full path; a safe-call chain's covers every prefix.
        let shortest = if matches!(self.file.expr(operand), Expr::SafeCall { .. }) {
            0
        } else {
            path.segments.len()
        };
        for len in (shortest..=path.segments.len()).rev() {
            let prefix = NarrowPath {
                root: path.root.clone(),
                segments: path.segments[..len].to_vec(),
            };
            self.null_proof_or_decline(scope, &prefix, out, declined, self.span(operand));
        }
    }
}
