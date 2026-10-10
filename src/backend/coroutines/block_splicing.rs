//! Lifting a statement's value-position block into the statement list, so a suspension buried in
//! the block's statements surfaces where the hoister and the flattener see it.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};

use super::control_flow::stmt_diverges;

/// Lift a value-position `Block` out of a top-level statement's direct operand, so a suspension buried in
/// the block's statements surfaces at the top level where the hoister/flattener handle it. An elvis /
/// safe-call whose subject suspends lowers to `{ val t = susp()…; when{…} }` in `return`/`val =`/assign
/// position — a block the hoister can't see into, so the flattener would bail. This rewrites:
///   `return { s…; v }`      → `s…; return v`
///   `val x = { s…; v }`     → `s…; val x = v`
///   `x = { s…; v }`         → `s…; x = v`
///   `capture = { s…; v }`   → `s…; capture = v`
/// A value-bearing block is spliced into its consumer. A value-less block is spliced only when it
/// definitely diverges, in which case the consumer can never execute and is removed. Re-runs until
/// settled, so nested blocks (safe-call inside elvis) fully unfold; lifted statements are reprocessed.
pub(crate) fn splice_return_blocks(ir: &mut IrFile, b: ExprId) {
    // Normalize lexical child blocks before their parent. A return/value wrapper can sit inside an
    // `if`/`when` or catch body just as legitimately as at the function body's top level; making the
    // transform depend on that container was the source of a private conditional-tail regression.
    // Stop the discovery walk at each nested block and recurse from there, so every block is processed
    // exactly within its own lexical boundary: statements may move inside that block, never out through
    // a branch/try owner. `Lambda` and `While` are LOWERING boundaries, not merely nested lexical
    // regions: a lambda's `inline_body` is a substitution template and collection HOFs have already
    // expanded their template into a loop by this phase. Descending into either here can reshape the
    // loop body before the dedicated loop/state-machine flattening establishes its labels and spills;
    // that made otherwise-supported suspending `map`/`flatMap` bodies decline emission. Their own
    // lowering path presents the relevant blocks to this normalizer when it is safe to do so. The
    // visited set also makes shared arena nodes and malformed cycles safe.
    let mut nested_blocks = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::from([b]);
    for_each_child(&ir.exprs, b, &mut |child| pending.push(child));
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        if matches!(
            ir.exprs[node as usize],
            IrExpr::Lambda { .. } | IrExpr::While { .. }
        ) {
            continue;
        }
        if matches!(ir.exprs[node as usize], IrExpr::Block { .. }) {
            nested_blocks.push(node);
            continue;
        }
        for_each_child(&ir.exprs, node, &mut |child| pending.push(child));
    }
    for nested in nested_blocks {
        splice_return_blocks(ir, nested);
    }

    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len());
    let mut changed = false;
    let mut value = value;
    let n = stmts.len();
    for (i, s) in stmts.into_iter().enumerate() {
        // A bare `Block` STATEMENT is pure grouping (IR locals are flat-indexed) — lift its statements
        // into the parent so a labeled break/suspension buried in the nested block reaches the top-level
        // flattening stream rather than surviving as a structured node. Its trailing VALUE: when the block
        // is the parent's LAST statement and the parent has no value of its own (an `= withLock { … }`
        // expression body lowers to `{ <withLock block> }`), the value IS the body's result — promote it
        // to the parent value so `ensure_tail_return` returns it. Otherwise it sits in statement position
        // and is run for effect.
        if let IrExpr::Block {
            stmts: bs,
            value: bv,
        } = ir.exprs[s as usize].clone()
        {
            // A `suspendCoroutineUninterceptedOrReturn` block is a registered suspension point,
            // not grouping — splicing it would orphan the suspension-point id.
            if ir.intrinsic_suspension_points.contains_key(&s) {
                new_stmts.push(s);
                continue;
            }
            new_stmts.extend(bs);
            if let Some(v) = bv {
                if i + 1 == n && value.is_none() {
                    value = Some(v);
                } else {
                    new_stmts.push(v);
                }
            }
            changed = true;
            continue;
        }
        // A checked coercion around a value-less block is equally transparent when that block
        // cannot fall through (notably `Nothing?` around an inlined labelled return). There is no
        // value on which the coercion could execute, so expose the structural exit to state-machine
        // splitting just as `diverging_value_consumer_statements` does for a return/store operand.
        if let Some(statements) = diverging_block_statements(ir, s) {
            new_stmts.extend(statements);
            changed = true;
            continue;
        }
        let statement = ir.exprs[s as usize].clone();
        // A checked inline/labeled return can leave `return { effects; return value }`: the inner
        // value-less block never falls through, so evaluating the outer consumer is impossible.
        // Lift its statements and drop the unreachable consumer. Apply the same semantic rule to
        // local/captured writes whose stable holder has no prior effect; this is the divergent twin
        // of the value-block normalization immediately below.
        if let Some(statements) = diverging_value_consumer_statements(ir, &statement) {
            new_stmts.extend(statements);
            changed = true;
            continue;
        }
        let spliced = match statement {
            IrExpr::Return(Some(inner)) => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::Return(Some(bv)))
                }),
            IrExpr::Variable {
                index,
                ty,
                init: Some(inner),
                named,
            } => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::Variable {
                        index,
                        ty,
                        init: Some(bv),
                        named,
                    })
                }),
            IrExpr::SetValue { var, value: inner } => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::SetValue { var, value: bv })
                }),
            IrExpr::RefSet {
                holder,
                elem,
                value: inner,
            } if matches!(ir.exprs[holder as usize], IrExpr::GetValue(_)) => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::RefSet {
                        holder,
                        elem,
                        value: bv,
                    })
                }),
            _ => None,
        };
        match spliced {
            Some(ns) => {
                new_stmts.push(ns);
                changed = true;
            }
            None => new_stmts.push(s),
        }
    }
    // The block's own trailing value may itself be a value-carrying block whose statements must surface.
    let value = match value {
        Some(v) => match value_block(ir, v) {
            Some((bs, bv)) => {
                new_stmts.extend(bs);
                changed = true;
                Some(bv)
            }
            None => Some(v),
        },
        None => None,
    };
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
        // A lifted statement may itself carry a value-block (safe-call nested in elvis) — repeat.
        splice_return_blocks(ir, b);
    }
}

/// Statements of a structural block that cannot produce a value or fall through. Checked
/// coercions around it are observationally irrelevant because control never reaches the coercion.
pub(crate) fn diverging_control_core(ir: &IrFile, expression: ExprId) -> Option<ExprId> {
    if !stmt_diverges(ir, expression) {
        return None;
    }
    match &ir.exprs[expression as usize] {
        IrExpr::TypeOp { arg, .. } => diverging_control_core(ir, *arg),
        _ => Some(expression),
    }
}

fn diverging_block_statements(ir: &IrFile, expression: ExprId) -> Option<Vec<ExprId>> {
    match &ir.exprs[diverging_control_core(ir, expression)? as usize] {
        IrExpr::Block { stmts, value: None } => Some(stmts.clone()),
        _ => None,
    }
}

/// Statements of a value operand that can never return to its surrounding consumer. Local/value
/// declarations and local assignments have no separately evaluated target; a captured-cell write is
/// equally safe only when its holder is already a stable local read. In those cases the operand's
/// statements can replace the unreachable consumer without changing evaluation order.
pub(super) fn diverging_value_consumer_statements(
    ir: &IrFile,
    consumer: &IrExpr,
) -> Option<Vec<ExprId>> {
    let operand = match consumer {
        IrExpr::Return(Some(operand))
        | IrExpr::Variable {
            init: Some(operand),
            ..
        }
        | IrExpr::SetValue { value: operand, .. } => *operand,
        IrExpr::RefSet { holder, value, .. }
            if matches!(ir.exprs[*holder as usize], IrExpr::GetValue(_)) =>
        {
            *value
        }
        _ => return None,
    };
    (!ir.intrinsic_suspension_points.contains_key(&operand))
        .then(|| diverging_block_statements(ir, operand))
        .flatten()
}

/// If `e` is a value-bearing `Block`, return `(its statements, its value)`; else `None`.
pub(super) fn value_block(ir: &mut IrFile, e: ExprId) -> Option<(Vec<ExprId>, ExprId)> {
    // A `suspendCoroutineUninterceptedOrReturn` block is a registered suspension point, not an
    // evaluation-order wrapper: lifting its statements would orphan the suspension-point id.
    if ir.intrinsic_suspension_points.contains_key(&e) {
        return None;
    }
    match ir.exprs[e as usize].clone() {
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            // The block is only an evaluation-order wrapper. When it is removed, retain the exact
            // checked result type on the expression that replaces it; later coroutine
            // normalization uses that metadata to allocate branch-result storage without
            // re-inferring a type from backend shapes.
            preserve_replacement_logical_type(ir, e, value);
            Some((stmts, value))
        }
        // Checked result coercions do not make a source grouping block semantically observable.
        // Lift the block's statements and reapply the exact operation to its value; no type or
        // conversion decision is repeated here.
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let (statements, value) = value_block(ir, arg)?;
            let value = ir.add_expr(IrExpr::TypeOp {
                op,
                arg: value,
                type_operand,
            });
            Some((statements, value))
        }
        _ => None,
    }
}

pub(super) fn preserve_replacement_logical_type(
    ir: &mut IrFile,
    original: ExprId,
    replacement: ExprId,
) {
    if let Some(ty) = ir.logical_types.get(&original).copied() {
        ir.logical_types.entry(replacement).or_insert(ty);
    }
}
