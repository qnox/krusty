//! Value-position `when` normalization: bind a conditional whose branch values suspend to a
//! fresh local assigned in each branch, so each branch's suspension sits at a statement.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::Ty;

use super::block_splicing::value_block;
use super::control_flow::expr_contains_owned_loop_jump;
use super::suspension_points::expr_calls_suspend;
use super::value_namespace::{max_value_index, zero_value};
use super::value_try::{assign_branch_to_tmp, ValueBranchWrap};
use super::CoroutineRepresentation;

/// Desugar a VALUE-position `when`/`if` in `return` position whose BRANCH VALUES suspend (but whose
/// CONDITIONS do not — a suspending condition is hoisted earlier) into a STATEMENT-position `when` binding
/// a temp: `return when (x) { a -> v0; else -> v1 }` becomes `var tmp = <default>; when (x) { a -> { …
/// tmp = v0 }; else -> { … tmp = v1 } }; return tmp`. The flattener models a `when` STATEMENT with
/// suspending branch bodies (`emit_when_stmt`), so each branch's suspension surfaces there.
pub(crate) fn desugar_value_when(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    b: ExprId,
    suspend_set: &HashSet<u32>,
    ret_ty: &Ty,
) {
    for nested in owned_nested_blocks(ir, b) {
        desugar_value_when(ir, representation, nested, suspend_set, ret_ty);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if let Some((decl, new_when, get)) =
                bind_value_when_to_fresh_local(ir, representation, e, ret_ty, suspend_set)
            {
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(decl);
                new_stmts.push(new_when);
                new_stmts.push(ret);
                changed = true;
                continue;
            }
        }
        new_stmts.push(s);
    }
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
    }
}

/// Immediate nested block regions owned by `root`, excluding lambda bodies (which have an independent
/// value namespace and coroutine machine). Each returned block owns its deeper recursion, so shared
/// arena nodes are visited once per normalization entry without moving statements across a branch,
/// loop, or protected-region boundary.
pub(super) fn owned_nested_blocks(ir: &IrFile, root: ExprId) -> Vec<ExprId> {
    let mut blocks = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::from([root]);
    for_each_child(&ir.exprs, root, &mut |child| pending.push(child));
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.exprs[expression as usize] {
            IrExpr::Lambda { .. } => {}
            IrExpr::Block { .. } => blocks.push(expression),
            _ => for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    blocks
}

/// Bind a suspending value-`when` to one fresh typed local and return the declaration, the
/// statement-position conditional, and the final read. This is the one normalization shared by
/// returns and storage writes; callers decide only where the final read is consumed.
pub(super) fn bind_value_when_to_fresh_local(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    expression: ExprId,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> Option<(ExprId, ExprId, ExprId)> {
    // A result coercion belongs on each selected branch, after its suspension resumes.
    let (when_expr, branch_wrap) = match ir.exprs[expression as usize].clone() {
        IrExpr::When { .. } => (expression, None),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } if matches!(ir.exprs[arg as usize], IrExpr::When { .. }) => {
            (arg, Some(ValueBranchWrap::TypeOp(op, type_operand)))
        }
        _ => return None,
    };
    let suspends = expr_calls_suspend(ir, when_expr, suspend_set);
    let exits_loop = expr_contains_owned_loop_jump(ir, when_expr);
    if !suspends && !exits_loop {
        return None;
    }
    let IrExpr::When { branches } = ir.exprs[when_expr as usize].clone() else {
        return None;
    };
    let tmp = max_value_index(ir) + 1;
    let dflt = zero_value(ir, representation, ty);
    let declaration = ir.add_expr(IrExpr::Variable {
        index: tmp,
        ty: *ty,
        init: Some(dflt),
        named: false,
    });
    let branches = branches
        .into_iter()
        .map(|(condition, body)| {
            (
                condition,
                assign_branch_to_tmp(
                    ir,
                    representation,
                    body,
                    tmp,
                    ty,
                    suspend_set,
                    branch_wrap.clone(),
                ),
            )
        })
        .collect();
    let conditional = ir.add_expr(IrExpr::When { branches });
    let value = ir.add_expr(IrExpr::GetValue(tmp));
    Some((declaration, conditional, value))
}

pub(super) fn value_when(ir: &IrFile, expression: ExprId) -> Option<ExprId> {
    match ir.exprs[expression as usize] {
        IrExpr::When { .. } => Some(expression),
        IrExpr::TypeOp { arg, .. } if matches!(ir.exprs[arg as usize], IrExpr::When { .. }) => {
            Some(arg)
        }
        _ => None,
    }
}

/// Expose a checked conversion wrapped around a value conditional by applying that same operation
/// to each selected branch value. This is a structural equality:
/// `convert(if (c) a else b)` becomes `if (c) convert(a) else convert(b)`; branch statements and
/// divergent branches remain inside their original control-flow region.
pub(super) fn normalize_value_when(ir: &mut IrFile, expression: ExprId) -> Option<ExprId> {
    match ir.exprs[expression as usize].clone() {
        IrExpr::When { .. } => Some(expression),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let IrExpr::When { branches } = ir.exprs[arg as usize].clone() else {
                return None;
            };
            let branches = branches
                .into_iter()
                .map(|(condition, body)| {
                    let body = if let Some((stmts, value)) = value_block(ir, body) {
                        let value = ir.add_expr(IrExpr::TypeOp {
                            op,
                            arg: value,
                            type_operand,
                        });
                        ir.add_expr(IrExpr::Block {
                            stmts,
                            value: Some(value),
                        })
                    } else if matches!(ir.exprs[body as usize], IrExpr::Block { value: None, .. }) {
                        body
                    } else {
                        ir.add_expr(IrExpr::TypeOp {
                            op,
                            arg: body,
                            type_operand,
                        })
                    };
                    (condition, body)
                })
                .collect();
            Some(ir.add_expr(IrExpr::When { branches }))
        }
        _ => None,
    }
}
