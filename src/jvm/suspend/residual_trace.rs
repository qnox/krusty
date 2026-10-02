//! Suspend-flattener diagnostics for residual expression trees.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrFile};

/// Trace a residual suspending expression as an id-labelled tree. This stays suspend-specific: it
/// is useful only when normalization leaves a suspension under an unsupported statement shape.
pub(super) fn trace_residual_suspension(ir: &IrFile, expression: ExprId, depth: usize) {
    crate::trace_compiler!(
        "suspend",
        "flatten residual {}{expression}: {:?}",
        "  ".repeat(depth),
        ir.exprs[expression as usize]
    );
    if depth >= 12 {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        trace_residual_suspension(ir, child, depth + 1);
    }
}

pub(super) fn trace_residual_parents(ir: &IrFile, expression: ExprId) {
    fn walk(ir: &IrFile, child: ExprId, depth: usize, seen: &mut HashSet<ExprId>) {
        if depth >= 10 || !seen.insert(child) {
            return;
        }
        for (parent, node) in ir.exprs.iter().enumerate() {
            let mut contains = false;
            for_each_child(&ir.exprs, parent as ExprId, &mut |candidate| {
                contains = contains || candidate == child;
            });
            if contains {
                crate::trace_compiler!(
                    "suspend",
                    "flatten parent {}{}: {:?}",
                    "  ".repeat(depth),
                    parent,
                    node
                );
                walk(ir, parent as ExprId, depth + 1, seen);
            }
        }
    }
    walk(ir, expression, 0, &mut HashSet::new());
}
