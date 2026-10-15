//! Parser-owned initializer shape facts used by stable declaration headers.

use super::{Expr, ExprId, File, PropDecl, UnOp};

impl File {
    /// Whether `property` is initialized by a literal: a number (with optional unary sign), string,
    /// char, boolean or `null` written directly, as kotlinc's raw FIR keeps a literal expression.
    pub(crate) fn literal_initializer(&self, property: &PropDecl) -> bool {
        let number = |expression: ExprId| {
            matches!(
                self.expr_arena.get(expression.0 as usize),
                Some(
                    Expr::IntLit(_)
                        | Expr::LongLit(_)
                        | Expr::UIntLit(_)
                        | Expr::ULongLit(_)
                        | Expr::DoubleLit(_)
                        | Expr::FloatLit(_)
                )
            )
        };
        let Some(initializer) = property.init else {
            return false;
        };
        match self.expr_arena.get(initializer.0 as usize) {
            Some(Expr::Unary {
                op: UnOp::Plus | UnOp::Neg,
                operand,
            }) => number(*operand),
            Some(Expr::StringLit(_) | Expr::CharLit(_) | Expr::BoolLit(_) | Expr::NullLit) => true,
            _ => number(initializer),
        }
    }
}
