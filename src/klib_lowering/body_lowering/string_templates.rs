//! String templates.
//!
//! A KLIB serializes a source template as one concatenation of its parts: its literal text as
//! `String` constants and its interpolated values. Checked FIR lowering lowers the same template
//! as a concatenation of the same parts, each run of literal text one `String` constant it
//! creates itself, and each value lowered as written.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrConst, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrExprKind};
use crate::metadata::klib_ir::KlibIrConstant;
use crate::types::Ty;

impl BodyLowering<'_, '_, '_> {
    pub(super) fn string_template(&mut self, parts: &[KlibIrExprId], ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::String {
            return Err(mismatch("a string template is not typed `String`"));
        }
        let mut lowered = Vec::with_capacity(parts.len());
        let mut text: Option<String> = None;
        let mut values = 0;
        for part in parts {
            match &self.arena.expr(*part).kind {
                KlibIrExprKind::Const(KlibIrConstant::String(literal)) => {
                    text.get_or_insert_with(String::new).push_str(literal);
                }
                KlibIrExprKind::Const(_) => {
                    // Checked FIR lowering folds a constant into the literal text around it.
                    return unsupported("a string template with a constant of another type");
                }
                KlibIrExprKind::StringConcat(_) => {
                    // Checked FIR lowering flattens a nested template into the outer one.
                    return unsupported("a nested string template");
                }
                _ => {
                    self.flush_text(&mut text, &mut lowered);
                    lowered.push(self.expression(*part)?);
                    values += 1;
                }
            }
        }
        self.flush_text(&mut text, &mut lowered);
        if values == 0 {
            // Checked FIR lowering folds a template of literal text to its constant value.
            return unsupported("a string template of literal text alone");
        }
        Ok(self.ir.add_expr(IrExpr::StringConcat(lowered)))
    }

    /// The literal text read since the last value, as one constant part; empty text is no part.
    fn flush_text(&mut self, text: &mut Option<String>, lowered: &mut Vec<ExprId>) {
        if let Some(text) = text.take().filter(|text| !text.is_empty()) {
            lowered.push(
                self.ir
                    .add_expr(IrExpr::Const(IrConst::String(text.into()))),
            );
        }
    }
}
