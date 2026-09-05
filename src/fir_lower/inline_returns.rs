//! Checked return realization at local-callable boundaries.
//!
//! A materialized callable and its inline template have different control-flow ownership. A
//! depth-zero return is a real return in the callable method, but exits only that invocation after
//! the callable is spliced. Deeper returns cross one lexical callable boundary. This module turns
//! those already-checked depths into structural common IR once, before an inline template is
//! published to any source or backend inliner.

use crate::ir::{ExprId, IrConst, IrExpr, IrFile};
use crate::types::Ty;

/// Return nodes remain ordinary common IR in a materialized callable. This sparse checked fact is
/// consumed when its private inline-template copy crosses the callable boundary. Nested callable
/// bodies are independent templates, so only their capture operands belong to this traversal.
pub(super) fn reachable_checked_returns(ir: &IrFile, root: ExprId) -> Vec<(ExprId, u32)> {
    fn visit(
        ir: &IrFile,
        expression: ExprId,
        seen: &mut std::collections::HashSet<ExprId>,
        out: &mut Vec<(ExprId, u32)>,
    ) {
        if !seen.insert(expression) {
            return;
        }
        if let Some(depth) = ir.checked_return_depths.get(&expression).copied() {
            out.push((expression, depth));
        }
        if let IrExpr::Lambda { captures, .. } = ir.expr(expression) {
            for &capture in captures {
                visit(ir, capture, seen, out);
            }
            return;
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            visit(ir, child, seen, out);
        }
    }

    let mut out = Vec::new();
    visit(ir, root, &mut std::collections::HashSet::new(), &mut out);
    out
}

/// Prepare the value-producing template for one inline-callable boundary. Local returns become a
/// labelled structural exit; non-local returns remain ordinary returns and move one lexical frame
/// nearer their checked target. Consumers clone this prepared template without repeating either
/// decision.
pub(super) fn prepare_inline_template(
    ir: &mut IrFile,
    root: ExprId,
    result: Ty,
    value_needed: bool,
    next_temporary: &mut u32,
) -> Option<ExprId> {
    let returns = reachable_checked_returns(ir, root);
    let owns_returns = returns.iter().any(|(_, depth)| *depth == 0);
    for &(returned, depth) in &returns {
        if depth > 0 {
            ir.checked_return_depths.insert(returned, depth - 1);
        }
    }
    if !owns_returns {
        return Some(root);
    }

    let label = format!("$fir_inline_return_{root}");
    let result_slot = (value_needed && result != Ty::Unit).then(|| {
        let slot = *next_temporary;
        *next_temporary = (*next_temporary)
            .checked_add(1)
            .expect("too many FIR temporaries");
        slot
    });
    for (returned, depth) in returns {
        if depth != 0 {
            continue;
        }
        ir.checked_return_depths.remove(&returned);
        let IrExpr::Return(value) = ir.expr(returned).clone() else {
            return None;
        };
        let mut statements = Vec::new();
        if let Some(value) = value {
            if let Some(slot) = result_slot {
                statements.push(ir.add_expr(IrExpr::SetValue { var: slot, value }));
            } else {
                statements.push(value);
            }
        }
        statements.push(ir.add_expr(IrExpr::Break {
            label: Some(label.clone()),
        }));
        ir.exprs[returned as usize] = IrExpr::Block {
            stmts: statements,
            value: None,
        };
    }

    let mut frame_statements = Vec::new();
    if let Some(slot) = result_slot {
        let initial = ir.add_expr(IrExpr::Const(IrConst::zero_for_value_type(result)));
        frame_statements.push(ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: result,
            init: Some(initial),
            named: false,
        }));
    }
    let mut body_statements = if let Some(slot) = result_slot {
        vec![ir.add_expr(IrExpr::SetValue {
            var: slot,
            value: root,
        })]
    } else {
        vec![root]
    };
    body_statements.push(ir.add_expr(IrExpr::Break {
        label: Some(label.clone()),
    }));
    let body = ir.add_expr(IrExpr::Block {
        stmts: body_statements,
        value: None,
    });
    let true_value = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    frame_statements.push(ir.add_expr(IrExpr::While {
        cond: true_value,
        body,
        update: None,
        post_test: false,
        label: Some(label),
    }));
    let value = match result_slot {
        Some(slot) => Some(ir.add_expr(IrExpr::GetValue(slot))),
        None if value_needed => Some(ir.add_expr(IrExpr::UnitInstance)),
        None => None,
    };
    Some(ir.add_expr(IrExpr::Block {
        stmts: frame_statements,
        value,
    }))
}

/// Lambda implementation methods return language `Unit` through a value carrier. Make every
/// checked local bare return produce that value just as the implicit epilogue does. Ordinary
/// Unit-returning source functions call this with `unit_as_value == false` and retain a void return.
pub(super) fn materialize_unit_callable_returns(ir: &mut IrFile, roots: &[ExprId]) {
    for root in roots {
        for (returned, depth) in reachable_checked_returns(ir, *root) {
            if depth != 0 || !matches!(ir.expr(returned), IrExpr::Return(None)) {
                continue;
            }
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.exprs[returned as usize] = IrExpr::Return(Some(unit));
        }
    }
}
