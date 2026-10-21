//! Whether an inline lambda's body leaves the lambda by a non-local `return`, `break` or
//! `continue`.
//!
//! Common lowering records the fact once for each lambda body: when it routes a same-module
//! inline call, for the call's literal lambdas, and for every other lambda when the file's
//! lowering is finished. Backends read the recorded fact and never scan a body themselves.

use crate::fx_hash::HashMap;

use super::{ExprId, IrCheckedOperation, IrExpr, IrFile};

#[derive(Default)]
pub(super) struct LambdaExits {
    /// Inline lambda body → whether it leaves the lambda non-locally.
    leaves: HashMap<ExprId, bool>,
}

impl IrFile {
    /// Whether the inline lambda body `body` leaves the lambda non-locally, recording the fact the
    /// first time it is asked. Every `return` in an inline body is non-local (a local return is
    /// lowered to the body's value); a `break` or `continue` is when its loop is outside `body`.
    pub(crate) fn record_lambda_exit(&mut self, body: ExprId) -> bool {
        if let Some(&leaves) = self.lambda_exits.leaves.get(&body) {
            return leaves;
        }
        let leaves = leaves_non_locally(self, body, &mut Vec::new());
        self.lambda_exits.leaves.insert(body, leaves);
        leaves
    }

    /// Record the exit of every inline body of the file that has no recorded exit yet: each
    /// lambda's, and the invocation template of each callable reference an inline call can take.
    pub(crate) fn record_lambda_exits(&mut self) {
        let bodies = (0..self.exprs.len())
            .filter_map(|expression| {
                let expression = ExprId::try_from(expression).ok()?;
                match self.expr(expression) {
                    IrExpr::Lambda {
                        inline_body: Some(body),
                        ..
                    } => Some(*body),
                    IrExpr::CallableReference(_) => self
                        .callable_reference_inline_template(expression)
                        .map(|(_, _, body, _)| body),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        for body in bodies {
            self.record_lambda_exit(body);
        }
    }

    /// The recorded exit of the inline lambda body `body`; `None` when common lowering recorded
    /// none, which no lambda that reaches a backend lacks.
    pub(crate) fn lambda_exits_non_locally(&self, body: ExprId) -> Option<bool> {
        self.lambda_exits.leaves.get(&body).copied()
    }

    /// Give the copy `target` of expression `source` the exit recorded for `source`.
    pub(super) fn copy_lambda_exit(&mut self, source: ExprId, target: ExprId) {
        if let Some(&leaves) = self.lambda_exits.leaves.get(&source) {
            self.lambda_exits.leaves.insert(target, leaves);
        }
    }
}

/// Whether `expression` holds a `return`, or a `break` or `continue` of no loop in `loops` or
/// inside `expression`. A nested lambda's inline body is placed in this one, so its jumps count.
fn leaves_non_locally(ir: &IrFile, expression: ExprId, loops: &mut Vec<String>) -> bool {
    let label = match ir.expr(expression) {
        IrExpr::Return(_) => return true,
        IrExpr::Break { label } | IrExpr::Continue { label } => {
            return match label {
                None => loops.is_empty(),
                Some(label) => !loops.contains(label),
            };
        }
        IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } => return leaves_non_locally(ir, *body, loops),
        IrExpr::While { label, .. } => Some(label.clone().unwrap_or_default()),
        IrExpr::Checked(checked_op)
            if let IrCheckedOperation::RangeLoop { label, .. } = &**checked_op =>
        {
            Some(label.clone())
        }
        _ => None,
    };
    let entered = label.is_some();
    loops.extend(label);
    let mut leaves = false;
    super::for_each_child(&ir.exprs, expression, &mut |child| {
        leaves = leaves || leaves_non_locally(ir, child, loops);
    });
    if entered {
        loops.pop();
    }
    leaves
}

#[cfg(test)]
mod tests {
    use crate::ir::{IrExpr, IrFile};

    fn block(ir: &mut IrFile, stmts: Vec<u32>) -> u32 {
        ir.add_expr(IrExpr::Block { stmts, value: None })
    }

    fn looping(ir: &mut IrFile, body: u32, label: Option<&str>) -> u32 {
        let cond = ir.add_expr(IrExpr::UnitInstance);
        ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test: false,
            label: label.map(str::to_string),
        })
    }

    #[test]
    fn a_return_leaves_the_lambda() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Return(None));
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exits_non_locally(body), Some(true));
    }

    #[test]
    fn a_break_of_a_loop_inside_the_lambda_stays_in_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let inner = looping(&mut ir, exit, None);
        let body = block(&mut ir, vec![inner]);
        assert!(!ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exits_non_locally(body), Some(false));
    }

    #[test]
    fn a_break_or_continue_of_a_loop_outside_the_lambda_leaves_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));

        let next = ir.add_expr(IrExpr::Continue {
            label: Some("outer".to_string()),
        });
        let inner = looping(&mut ir, next, Some("inner"));
        let body = block(&mut ir, vec![inner]);
        assert!(ir.record_lambda_exit(body));
    }
}
