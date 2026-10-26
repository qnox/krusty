//! The body an inline-return frame wraps.
//!
//! A template whose body returns from the template itself is prepared as a frame: a labelled loop
//! whose body evaluates the original body, storing its value as the frame's result, and leaves.
//! Lowering records each body it wraps so a backend can find the body's own scope again.

use super::{ExprId, IrExpr, IrFile};

impl IrFile {
    /// The body the inline-return frame `template` wraps, when `template` is such a frame.
    pub(crate) fn inline_return_frame_body(&self, template: ExprId) -> Option<ExprId> {
        let IrExpr::Block { stmts, .. } = self.expr(template) else {
            return None;
        };
        stmts.iter().find_map(|&statement| {
            let IrExpr::While {
                body,
                label: Some(_),
                ..
            } = self.expr(statement)
            else {
                return None;
            };
            let IrExpr::Block { stmts, .. } = self.expr(*body) else {
                return None;
            };
            let evaluated = match self.expr(*stmts.first()?) {
                IrExpr::SetValue { value, .. } => *value,
                _ => *stmts.first()?,
            };
            self.inline_return_frame_bodies
                .contains(&evaluated)
                .then_some(evaluated)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrConst;

    fn frame(ir: &mut IrFile, evaluated: ExprId, stored: bool) -> ExprId {
        let evaluation = if stored {
            ir.add_expr(IrExpr::SetValue {
                var: 0,
                value: evaluated,
            })
        } else {
            evaluated
        };
        let exit = ir.add_expr(IrExpr::Break {
            label: Some("frame".to_string()),
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![evaluation, exit],
            value: None,
        });
        let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let looping = ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test: false,
            label: Some("frame".to_string()),
        });
        ir.add_expr(IrExpr::Block {
            stmts: vec![looping],
            value: None,
        })
    }

    #[test]
    fn a_frame_names_the_body_it_wraps_whether_it_stores_a_result_or_not() {
        for stored in [true, false] {
            let mut ir = IrFile::default();
            let evaluated = ir.add_expr(IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            });
            ir.inline_return_frame_bodies.insert(evaluated);
            let template = frame(&mut ir, evaluated, stored);
            assert_eq!(ir.inline_return_frame_body(template), Some(evaluated));
        }
    }

    #[test]
    fn a_labelled_loop_lowering_did_not_record_wraps_no_body() {
        let mut ir = IrFile::default();
        let evaluated = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let template = frame(&mut ir, evaluated, true);
        assert_eq!(ir.inline_return_frame_body(template), None);
        assert_eq!(ir.inline_return_frame_body(evaluated), None);
    }
}
