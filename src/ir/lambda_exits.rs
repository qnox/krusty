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

/// How an inline lambda's body leaves the lambda: each way it can leave, kept apart, since one body
/// can leave by several and each obligates its route differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LambdaExit {
    /// By a non-local `return` of the function the lambda is written in.
    pub(crate) returns: bool,
    /// By a `break` or `continue` of a loop outside it.
    pub(crate) jumps: bool,
    /// By a return of an inline expansion outside it, which lowering made a `break` of the
    /// expansion's return frame. The return stores the frame's result, a value of the code outside
    /// the lambda.
    pub(crate) frame_returns: bool,
}

impl LambdaExit {
    const RETURN: Self = Self {
        returns: true,
        jumps: false,
        frame_returns: false,
    };
    const JUMP: Self = Self {
        returns: false,
        jumps: true,
        frame_returns: false,
    };
    const FRAME_RETURN: Self = Self {
        returns: false,
        jumps: false,
        frame_returns: true,
    };

    /// Whether the body leaves only through its value.
    pub(crate) fn is_local(self) -> bool {
        self == Self::default()
    }

    fn union(self, other: Self) -> Self {
        Self {
            returns: self.returns || other.returns,
            jumps: self.jumps || other.jumps,
            frame_returns: self.frame_returns || other.frame_returns,
        }
    }
}

impl IrFile {
    /// Whether the inline lambda body `body` leaves the lambda non-locally, recording the fact the
    /// first time it is asked. Every `return` in an inline body is non-local (a local return is
    /// lowered to the body's value); a `break` or `continue` is when its loop is outside `body`.
    pub(crate) fn record_lambda_exit(&mut self, body: ExprId) -> bool {
        if let Some(&exit) = self.lambda_exits.leaves.get(&body) {
            return !exit.is_local();
        }
        let exit = lambda_exit(self, body, &mut Vec::new());
        self.lambda_exits.leaves.insert(body, exit);
        !exit.is_local()
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
    /// of that lambda's return frame. The body as it stands is walked again, so such a body leaves
    /// by a frame return rather than the return it was recorded as.
    pub(crate) fn lambda_exit(&self, body: ExprId) -> Option<LambdaExit> {
        self.lambda_exits.leaves.get(&body)?;
        Some(lambda_exit(self, body, &mut Vec::new()))
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
        // The returned value is evaluated first, and can itself leave by an outer loop's
        // `break` or `continue` (`return if (c) break else x`).
        IrExpr::Return(value) => {
            return value.map_or(LambdaExit::RETURN, |value| {
                LambdaExit::RETURN.union(lambda_exit(ir, value, loops))
            });
        }
        IrExpr::Break { label } | IrExpr::Continue { label } => {
            return match label {
                None if loops.is_empty() => LambdaExit::JUMP,
                Some(label) if !loops.contains(label) => {
                    if ir.inline_return_frames.values().any(|frame| frame == label) {
                        LambdaExit::FRAME_RETURN
                    } else {
                        LambdaExit::JUMP
                    }
                }
                _ => LambdaExit::default(),
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
    let mut exit = LambdaExit::default();
    super::for_each_child(&ir.exprs, expression, &mut |child| {
        exit = exit.union(lambda_exit(ir, child, loops));
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
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::RETURN));
    }

    #[test]
    fn a_break_of_a_loop_inside_the_lambda_stays_in_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let inner = looping(&mut ir, exit, None);
        let body = block(&mut ir, vec![inner]);
        assert!(!ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::default()));
    }

    #[test]
    fn a_break_or_continue_of_a_loop_outside_the_lambda_leaves_it() {
        let mut ir = IrFile::default();
        let exit = ir.add_expr(IrExpr::Break { label: None });
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::JUMP));

        let next = ir.add_expr(IrExpr::Continue {
            label: Some("outer".to_string()),
        });
        let inner = looping(&mut ir, next, Some("inner"));
        let body = block(&mut ir, vec![inner]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::JUMP));
    }

    #[test]
    fn a_jump_and_a_return_are_both_kept_wherever_each_stands() {
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
            assert_eq!(
                ir.lambda_exit(body),
                Some(LambdaExit {
                    returns: true,
                    jumps: true,
                    frame_returns: false,
                })
            );
        }
    }

    #[test]
    fn a_jump_inside_a_returned_value_is_a_jump() {
        let mut ir = IrFile::default();
        let jump = ir.add_expr(IrExpr::Continue { label: None });
        let exit = ir.add_expr(IrExpr::Return(Some(jump)));
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(
            ir.lambda_exit(body),
            Some(LambdaExit {
                returns: true,
                jumps: true,
                frame_returns: false,
            })
        );

        // A loop inside the returned value owns its own `break`.
        let mut ir = IrFile::default();
        let jump = ir.add_expr(IrExpr::Break { label: None });
        let inner = looping(&mut ir, jump, None);
        let exit = ir.add_expr(IrExpr::Return(Some(inner)));
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::RETURN));
    }

    #[test]
    fn a_return_rewritten_into_a_frame_break_after_recording_is_a_frame_return() {
        let mut ir = IrFile::default();
        let result = ir.add_expr(IrExpr::UnitInstance);
        ir.inline_return_frames
            .insert(result, "$fir_inline_return_0".to_string());
        let exit = ir.add_expr(IrExpr::Return(None));
        let body = block(&mut ir, vec![exit]);
        assert!(ir.record_lambda_exit(body));
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::RETURN));
        ir.exprs[exit as usize] = IrExpr::Break {
            label: Some("$fir_inline_return_0".to_string()),
        };
        assert_eq!(ir.lambda_exit(body), Some(LambdaExit::FRAME_RETURN));
    }

    #[test]
    fn a_frame_return_and_a_jump_are_both_kept_wherever_each_stands() {
        for jump_first in [true, false] {
            let mut ir = IrFile::default();
            let result = ir.add_expr(IrExpr::UnitInstance);
            ir.inline_return_frames
                .insert(result, "$fir_inline_return_0".to_string());
            let frame = ir.add_expr(IrExpr::Break {
                label: Some("$fir_inline_return_0".to_string()),
            });
            let jump = ir.add_expr(IrExpr::Continue { label: None });
            let stmts = if jump_first {
                vec![jump, frame]
            } else {
                vec![frame, jump]
            };
            let body = block(&mut ir, stmts);
            assert!(ir.record_lambda_exit(body));
            assert_eq!(
                ir.lambda_exit(body),
                Some(LambdaExit {
                    returns: false,
                    jumps: true,
                    frame_returns: true,
                })
            );
        }
    }
}
