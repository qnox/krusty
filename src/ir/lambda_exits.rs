//! Whether an inline lambda's body leaves the lambda by a non-local `return`, `break` or
//! `continue`, and by which.
//!
//! Common lowering records the fact once for each lambda body: when it routes a same-module
//! inline call, for the call's literal lambdas, and for every other lambda when the file's
//! lowering is finished. Backends read the recorded fact and never scan a body themselves.

use crate::fx_hash::HashMap;

use super::{ExprId, IrCheckedOperation, IrExpr, IrFile};

#[derive(Default)]
pub(super) struct LambdaExits {
    /// Inline lambda body → how it leaves the lambda.
    leaves: HashMap<ExprId, LambdaExit>,
}

/// How an inline lambda's body leaves the lambda.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LambdaExit {
    /// Only through its value.
    Local,
    /// By a non-local `return` and by no `break` or `continue` of an outer loop.
    Return,
    /// By a `break` or `continue` of a loop outside it, whatever else it does.
    Jump,
}

impl IrFile {
    /// Whether the inline lambda body `body` leaves the lambda non-locally, recording the fact the
    /// first time it is asked. Every `return` in an inline body is non-local (a local return is
    /// lowered to the body's value); a `break` or `continue` is when its loop is outside `body`.
    pub(crate) fn record_lambda_exit(&mut self, body: ExprId) -> bool {
        if let Some(&exit) = self.lambda_exits.leaves.get(&body) {
            return exit != LambdaExit::Local;
        }
        let exit = lambda_exit(self, body, &mut Vec::new());
        self.lambda_exits.leaves.insert(body, exit);
        exit != LambdaExit::Local
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

    /// The exit of the inline lambda body `body`; `None` when common lowering recorded none, which
    /// no lambda that reaches a backend lacks. Lowering can rewrite a body after recording it: a
    /// labeled return of an enclosing lambda (`return@outer` in an inner lambda) becomes a `break`
    /// of that lambda's return frame. The body as it stands is walked again and the stronger exit
    /// is kept, so such a body is a jump rather than the return it was recorded as.
    pub(crate) fn lambda_exit(&self, body: ExprId) -> Option<LambdaExit> {
        let recorded = self.lambda_exits.leaves.get(&body).copied()?;
        Some(recorded.max(lambda_exit(self, body, &mut Vec::new())))
    }

    /// Give the copy `target` of expression `source` the exit recorded for `source`.
    pub(super) fn copy_lambda_exit(&mut self, source: ExprId, target: ExprId) {
        if let Some(&leaves) = self.lambda_exits.leaves.get(&source) {
            self.lambda_exits.leaves.insert(target, leaves);
        }
    }
}

/// How `expression` leaves: by a `return`, or by a `break` or `continue` of no loop in `loops` or
/// inside `expression`. A nested lambda's inline body is placed in this one, so its exits count.
fn lambda_exit(ir: &IrFile, expression: ExprId, loops: &mut Vec<String>) -> LambdaExit {
    let label = match ir.expr(expression) {
        IrExpr::Return(_) => return LambdaExit::Return,
        IrExpr::Break { label } | IrExpr::Continue { label } => {
            let outer = match label {
                None => loops.is_empty(),
                Some(label) => !loops.contains(label),
            };
            return if outer {
                LambdaExit::Jump
            } else {
                LambdaExit::Local
            };
        }
        IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } => return lambda_exit(ir, *body, loops),
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
    let mut exit = LambdaExit::Local;
    super::for_each_child(&ir.exprs, expression, &mut |child| {
        if exit != LambdaExit::Jump {
            exit = exit.max(lambda_exit(ir, child, loops));
        }
    });
    if entered {
        loops.pop();
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::LambdaExit;
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
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Return));
    }

    #[test]
    fn a_break_of_a_loop_inside_the_lambda_stays_in_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let inner = looping(&mut ir, exit, None);
        let body = block(&mut ir, vec![inner]);
        assert!(!ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Local));
    }

    #[test]
    fn a_break_or_continue_of_a_loop_outside_the_lambda_leaves_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Jump));

        let next = ir.add_expr(IrExpr::Continue {
            label: Some("outer".to_string()),
        });
        let inner = looping(&mut ir, next, Some("inner"));
        let body = block(&mut ir, vec![inner]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Jump));
    }

    #[test]
    fn a_jump_outweighs_a_return_wherever_each_stands() {
        for jump_first in [true, false] {
            let mut ir = IrFile::default();
            let exit = ir.add_expr(IrExpr::Return(None));
            let jump = ir.add_expr(IrExpr::Break { label: None });
            let stmts = if jump_first {
                vec![jump, exit]
            } else {
                vec![exit, jump]
            };
            let body = block(&mut ir, stmts);
            assert!(ir.record_lambda_exit(body));
            assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Jump));
        }
    }

    #[test]
    fn a_return_rewritten_into_a_frame_break_after_recording_is_a_jump() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Return(None));
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Return));
        ir.exprs[exit as usize] = IrExpr::Break {
            label: Some("$fir_inline_return_0".to_string()),
        };
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::Jump));
    }
}
