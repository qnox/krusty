//! kotlinc's `PatchLambdaOffsetsLowering`: a lambda literal takes the position of the nearest call
//! that encloses it, so stepping stops on the call, not on the line the lambda opens on.
//!
//! The call is the nearest one on the path from the lambda up to its enclosing declaration,
//! however deep the lambda sits in its arguments (through a constructor call, a conditional or a
//! block). A local variable's initializer is a declaration of its own there, and so is a lambda's
//! or a local function's body, which this pass lowers with a fresh [`BodyLowering`]. A lambda with
//! no enclosing call keeps its own position, and so do an anonymous function (`fun() { }`) and a
//! lambda converted to a functional interface, whose instance kotlinc creates at the lambda.

use super::BodyLowering;
use crate::fir::{FirExpr, FirExprId, FirExprKind, FirLambdaForm, FirStatementId};
use crate::ir::{ExprId, IrExpr};

impl BodyLowering<'_> {
    /// The line a lambda literal written under `expression` takes: the expression's own line when
    /// it is a call (kotlinc's `IrCall`, a property setter and an array factory included), else
    /// the line of the call enclosing it.
    pub(super) fn lambda_call_line_within(
        &self,
        expression_id: FirExprId,
        expression: &FirExpr,
    ) -> Option<u32> {
        let call = matches!(
            expression.kind,
            FirExprKind::Call(_)
                | FirExprKind::LocalCall { .. }
                | FirExprKind::FunctionInvoke { .. }
                | FirExprKind::ComparisonCall { .. }
                | FirExprKind::ContainmentCall { .. }
                | FirExprKind::PropertyWrite { .. }
                | FirExprKind::IndexedWrite { .. }
                | FirExprKind::ArrayLiteral { .. }
        );
        if !call {
            return self.lambda_call_line;
        }
        // A write recording no line of its own is positioned by its assignment statement.
        Some(self.body.expression_debug_lines(expression_id).source)
            .filter(|&line| line != 0)
            .or(self.lambda_call_line)
    }

    /// The line of the call assignment statement `statement` makes (a property setter, an
    /// operator `set` call or an array store), when that call records no line of its own: the
    /// assignment's.
    pub(super) fn assignment_call_line(
        &self,
        statement: FirStatementId,
        expression: FirExprId,
    ) -> Option<u32> {
        let mut write = self.body.expr(expression)?;
        // A `set` call returning a value is discarded through a conversion to `Unit`.
        if let FirExprKind::ImplicitConversion { value, .. } = write.kind {
            write = self.body.expr(value)?;
        }
        let assignment = matches!(
            write.kind,
            FirExprKind::PropertyWrite { .. }
                | FirExprKind::IndexedWrite { .. }
                | FirExprKind::Call(_)
        );
        let own = self.body.expression_debug_lines(expression).source;
        let line = self.body.statement_debug_line(statement);
        (assignment && own == 0 && line != 0).then_some(line)
    }

    /// The line `lowered`, the lowering of lambda literal `expression`, is positioned at:
    /// the enclosing call's when it has one, else `own`.
    pub(super) fn lambda_literal_line(
        &self,
        expression: &FirExpr,
        lowered: ExprId,
        own: u32,
    ) -> u32 {
        let FirExprKind::Lambda { body, .. } = &expression.kind else {
            return own;
        };
        let literal = body
            .source_lambda()
            .is_some_and(|lambda| lambda.form() == FirLambdaForm::Literal);
        let converted = !matches!(
            self.ir.exprs[lowered as usize],
            IrExpr::Lambda { sam: None, .. }
        );
        match self.lambda_call_line {
            Some(line) if literal && !converted => line,
            _ => own,
        }
    }
}
