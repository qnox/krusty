//! JavaScript realization of common-IR loop updates.
//!
//! Common IR keeps a loop update separate so a target can place it at its native continue target.
//! JavaScript's `for` update slot accepts expressions only, while Kotlin range updates may contain
//! checked control flow (notably the inclusive-end overflow guard). Move such updates into the loop
//! body and execute them before every continue targeting that exact stable loop label.

use std::collections::HashMap;

use crate::ir::{IrExpr, IrFile};

pub(super) fn realize_updates(ir: &mut IrFile) {
    let original_count = ir.exprs.len();
    let loops = (0..original_count)
        .filter_map(|index| match &ir.exprs[index] {
            IrExpr::While {
                body,
                update: Some(update),
                label: Some(label),
                ..
            } => Some((index, *body, *update, label.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();

    // Rewrite every original continue before adding any wrapper expressions. Processing one nested
    // loop at a time can otherwise hide an outer-targeting continue inside the wrapper just added
    // for the inner loop. The checked label is the complete target identity at this stage.
    let updates = loops
        .iter()
        .map(|(_, _, update, label)| (label.as_str(), *update))
        .collect::<HashMap<_, _>>();
    for expression in 0..original_count {
        let Some((label, update)) = (match &ir.exprs[expression] {
            IrExpr::Continue { label: Some(label) } => updates
                .get(label.as_str())
                .map(|update| (label.clone(), *update)),
            _ => None,
        }) else {
            continue;
        };
        let continuation = ir.add_expr(IrExpr::Continue { label: Some(label) });
        ir.exprs[expression] = IrExpr::Block {
            stmts: vec![update, continuation],
            value: None,
        };
    }

    for (loop_index, body, update, _) in loops {
        let body_with_update = ir.add_expr(IrExpr::Block {
            stmts: vec![body, update],
            value: None,
        });
        let IrExpr::While {
            body,
            update: loop_update,
            ..
        } = &mut ir.exprs[loop_index]
        else {
            unreachable!("collected loop must remain a loop")
        };
        *body = body_with_update;
        *loop_update = None;
    }
}
