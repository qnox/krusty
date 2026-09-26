//! Which initializer values a freshly allocated JVM field already holds.
//!
//! The JVM zero-fills every field before a constructor or `<clinit>` runs: a primitive slot holds
//! its zero (`0`, `0L`, `+0.0`, `false`) and a reference slot holds `null`. kotlinc emits no
//! declaration store of that value. The answer depends on the field's physical slot, so callers
//! pass the slot type after value-class carrier realization, never a semantic declared type: a
//! boxed `Int?` is a reference whose fresh value is `null`, not `0`.

use crate::ir::{ExprId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Does a fresh field whose physical slot is `slot` already hold the value of `expression`?
pub(crate) fn holds_fresh_value(ir: &IrFile, slot: Ty, expression: ExprId) -> bool {
    if slot.is_jvm_scalar() {
        is_scalar_zero(ir, expression)
    } else {
        is_null_constant(ir, expression)
    }
}

fn is_null_constant(ir: &IrFile, expression: ExprId) -> bool {
    match ir.expr(expression) {
        IrExpr::Const(IrConst::Null) => true,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => is_null_constant(ir, *arg),
        _ => false,
    }
}

/// `false`, or a zero of any width, signed or unsigned (an unsigned value's slot is its signed
/// carrier). A floating zero counts only with its sign bit clear: `-0.0` is not all-zero bits.
fn is_scalar_zero(ir: &IrFile, expression: ExprId) -> bool {
    match ir.expr(expression) {
        IrExpr::Const(IrConst::Boolean(false))
        | IrExpr::Const(IrConst::Byte(0))
        | IrExpr::Const(IrConst::Short(0))
        | IrExpr::Const(IrConst::Int(0))
        | IrExpr::Const(IrConst::Long(0))
        | IrExpr::Const(IrConst::Char(0))
        | IrExpr::Const(IrConst::UByte(0))
        | IrExpr::Const(IrConst::UShort(0))
        | IrExpr::Const(IrConst::UInt(0))
        | IrExpr::Const(IrConst::ULong(0)) => true,
        IrExpr::Const(IrConst::Float(value)) => value.to_bits() == 0,
        IrExpr::Const(IrConst::Double(value)) => value.to_bits() == 0,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => is_scalar_zero(ir, *arg),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scalar slot's fresh value is `false` or a zero of its width; a reference slot's is only
    /// `null`, so a boxed zero or `false` is a real store.
    #[test]
    fn the_fresh_value_follows_the_physical_slot() {
        let mut ir = IrFile::default();
        let zeros = [
            IrConst::Boolean(false),
            IrConst::Byte(0),
            IrConst::Short(0),
            IrConst::Int(0),
            IrConst::Long(0),
            IrConst::Char(0),
            IrConst::UByte(0),
            IrConst::UShort(0),
            IrConst::UInt(0),
            IrConst::ULong(0),
            IrConst::Float(0.0),
            IrConst::Double(0.0),
        ];
        let non_zeros = [
            IrConst::Null,
            IrConst::Boolean(true),
            IrConst::Int(1),
            IrConst::UInt(1),
            IrConst::Float(-0.0),
            IrConst::Double(-0.0),
        ];
        for (constants, scalar_default) in [(zeros.as_slice(), true), (non_zeros.as_slice(), false)]
        {
            for constant in constants {
                let expression = ir.add_expr(IrExpr::Const(constant.clone()));
                assert_eq!(
                    holds_fresh_value(&ir, Ty::Int, expression),
                    scalar_default,
                    "{constant:?} in an int slot"
                );
                assert_eq!(
                    holds_fresh_value(&ir, Ty::UInt, expression),
                    scalar_default,
                    "{constant:?} in an unsigned carrier slot"
                );
                assert_eq!(
                    holds_fresh_value(&ir, Ty::nullable(Ty::Int), expression),
                    *constant == IrConst::Null,
                    "{constant:?} in a boxed Integer slot"
                );
                assert_eq!(
                    holds_fresh_value(&ir, Ty::obj("fixture/Value"), expression),
                    *constant == IrConst::Null,
                    "{constant:?} in a boxed value-class slot"
                );
            }
        }
    }
}
