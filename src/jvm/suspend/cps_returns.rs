//! The returns of a body that takes the CPS signature: every one yields an `Object`, and the body
//! ends in one.

use super::{object_ty, stmt_diverges};
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile, IrTypeOp};

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
