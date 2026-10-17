//! Declaration-owned and current value identities for mutable locals in compact signatures.

use super::{SigExpr, SigExprId, SignatureConstraintExtractor};

impl SignatureConstraintExtractor {
    /// Retain the initializer/type identity of a mutable local beside its current flow value.
    pub(super) fn declared_local_flow(&mut self, value: SigExprId, mutable: bool) -> SigExprId {
        if mutable {
            self.graph.add_expr(SigExpr::MutableLocalFlow {
                current: value,
                declared: value,
            })
        } else {
            value
        }
    }

    /// Replace only the current value while preserving the declaration-owned publication value.
    pub(super) fn updated_mutable_local(
        &mut self,
        previous: SigExprId,
        current: SigExprId,
    ) -> SigExprId {
        let Some(SigExpr::MutableLocalFlow { declared, .. }) = self.graph.expr(previous) else {
            return current;
        };
        self.graph
            .add_expr(SigExpr::MutableLocalFlow { current, declared })
    }
}
