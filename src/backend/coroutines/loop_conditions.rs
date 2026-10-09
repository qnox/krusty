//! A loop whose condition suspends, as a loop the state machine splits.
//!
//! The machine splits a loop's body; a condition is evaluated where no state can begin. Testing the
//! condition as the body's first statement keeps its meaning, since a `continue` re-enters the loop
//! at the top, which now tests it:
//!
//! ```text
//! while (cond()) { body }        while (true) { val c = cond(); if (!c) break; body }
//! do { body } while (cond())     while (true) { body; val c = cond(); if (!c) break }
//! ```
//!
//! A `do`/`while` whose body continues this loop keeps its shape: its `continue` must reach the
//! condition, which now sits after it, and the machine declines that loop as before.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrConst, IrExpr, IrFile};
use crate::types::Ty;

use super::suspension_points::expr_calls_suspend;
use super::value_namespace::max_value_index;

/// Move every suspending loop condition under `expression` into its loop's body.
pub(crate) fn test_suspending_conditions_in_body(
    ir: &mut IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        test_suspending_conditions_in_body(ir, child, suspend_set);
    }
    let IrExpr::While {
        cond,
        body,
        update,
        post_test,
        label,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if !expr_calls_suspend(ir, cond, suspend_set)
        || (post_test && continues(ir, body, label.as_deref(), true))
    {
        return;
    }
    let test = exit_unless(ir, cond, label.clone());
    let stmts = if post_test {
        vec![body, test]
    } else {
        vec![test, body]
    };
    let body = ir.add_expr(IrExpr::Block { stmts, value: None });
    let always = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    ir.exprs[expression as usize] = IrExpr::While {
        cond: always,
        body,
        update,
        post_test: false,
        label,
    };
}

/// `{ val c = cond; if (!c) break@label }`.
fn exit_unless(ir: &mut IrFile, cond: ExprId, label: Option<String>) -> ExprId {
    let index = max_value_index(ir) + 1;
    let declaration = ir.add_expr(IrExpr::Variable {
        index,
        ty: Ty::Boolean,
        init: Some(cond),
        named: false,
    });
    let holds = ir.add_expr(IrExpr::GetValue(index));
    let stay = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let leave = ir.add_expr(IrExpr::Break { label });
    let leave = ir.add_expr(IrExpr::Block {
        stmts: vec![leave],
        value: None,
    });
    let test = ir.add_expr(IrExpr::When {
        branches: vec![(Some(holds), stay), (None, leave)],
    });
    ir.add_expr(IrExpr::Block {
        stmts: vec![declaration, test],
        value: None,
    })
}

/// Whether `expression` continues the loop labeled `label`, or, while `innermost`, the loop it is
/// directly in.
fn continues(ir: &IrFile, expression: ExprId, label: Option<&str>, innermost: bool) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::Continue { label: None } => innermost,
        IrExpr::Continue {
            label: Some(target),
        } => label == Some(target.as_str()),
        IrExpr::Lambda { .. } => false,
        IrExpr::While { .. } => {
            let mut found = false;
            for_each_child(&ir.exprs, expression, &mut |child| {
                found = found || continues(ir, child, label, false);
            });
            found
        }
        _ => {
            let mut found = false;
            for_each_child(&ir.exprs, expression, &mut |child| {
                found = found || continues(ir, child, label, innermost);
            });
            found
        }
    }
}
