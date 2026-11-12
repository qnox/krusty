//! Whether a `while (true)` loop can be left by a `break`.
//!
//! An inline-return frame is a labelled single-iteration `while (true)` loop whose exit is a
//! `break` (see `crate::fir_lower::inline_returns`). When every path through the inlined body
//! diverges before reaching an exit (`error(...)`, whose body returns a `throw`), the frame never
//! completes, which a target needs to know to leave the frame's value position unreachable.

use super::{for_each_child, ExprId, IrConst, IrExpr, IrFile};

impl IrFile {
    /// Whether `expression` is a `while (true)` loop that no reachable `break` leaves, so that
    /// evaluating it never completes normally.
    pub fn loop_never_exits(&self, expression: ExprId) -> bool {
        let IrExpr::While {
            cond, body, label, ..
        } = self.expr(expression)
        else {
            return false;
        };
        matches!(self.expr(*cond), IrExpr::Const(IrConst::Boolean(true)))
            && !self.reaches_exit(*body, label.as_deref(), true)
    }

    /// Whether evaluating `expression` can reach a `break` leaving the loop labelled `label`, or,
    /// while `unlabeled` holds, an unlabelled `break` of that loop. A statement after one that
    /// always diverges is never reached.
    fn reaches_exit(&self, expression: ExprId, label: Option<&str>, unlabeled: bool) -> bool {
        match self.expr(expression) {
            IrExpr::Break { label: None } => unlabeled,
            IrExpr::Break {
                label: Some(target),
            } => label == Some(target.as_str()),
            IrExpr::Block { stmts, value } => {
                for &statement in stmts {
                    if self.reaches_exit(statement, label, unlabeled) {
                        return true;
                    }
                    if self.expr_discarding_diverges_by(statement, &|_, _| false) {
                        return false;
                    }
                }
                value.is_some_and(|value| self.reaches_exit(value, label, unlabeled))
            }
            // An unlabelled `break` inside a nested loop leaves that loop.
            IrExpr::While { .. } => {
                let mut reaches = false;
                for_each_child(&self.exprs, expression, &mut |child| {
                    reaches |= self.reaches_exit(child, label, false);
                });
                reaches
            }
            _ => {
                let mut reaches = false;
                for_each_child(&self.exprs, expression, &mut |child| {
                    reaches |= self.reaches_exit(child, label, unlabeled);
                });
                reaches
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ir::{IrConst, IrExpr, IrFile};

    fn frame(ir: &mut IrFile, body: Vec<u32>) -> u32 {
        let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: body,
            value: None,
        });
        ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test: false,
            label: Some("frame".to_string()),
            inline_return_frame: false,
        })
    }

    fn exit(ir: &mut IrFile, label: Option<&str>) -> u32 {
        ir.add_expr(IrExpr::Break {
            label: label.map(str::to_string),
        })
    }

    #[test]
    fn a_frame_whose_exit_follows_a_throw_never_exits() {
        let mut ir = IrFile::default();
        let operand = ir.add_expr(IrExpr::Const(IrConst::Null));
        let thrown = ir.add_expr(IrExpr::Throw { operand });
        let exit = exit(&mut ir, Some("frame"));
        let frame = frame(&mut ir, vec![thrown, exit]);
        assert!(ir.loop_never_exits(frame));
        let outer = ir.add_expr(IrExpr::Block {
            stmts: vec![frame],
            value: None,
        });
        assert!(ir.expr_diverges_by(outer, &|leaf, _| ir.loop_never_exits(leaf)));
    }

    #[test]
    fn a_reachable_break_leaves_the_frame() {
        let mut ir = IrFile::default();
        let labelled = exit(&mut ir, Some("frame"));
        assert!(!{
            let frame = frame(&mut ir, vec![labelled]);
            ir.loop_never_exits(frame)
        });
        let unlabeled = exit(&mut ir, None);
        let frame = frame(&mut ir, vec![unlabeled]);
        assert!(!ir.loop_never_exits(frame));
    }

    #[test]
    fn a_nested_loop_owns_its_unlabeled_break_but_not_the_frame_label() {
        let mut ir = IrFile::default();
        let inner_exit = exit(&mut ir, None);
        let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let inner = ir.add_expr(IrExpr::While {
            cond,
            body: inner_exit,
            update: None,
            post_test: false,
            label: None,
            inline_return_frame: false,
        });
        let operand = ir.add_expr(IrExpr::Const(IrConst::Null));
        let thrown = ir.add_expr(IrExpr::Throw { operand });
        let frame_exit = exit(&mut ir, Some("frame"));
        let closed = frame(&mut ir, vec![inner, thrown, frame_exit]);
        assert!(ir.loop_never_exits(closed));

        let labelled = exit(&mut ir, Some("frame"));
        let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let escaping = ir.add_expr(IrExpr::While {
            cond,
            body: labelled,
            update: None,
            post_test: false,
            label: None,
            inline_return_frame: false,
        });
        let open = frame(&mut ir, vec![escaping]);
        assert!(!ir.loop_never_exits(open));
    }

    #[test]
    fn only_a_constant_true_condition_loops_forever() {
        let mut ir = IrFile::default();
        let operand = ir.add_expr(IrExpr::Const(IrConst::Null));
        let thrown = ir.add_expr(IrExpr::Throw { operand });
        let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![thrown],
            value: None,
        });
        let conditional = ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test: false,
            label: None,
            inline_return_frame: false,
        });
        assert!(!ir.loop_never_exits(conditional));
    }
}
