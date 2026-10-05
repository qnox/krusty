//! Boolean negation (kotlinc's `Boolean.not()`).
//!
//! Common IR spells a negation as the primitive comparison `operand == false`, the shape every
//! target can already evaluate. That shape alone does not say whether the comparison is a
//! negation or an equality with a literal, and kotlinc realizes the two differently: `not` only
//! flips the sense of its operand's jump, while `==` (`BooleanComparison`) materializes both
//! operands. Lowering therefore records which comparisons are negations.

use super::{ExprId, IrBinOp, IrConst, IrExpr, IrFile};

impl IrFile {
    /// Build the negation of the Boolean `operand` and record it as one.
    pub fn add_negation(&mut self, operand: ExprId) -> ExprId {
        let false_value = self.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let negation = self.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: operand,
            rhs: false_value,
        });
        self.negations.insert(negation);
        negation
    }

    /// The operand of `expression` when it is a recorded negation.
    pub fn negated_operand(&self, expression: ExprId) -> Option<ExprId> {
        if !self.negations.contains(&expression) {
            return None;
        }
        let IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs,
            ..
        } = self.expr(expression)
        else {
            panic!("a recorded negation must be a primitive comparison with `false`");
        };
        Some(*lhs)
    }
}
