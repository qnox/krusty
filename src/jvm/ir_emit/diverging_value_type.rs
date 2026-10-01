//! JVM value type of a common-IR expression that completes no value.
//!
//! A source `return` is a bottom-typed expression, including when it occupies an argument slot.
//! The emitter reports that fact here so the oversized value-type walk does not grow a new arm.

use crate::ir::IrExpr;
use crate::types::Ty;

/// `Nothing` when `expr` transfers control or is an explicit bottom value.
pub(super) fn diverging_value_ty(expr: &IrExpr) -> Option<Ty> {
    match expr {
        IrExpr::Return(_)
        | IrExpr::Throw { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::BottomValue { .. } => Some(Ty::Nothing),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::IrConst;

    #[test]
    fn a_source_return_is_nothing_and_a_constant_is_not() {
        assert_eq!(diverging_value_ty(&IrExpr::Return(None)), Some(Ty::Nothing));
        assert_eq!(diverging_value_ty(&IrExpr::Const(IrConst::Int(1))), None);
    }
}
