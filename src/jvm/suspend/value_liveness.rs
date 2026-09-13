//! Path-sensitive lifetime termination for locals carried across JVM coroutine suspension points.

use super::{expr_calls_suspend, expr_reads, for_each_child, ExprId, HashSet, IrExpr, IrFile};

/// Whether every path through `statement` assigns `slot` before reading it and no suspension runs
/// after that assignment. The value held on entry is therefore dead at the statement boundary.
pub(super) fn kills_value(
    ir: &IrFile,
    statement: ExprId,
    slot: u32,
    suspend_functions: &HashSet<u32>,
) -> bool {
    match &ir.exprs[statement as usize] {
        IrExpr::Variable {
            index,
            init: Some(init),
            ..
        } if *index == slot => !expr_reads(ir, *init, slot),
        IrExpr::SetValue { var, value } if *var == slot => !expr_reads(ir, *value, slot),
        IrExpr::Block { stmts, value } => {
            let mut killed = false;
            for &nested in stmts.iter().chain(value.iter()) {
                if killed {
                    if expr_calls_suspend(ir, nested, suspend_functions) {
                        return false;
                    }
                    continue;
                }
                if kills_value(ir, nested, slot, suspend_functions) {
                    killed = true;
                    continue;
                }
                if expr_reads(ir, nested, slot) || writes_value(ir, nested, slot) {
                    return false;
                }
            }
            killed
        }
        IrExpr::When { branches } => {
            branches.iter().any(|(condition, _)| condition.is_none())
                && branches.iter().all(|(condition, body)| {
                    condition.is_none_or(|condition| {
                        !expr_reads(ir, condition, slot) && !writes_value(ir, condition, slot)
                    }) && kills_value(ir, *body, slot, suspend_functions)
                })
        }
        _ => false,
    }
}

/// Whether one of the expressions that executes after the current walk point reads `slot` before
/// every-path assignment kills it. `level_starts` partitions the pending list by nesting level;
/// inner levels execute before outer ones.
pub(super) fn pending_reads_after(
    ir: &IrFile,
    pending: &[ExprId],
    level_starts: &[usize],
    slot: u32,
    suspend_functions: &HashSet<u32>,
) -> bool {
    let mut end = pending.len();
    for &start in level_starts.iter().rev() {
        for &expression in &pending[start..end] {
            if expr_reads(ir, expression, slot) {
                return true;
            }
            if kills_value(ir, expression, slot, suspend_functions) {
                return false;
            }
        }
        end = start;
    }
    pending[..end]
        .iter()
        .any(|&expression| expr_reads(ir, expression, slot))
}

fn writes_value(ir: &IrFile, expression: ExprId, slot: u32) -> bool {
    match ir.exprs[expression as usize] {
        IrExpr::Variable { index, .. } if index == slot => return true,
        IrExpr::SetValue { var, .. } if var == slot => return true,
        _ => {}
    }
    let mut found = false;
    for_each_child(&ir.exprs, expression, &mut |child| {
        if writes_value(ir, child, slot) {
            found = true;
        }
    });
    found
}
