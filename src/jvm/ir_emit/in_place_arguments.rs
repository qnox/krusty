//! Emission-side admission for reading an `@InlineOnly` call's arguments in place.

use crate::ir::{IrExpr, IrFile, IrTypeOp};

/// Whether every argument's code can be moved to where the body loads it: kotlinc refuses the
/// move for a whole call when any argument's code stores a local or jumps out of itself.
/// `plain_construction` says whether a `New` is written as one constructor invoke, not as a
/// guarded construction such as a nullable SAM wrapper's null check.
pub(super) fn arguments_movable(
    ir: &IrFile,
    arguments: &[u32],
    plain_construction: &dyn Fn(u32) -> bool,
) -> bool {
    arguments
        .iter()
        .all(|&argument| evaluates_without_local_writes(ir, argument, plain_construction))
}

/// Whether an expression's emitted code can neither store a local nor jump. Deliberately narrow:
/// an unlisted shape retains the ordinary stored-parameter path.
fn evaluates_without_local_writes(
    ir: &IrFile,
    expression: u32,
    plain_construction: &dyn Fn(u32) -> bool,
) -> bool {
    match ir.expr(expression) {
        IrExpr::GetValue(_)
        | IrExpr::Const(_)
        | IrExpr::GetStatic(_)
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::UnitInstance => true,
        // `new; dup; <arguments>; invokespecial`: a direct construction stores nothing either.
        IrExpr::New { args, defaults, .. }
            if defaults.is_empty() && plain_construction(expression) =>
        {
            arguments_movable(ir, args, plain_construction)
        }
        IrExpr::TypeOp {
            op: IrTypeOp::Cast | IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => evaluates_without_local_writes(ir, *arg, plain_construction),
        _ => false,
    }
}
