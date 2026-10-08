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
/// consumed when its private inline-template copy crosses the callable boundary. A nested lambda's
/// inline template is spliced inside this body wherever it is inlined, so a return it still carries
/// (one its own boundary already moved a frame nearer) crosses this boundary too; its separately
/// materialized implementation is not part of this body.
pub(super) fn reachable_checked_returns(ir: &IrFile, root: ExprId) -> Vec<(ExprId, u32)> {
    returns_by_body(ir, root)
        .into_iter()
        .map(|found| (found.returned, found.depth))
        .collect()
}

/// A checked return reachable from a callable's body, and whether a nested lambda body holds it.
pub(super) struct ReachableReturn {
    pub(super) returned: ExprId,
    pub(super) depth: u32,
    /// The return sits in the template of a lambda nested in the body, which numbers its values
    /// separately: it cannot name the body's values.
    pub(super) nested: bool,
}

/// [`reachable_checked_returns`], with whether each return sits in a nested lambda's template.
pub(super) fn returns_by_body(ir: &IrFile, root: ExprId) -> Vec<ReachableReturn> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![(root, false)];
    while let Some((expression, nested)) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(depth) = ir.checked_return_depths.get(&expression).copied() {
            out.push(ReachableReturn {
                returned: expression,
                depth,
                nested,
            });
        }
        if let IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } = ir.expr(expression)
        {
            pending.extend(captures.iter().map(|&capture| (capture, nested)));
            pending.extend(inline_body.map(|body| (body, true)));
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
            pending.push((child, nested))
        });
    }
    out
}

/// The statements that leave inline-return frame `label` with `value`: store it as the frame's
/// result, then `break`. A return in the frame's own body stores by the result's value index; one
/// in a nested lambda's template names the frame instead. With no result, the value still runs.
pub(super) fn frame_exit(
    ir: &mut IrFile,
    label: &str,
    result: Option<u32>,
    nested: bool,
    value: Option<ExprId>,
    source: ExprId,
) -> IrExpr {
    let mut stmts = Vec::new();
    let store = if let Some(value) = value {
        let store = match (result, nested) {
            (Some(var), false) => Some(ir.add_expr(IrExpr::SetValue { var, value })),
            (Some(_), true) => Some(ir.add_expr(IrExpr::SetFrameResult {
                frame: label.to_string(),
                value,
            })),
            (None, _) => {
                stmts.push(value);
                None
            }
        };
        if let Some(store) = store {
            stmts.push(store);
        }
        store
    } else {
        None
    };
    // The break replaces `source`. It keeps that return's line and inline-copy identity so the
    // `goto` after `finally` is the rewritten return, not an unlabeled jump the goto cleanup
    // threads into the finalizer's own exit. The store takes the same line, marked at the
    // `istore`/`astore` rather than at the value that precedes it.
    let exit = ir.add_expr(IrExpr::Break {
        label: Some(label.to_string()),
    });
    let line = ir
        .expr_lines
        .get(&source)
        .copied()
        .or_else(|| ir.expr_source_lines.get(&source).copied());
    if let Some(line) = line {
        ir.expr_lines.insert(exit, line);
        ir.expr_source_lines.insert(exit, line);
        if let Some(store) = store {
            ir.expr_source_lines.insert(store, line);
            ir.copy_inline_copy_mark(source, store);
        }
    }
    ir.copy_inline_copy_mark(source, exit);
    stmts.push(exit);
    IrExpr::Block { stmts, value: None }
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
    let returns = returns_by_body(ir, root);
    let owns_returns = returns.iter().any(|found| found.depth == 0);
    for found in &returns {
        if found.depth > 0 {
            ir.checked_return_depths
                .insert(found.returned, found.depth - 1);
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
    for found in returns {
        if found.depth != 0 {
            continue;
        }
        ir.checked_return_depths.remove(&found.returned);
        let IrExpr::Return(value) = ir.expr(found.returned).clone() else {
            return None;
        };
        ir.exprs[found.returned as usize] =
            frame_exit(ir, &label, result_slot, found.nested, value, found.returned);
    }

    let mut frame_statements = Vec::new();
    if let Some(slot) = result_slot {
        let initial = ir.add_expr(IrExpr::Const(IrConst::zero_for_value_type(result)));
        let declaration = ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: result,
            init: Some(initial),
            named: false,
        });
        ir.inline_return_frames.insert(declaration, label.clone());
        frame_statements.push(declaration);
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
    let frame = ir.add_expr(IrExpr::Block {
        stmts: frame_statements,
        value,
    });
    let frame_type = if result_slot.is_some() {
        result
    } else {
        Ty::Unit
    };
    ir.logical_types.insert(frame, frame_type);
    Some(frame)
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
