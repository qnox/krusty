//! The `while (true)` loops a lowering builds around code it leaves only by a jump.

use super::{ExprId, IrConst, IrExpr, IrFile};

impl IrFile {
    /// `while (true) body`, labelled `label`: no update, a pre-test loop, and no source inline
    /// frame, so it is never an inline-return frame.
    pub(crate) fn add_endless_loop(&mut self, body: ExprId, label: Option<String>) -> ExprId {
        let always = self.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        self.add_expr(IrExpr::While {
            cond: always,
            body,
            update: None,
            post_test: false,
            label,
            inline_return_frame: false,
        })
    }
}
