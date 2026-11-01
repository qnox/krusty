//! The returns of a body that takes the CPS signature: every one yields an `Object`, and the body
//! ends in one.

use super::{object_ty, stmt_diverges};
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Ensure a leaf suspend fn's body ends with a `return` (its CPS method returns `Object`; without this a
/// fall-through body verifies as "control flow falls through code end"). Idempotent: a body already
/// ending in `return`/`throw` is left alone. A trailing VALUE becomes `return box(value)`; a statement
/// body / a `Unit` fn runs the body for effect and returns `Unit.INSTANCE`, which it returns.
///
/// Returns every synthesized `Unit.INSTANCE` so the caller can give each physical return the
/// function's closing source line.
pub(super) fn ensure_tail_return(ir: &mut IrFile, body: ExprId, unit_ret: bool) -> Vec<ExprId> {
    let IrExpr::Block { stmts, value } = ir.exprs[body as usize].clone() else {
        return Vec::new();
    };
    let mut stmts = stmts;
    let mut added_units = Vec::new();
    match value {
        Some(v) if !unit_ret => {
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: v,
                type_operand: object_ty(),
            });
            stmts.push(ir.add_expr(IrExpr::Return(Some(boxed))));
        }
        Some(v) => {
            // A terminal safe call keeps its selected arm through the physical return. kotlinc
            // writes one `Unit.INSTANCE; areturn` after the selector and another after the null
            // arm instead of joining them ahead of a shared return.
            if let Some(units) = return_unit_from_safe_call_arms(ir, v) {
                stmts.push(v);
                added_units.extend(units);
                ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
                return added_units;
            }
            // Every other `Unit` tail runs for effect, then returns the singleton once.
            stmts.push(v);
            let unit = ir.add_expr(IrExpr::UnitInstance);
            stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
            added_units.push(unit);
        }
        None => {
            // A block-bodied `Unit` function records its final safe call as the last statement,
            // rather than as the block value handled above. It has the same terminal-arm layout.
            if let Some(units) = stmts
                .last()
                .copied()
                .and_then(|tail| return_unit_from_safe_call_arms(ir, tail))
            {
                added_units.extend(units);
                ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
                return added_units;
            }
            // Statement body. If it doesn't already terminate, return `Unit.INSTANCE` (a leaf suspend fn
            // with a `Unit`/no-value body).
            let terminates = stmts.last().is_some_and(|&s| stmt_diverges(ir, s));
            if !terminates {
                let unit = ir.add_expr(IrExpr::UnitInstance);
                stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
                added_units.push(unit);
            }
        }
    }
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
    added_units
}

/// Put a `Unit` return into each live arm of a terminal safe call.
///
/// The frontend's null-guard identity is the semantic distinction from an arbitrary conditional;
/// wrapper blocks and checked coercions merely carry that identity to the function tail.
fn return_unit_from_safe_call_arms(ir: &mut IrFile, mut expression: ExprId) -> Option<Vec<ExprId>> {
    loop {
        match ir.exprs[expression as usize] {
            IrExpr::TypeOp { arg, .. } => expression = arg,
            IrExpr::Block {
                value: Some(value), ..
            } => expression = value,
            _ => break,
        }
    }
    if !ir.null_guards.contains(&expression) {
        return None;
    }
    let IrExpr::When { branches } = ir.exprs[expression as usize].clone() else {
        return None;
    };
    let mut units = Vec::with_capacity(branches.len());
    let branches = branches
        .into_iter()
        .map(|(condition, branch)| {
            if stmt_diverges(ir, branch) {
                return (condition, branch);
            }
            let unit = ir.add_expr(IrExpr::UnitInstance);
            units.push(unit);
            let returned = ir.add_expr(IrExpr::Return(Some(unit)));
            let branch = match ir.exprs[branch as usize].clone() {
                IrExpr::Block { mut stmts, value } => {
                    stmts.extend(value);
                    stmts.push(returned);
                    ir.add_expr(IrExpr::Block { stmts, value: None })
                }
                _ => ir.add_expr(IrExpr::Block {
                    stmts: vec![branch, returned],
                    value: None,
                }),
            };
            (condition, branch)
        })
        .collect();
    ir.exprs[expression as usize] = IrExpr::When { branches };
    Some(units)
}

/// Wrap the value of every `Return` reachable from `e` in an `ImplicitCoercion` to `Object`.
pub(super) fn box_returns(ir: &mut IrFile, e: ExprId) -> bool {
    match ir.exprs[e as usize].clone() {
        IrExpr::Return(None) => {
            // The CPS method returns `Object`, so a BARE `return` — a `Unit`-returning suspend fn's early
            // exit (`x ?: return`, `if (…) return`) — must `areturn Unit.INSTANCE`, not a void `return`
            // (which fails verification: "Method expects a return value"). Every other return in the
            // assembled state machine already yields a value.
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.exprs[e as usize] = IrExpr::Return(Some(unit));
            true
        }
        IrExpr::Return(Some(v)) => {
            // Already an Object-yielding suspension return (COROUTINE_SUSPENDED) needs no box; but a
            // double coercion to Object is harmless (identity on a reference), so box uniformly.
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: v,
                type_operand: object_ty(),
            });
            ir.exprs[e as usize] = IrExpr::Return(Some(boxed));
            box_returns(ir, v)
        }
        // A lambda argument (`m.map { it.value }`) is a VALUE whose impl function is a separate body,
        // but the canonical child walk exposes only what this function owns: its captures, which are
        // evaluated in this frame, and its retained `inline_body`. A `Return` that survives in an
        // inline body is a NON-LOCAL return by construction — the template preparation already turned
        // every local `return@label` into a labelled exit — and the classpath-inline splice realizes
        // it as a return from THIS method. It must box exactly like a return written in the body
        // proper: `twice(1) { x -> if (x == 2) return 100; x }` in a CPS body otherwise leaves
        // `bipush 100; areturn` (VerifyError: "Bad type on operand stack"), and a bare `return` a void
        // `return` where the `Object` result is expected. So a lambda is NOT a leaf here.
        //
        // The invariant covers returns that leave the template being spliced. A `return@outer` in a
        // lambda nested inside another emit-time-spliced lambda is prepared only by the INNER
        // template (`reachable_checked_returns` stops at a nested lambda), survives as a raw `Return`,
        // and the emitter realizes it as this method's — a pre-existing gap; boxing it here keeps
        // that shape at least loadable and is not what makes it wrong.
        //
        // Return boxing is a tree rewrite, not an IR-shape validator. Use the canonical child relation
        // so adding an unrelated expression kind cannot make an otherwise valid suspend function
        // unsupported. Unsupported coroutine control-flow is rejected by the state-machine flattener,
        // where that decision belongs.
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, e, &mut |child| children.push(child));
            children.into_iter().all(|child| box_returns(ir, child))
        }
    }
}

/// Where a tail suspension point stands in its parent, so the `return` built around it can take
/// its place.
#[derive(Clone, Copy)]
enum TailSlot {
    Statement(ExprId, usize),
    Value(ExprId),
    Branch(ExprId, usize),
}

/// kotlinc's `TailCallOptimizationLowering`: a suspension point in tail position that no `return`
/// already takes as its value is returned at the function's end, coerced to `Unit` in a `Unit`
/// function. The return carries the body's closing line, which kotlinc marks once the value is on
/// the stack, at the `areturn`; whatever followed the call becomes unreachable.
///
/// `tail_calls` are the suspension points whose result the function may return: every one in a
/// `Unit` function, otherwise those of the function's own result type. Tail positions are the last
/// statement before the first `return` (or before a bare `return` in a `Unit` function, the
/// statement ahead of it), through blocks, `when` branches and coercions to `Unit`.
pub(super) fn return_tail_calls(
    ir: &mut IrFile,
    body: ExprId,
    tail_calls: &[ExprId],
    unit_ret: bool,
    close_line: u32,
) {
    let mut found = Vec::new();
    collect_tail_calls(ir, body, None, false, tail_calls, unit_ret, &mut found);
    for (node, slot) in found {
        let value = if unit_ret {
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.add_expr(IrExpr::Block {
                stmts: vec![node],
                value: Some(unit),
            })
        } else {
            node
        };
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        ir.expr_source_lines.insert(returned, close_line);
        match slot {
            TailSlot::Statement(block, index) => {
                if let IrExpr::Block { stmts, .. } = &mut ir.exprs[block as usize] {
                    stmts[index] = returned;
                }
            }
            TailSlot::Value(block) => {
                if let IrExpr::Block { value, .. } = &mut ir.exprs[block as usize] {
                    *value = Some(returned);
                }
            }
            TailSlot::Branch(when, index) => {
                if let IrExpr::When { branches } = &mut ir.exprs[when as usize] {
                    branches[index].1 = returned;
                }
            }
        }
    }
}

fn collect_tail_calls(
    ir: &IrFile,
    expression: ExprId,
    slot: Option<TailSlot>,
    immediate_return: bool,
    tail_calls: &[ExprId],
    unit_ret: bool,
    found: &mut Vec<(ExprId, TailSlot)>,
) {
    let recurse = |expression, slot, immediate_return, found: &mut Vec<_>| {
        collect_tail_calls(
            ir,
            expression,
            slot,
            immediate_return,
            tail_calls,
            unit_ret,
            found,
        );
    };
    match &ir.exprs[expression as usize] {
        _ if tail_calls.contains(&expression) => {
            if let Some(slot) = slot.filter(|_| !immediate_return) {
                found.push((expression, slot));
            }
        }
        IrExpr::Block { stmts, value } => {
            // kotlinc's block ends in its value as its last statement.
            let statements: Vec<ExprId> = stmts.iter().copied().chain(*value).collect();
            if let Some(index) = tail_statement(ir, &statements, unit_ret) {
                let slot = if index == stmts.len() {
                    TailSlot::Value(expression)
                } else {
                    TailSlot::Statement(expression, index)
                };
                recurse(statements[index], Some(slot), false, found);
            }
        }
        IrExpr::When { branches } => {
            for (index, &(_, result)) in branches.iter().enumerate() {
                recurse(
                    result,
                    Some(TailSlot::Branch(expression, index)),
                    false,
                    found,
                );
            }
        }
        &IrExpr::Return(Some(value)) => recurse(value, slot, true, found),
        &IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if type_operand == Ty::Unit && tail_calls.contains(&arg) => {
            if let Some(slot) = slot.filter(|_| !immediate_return) {
                found.push((expression, slot));
            }
        }
        _ => {}
    }
}

/// kotlinc's `findTailCall` over one block's statements: the first `return`, unless it is a `Unit`
/// function's bare `return`, whose tail is the statement ahead of it; with no `return`, the last
/// statement.
fn tail_statement(ir: &IrFile, stmts: &[ExprId], unit_ret: bool) -> Option<usize> {
    let Some(index) = stmts
        .iter()
        .position(|&statement| matches!(ir.exprs[statement as usize], IrExpr::Return(_)))
    else {
        return stmts.len().checked_sub(1);
    };
    let bare_unit_return = unit_ret
        && match ir.exprs[stmts[index] as usize] {
            IrExpr::Return(None) => true,
            IrExpr::Return(Some(value)) => matches!(ir.exprs[value as usize], IrExpr::UnitInstance),
            _ => false,
        };
    if bare_unit_return {
        tail_statement(ir, &stmts[..index], unit_ret)
    } else {
        Some(index)
    }
}
