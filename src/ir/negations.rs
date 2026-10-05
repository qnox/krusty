//! Boolean negation (kotlinc's `Boolean.not()`).
//!
//! Common IR spells a negation as the primitive comparison `operand == false`, the shape every
//! target can already evaluate. That shape alone does not say whether the comparison is a
//! negation or an equality with a literal, and kotlinc realizes the two differently: `not` only
//! flips the sense of its operand's jump, while `==` (`BooleanComparison`) materializes both
//! operands. Every producer of a negation therefore builds it here, which records it as one.

use super::{ExprId, IrBinOp, IrConst, IrExpr, IrFile};

impl IrFile {
    /// Build the negation of the Boolean `operand` and record it as one.
    pub fn add_negation(&mut self, operand: ExprId) -> ExprId {
        let negation = self.negation_of(operand);
        let negation = self.add_expr(negation);
        self.negations.insert(negation);
        negation
    }

    /// Replace the node `destination` with the negation of the Boolean `operand` and record it as
    /// one, so the replacement and its provenance are committed together (a target realizing an
    /// exact `Boolean.not()` declaration in place of its call).
    pub fn replace_with_negation(&mut self, destination: ExprId, operand: ExprId) {
        let negation = self.negation_of(operand);
        self.exprs[destination as usize] = negation;
        self.negations.insert(destination);
    }

    fn negation_of(&mut self, operand: ExprId) -> IrExpr {
        let false_value = self.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: operand,
            rhs: false_value,
        }
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
