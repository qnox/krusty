//! What the value-class pass is handed, for the `value_classes` trace category.

use krusty::ir::{Callee, IrExpr, IrFile};

/// Trace the file's size and the property reads and virtual calls the pass may rewrite.
pub(super) fn trace_inputs(ir: &IrFile) {
    krusty::trace_compiler!(
        "value_classes",
        "lower start classes={} functions={} expressions={}",
        ir.classes.len(),
        ir.functions.len(),
        ir.exprs.len()
    );
    for (id, expression) in ir.exprs.iter().enumerate() {
        match expression {
            IrExpr::PropertyRead { owner, name, .. } => krusty::trace_compiler!(
                "value_classes",
                "input property read {id}: {}.{}",
                owner,
                name
            ),
            IrExpr::Call {
                callee: call_callee,
                ..
            } if let Callee::Virtual { owner, name, .. } = &**call_callee => {
                krusty::trace_compiler!(
                    "value_classes",
                    "input virtual call {id}: {}.{}",
                    owner,
                    name
                )
            }
            _ => {}
        }
    }
}
