//! The body an inline-return frame wraps.
//!
//! A template whose body returns from the template itself is prepared as a frame: a labelled loop
//! whose body evaluates the original body, storing its value as the frame's result, and leaves.
//! Lowering marks the loop itself as a frame (`IrExpr::While::inline_return_frame`), so a copy of
//! the template carries the mark, and a backend finds the body's own scope again from it.

use super::{ExprId, IrExpr, IrFile};

impl IrFile {
    /// The body the inline-return frame `template` wraps, when `template` is such a frame.
    pub fn inline_return_frame_body(&self, template: ExprId) -> Option<ExprId> {
        let IrExpr::Block { stmts, .. } = self.expr(template) else {
            return None;
        };
        stmts.iter().find_map(|&statement| {
            let IrExpr::While {
                body,
                inline_return_frame: true,
                ..
            } = self.expr(statement)
            else {
                return None;
            };
            let IrExpr::Block { stmts, .. } = self.expr(*body) else {
                return None;
            };
            Some(match self.expr(*stmts.first()?) {
                IrExpr::SetValue { value, .. } => *value,
                _ => *stmts.first()?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrConst;

    fn frame(
        ir: &mut IrFile,
        evaluated: ExprId,
        stored: bool,
        inline_return_frame: bool,
    ) -> ExprId {
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
            inline_return_frame,
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
            let template = frame(&mut ir, evaluated, stored, true);
            assert_eq!(ir.inline_return_frame_body(template), Some(evaluated));
        }
    }

    #[test]
    fn a_labelled_loop_lowering_did_not_mark_wraps_no_body() {
        let mut ir = IrFile::default();
        let evaluated = ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let template = frame(&mut ir, evaluated, true, false);
        assert_eq!(ir.inline_return_frame_body(template), None);
        assert_eq!(ir.inline_return_frame_body(evaluated), None);
    }
}
