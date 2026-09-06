//! Statement/value-region normalization required before coroutine state splitting.
//!
//! These rewrites preserve checked Kotlin meaning while making the consumer position explicit in
//! common IR. They neither discover suspension points nor choose JVM representations.

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// Common IR retains the inferred expression type even when a `try` occurs in a statement region.
/// Suspension normalization needs the consumer fact explicitly: only statement-position tries may
/// discard branch values and become `Unit`; a try used by a return, assignment, call argument, or
/// another value consumer keeps its checked result unchanged.
pub(super) fn normalize_statement_try_results(
    ir: &mut IrFile,
    expression: ExprId,
    statement: bool,
) {
    match ir.exprs[expression as usize].clone() {
        IrExpr::Lambda { .. } => {}
        IrExpr::Block { stmts, value } => {
            for child in stmts {
                normalize_statement_try_results(ir, child, true);
            }
            if let Some(value) = value {
                normalize_statement_try_results(ir, value, statement);
            }
        }
        IrExpr::When { branches } => {
            for (condition, body) in branches {
                if let Some(condition) = condition {
                    normalize_statement_try_results(ir, condition, false);
                }
                normalize_statement_try_results(ir, body, statement);
            }
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            if statement {
                if let IrExpr::Try { result, .. } = &mut ir.exprs[expression as usize] {
                    *result = Ty::Unit;
                }
            }
            for region in std::iter::once(body)
                .chain(catches.into_iter().map(|catch| catch.body))
                .chain(finally)
            {
                normalize_statement_try_results(ir, region, statement);
            }
        }
        IrExpr::While {
            cond, body, update, ..
        } if statement => {
            normalize_statement_try_results(ir, cond, false);
            normalize_statement_try_results(ir, body, true);
            if let Some(update) = update {
                normalize_statement_try_results(ir, update, true);
            }
        }
        IrExpr::TypeOp { arg, .. } if statement => {
            normalize_statement_try_results(ir, arg, true);
        }
        _ => {}
    }
}

/// A loop/protected-region body is a statement region even when common lowering preserved a tail
/// expression as the block value. Evaluate that value for effect before state splitting.
pub(super) fn demote_block_value_to_statement(ir: &mut IrFile, block: ExprId) {
    let IrExpr::Block {
        mut stmts,
        value: Some(value),
    } = ir.exprs[block as usize].clone()
    else {
        return;
    };
    stmts.push(value);
    ir.exprs[block as usize] = IrExpr::Block { stmts, value: None };
}

/// Rewrite each `Variable { init: Block { stmts: prelude, value: Some(inner) } }` into the
/// `prelude` statements followed by `Variable { init: inner }`. Elvis and primitive safe-call elvis
/// lower to such a block-valued initializer; lifting it exposes the inner conditional to the
/// state-machine flattener. Traversal stops at lambdas, which own another value namespace.
pub(super) fn normalize_block_inits(ir: &mut IrFile, expression: ExprId) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    if let IrExpr::Block { stmts, value } = ir.exprs[expression as usize].clone() {
        let mut out: Vec<ExprId> = Vec::with_capacity(stmts.len());
        let mut changed = false;
        for statement in stmts {
            if let IrExpr::Variable {
                index,
                ref ty,
                init: Some(init),
                named,
            } = ir.exprs[statement as usize].clone()
            {
                if let IrExpr::Block {
                    stmts: prelude,
                    value: inner_value,
                } = ir.exprs[init as usize].clone()
                {
                    if ir.intrinsic_suspension_points.contains_key(&init) {
                        out.push(statement);
                        continue;
                    }
                    let inner = match inner_value {
                        Some(inner) => Some(inner),
                        None if *ty == Ty::Unit => Some(ir.add_expr(IrExpr::UnitInstance)),
                        None => None,
                    };
                    if let Some(inner) = inner {
                        out.extend(prelude);
                        out.push(ir.add_expr(IrExpr::Variable {
                            index,
                            ty: *ty,
                            init: Some(inner),
                            named,
                        }));
                        changed = true;
                        continue;
                    }
                }
            }
            out.push(statement);
        }
        ir.exprs[expression as usize] = IrExpr::Block { stmts: out, value };
        if changed {
            normalize_block_inits(ir, expression);
            return;
        }
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        normalize_block_inits(ir, child);
    }
}

/// A statement-shaped conditional used as a `Unit` value leaves nothing on the operand stack.
/// Split it into the conditional statement followed by the semantic `Unit` singleton before the
/// state-machine emitter assigns or returns the value.
pub(super) fn split_unit_conditional_returns(ir: &mut IrFile, body: ExprId, unit_ret: bool) {
    let unwrap_when = |ir: &IrFile, mut expression: ExprId| {
        while let IrExpr::TypeOp { arg, .. } = ir.exprs[expression as usize] {
            expression = arg;
        }
        matches!(ir.exprs[expression as usize], IrExpr::When { .. }).then_some(expression)
    };
    let IrExpr::Block { stmts, value } = ir.exprs[body as usize].clone() else {
        return;
    };
    let mut out = Vec::with_capacity(stmts.len() + 1);
    let mut changed = false;
    for statement in stmts {
        match ir.exprs[statement as usize].clone() {
            IrExpr::Return(Some(value)) if unit_ret => {
                if let Some(conditional) = unwrap_when(ir, value) {
                    out.push(conditional);
                    let unit = ir.add_expr(IrExpr::UnitInstance);
                    out.push(ir.add_expr(IrExpr::Return(Some(unit))));
                    changed = true;
                    continue;
                }
            }
            IrExpr::Variable {
                index,
                ty,
                init: Some(init),
                named,
            } if ty == Ty::Unit => {
                if let Some(conditional) = unwrap_when(ir, init) {
                    out.push(conditional);
                    let unit = ir.add_expr(IrExpr::UnitInstance);
                    out.push(ir.add_expr(IrExpr::Variable {
                        index,
                        ty,
                        init: Some(unit),
                        named,
                    }));
                    changed = true;
                    continue;
                }
            }
            _ => {}
        }
        out.push(statement);
    }
    if changed {
        ir.exprs[body as usize] = IrExpr::Block { stmts: out, value };
    }
}
