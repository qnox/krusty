//! Control-flow facts the suspension normalizations consult: divergence and owned loop jumps.

use crate::ir::{ExprId, IrExpr, IrFile};

/// Whether statement `s` always transfers control away (never falls through): a `return`/`throw`, or a
/// block/`when` all of whose exits do. Used to suppress a dead fall-through transition after it.
pub(crate) fn stmt_diverges(ir: &IrFile, s: ExprId) -> bool {
    ir.expr_discarding_diverges_by(s, &|_, _| false)
}

/// Whether `expression` contains a `break`/`continue` that must escape this value position. A nested
/// loop owns its own unlabeled exits, and a lambda is always a control-flow boundary. A labeled exit
/// is safe to expose without resolving it here: branch binding preserves its label and `Flat` uses
/// the active loop stack to route that exact target.
pub(crate) fn expr_contains_owned_loop_jump(ir: &IrFile, expression: ExprId) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::Break { .. } | IrExpr::Continue { .. } => true,
        IrExpr::While { .. } | IrExpr::Lambda { .. } => false,
        _ => {
            let mut found = false;
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                found = found || expr_contains_owned_loop_jump(ir, child);
            });
            found
        }
    }
}

/// Whether `e`'s subtree contains a bare `return` (a function return), NOT descending into a nested
/// lambda (whose `return` is its own). A `return` inside a suspending try body needs a
/// finally-before-return transfer the flattener does not yet model, so the try-finally path declines it.
pub(crate) fn expr_has_return(ir: &IrFile, e: ExprId) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::Return(_) => true,
        IrExpr::Lambda { .. } => false,
        _ => {
            let mut found = false;
            crate::ir::for_each_child(&ir.exprs, e, &mut |c| {
                found = found || expr_has_return(ir, c);
            });
            found
        }
    }
}
