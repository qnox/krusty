//! Emission-side admission for reading an `@InlineOnly` call's arguments in place.

use crate::ir::{IrExpr, IrFile, IrTypeOp};

/// Whether every argument's code can be moved to where the body loads it: kotlinc refuses the
/// move for a whole call when any argument's code stores a local or jumps out of itself.
pub(super) fn arguments_movable(ir: &IrFile, arguments: &[u32]) -> bool {
    arguments
        .iter()
        .all(|&argument| evaluates_without_local_writes(ir, argument))
}

/// Whether an expression's emitted code can neither store a local nor jump. Deliberately narrow:
/// an unlisted shape retains the ordinary stored-parameter path.
fn evaluates_without_local_writes(ir: &IrFile, expression: u32) -> bool {
    match ir.expr(expression) {
        IrExpr::GetValue(_)
        | IrExpr::Const(_)
        | IrExpr::GetStatic(_)
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::UnitInstance => true,
        // `new; dup; <arguments>; invokespecial`: a direct construction stores nothing either.
        IrExpr::New { args, defaults, .. } if defaults.is_empty() => arguments_movable(ir, args),
        IrExpr::TypeOp {
            op: IrTypeOp::Cast | IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => evaluates_without_local_writes(ir, *arg),
        _ => false,
    }
}
