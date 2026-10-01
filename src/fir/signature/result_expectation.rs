//! Attaching a declaration's expected result type to the deferred selection that owns inference.

use super::*;

impl SignatureGraph {
    /// Attach a declaration's expected result type to the outer deferred selection that owns
    /// contextual generic inference. Both nodes live only in the temporary signature graph. This
    /// is used by an inferred explicit backing field: its initializer is checked against the
    /// property's declared public type exactly as in an ordinary typed initializer, without
    /// retaining either body syntax or a source coordinate.
    pub fn apply_result_expectation(&mut self, result: SigExprId, expected: SigExprId) -> bool {
        match self.expr(result) {
            Some(SigExpr::Value(selection)) => {
                self.value_selections[selection.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Call { target, .. })
            | Some(SigExpr::CallableReference(target))
            | Some(SigExpr::BoundCallableReference { target, .. }) => {
                self.callable_selections[target.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Member { lookup, .. })
            | Some(SigExpr::MemberCall { target: lookup, .. }) => {
                self.member_selections[lookup.raw() as usize].expected = Some(expected);
                true
            }
            Some(SigExpr::Sequence { result, .. })
            | Some(SigExpr::ScopedReceiver { result, .. }) => {
                self.apply_result_expectation(result, expected)
            }
            Some(SigExpr::Join { operands, .. }) => {
                let operands = self.operands(operands).to_vec();
                operands
                    .into_iter()
                    .all(|operand| self.apply_result_expectation(operand, expected))
            }
            _ => false,
        }
    }
}
