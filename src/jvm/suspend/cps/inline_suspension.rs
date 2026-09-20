//! Finding the suspensions that the IR coroutine machine cannot see.
//!
//! A lambda passed to a classpath `inline` function is not a closure: once the call is spliced, its
//! body runs in the enclosing method's own frame. The IR machine nevertheless stops at every
//! `Lambda` (`expr_calls_suspend` does so deliberately), so a suspension in such a body is invisible
//! to it — the enclosing function is classified as having no suspension point at all, and the
//! spliced call is then emitted with no continuation to pass. That is the
//! `call arity mismatch` bail.
//!
//! These are the suspensions whose machine moves to emission, where the spliced body's own locals
//! exist. See `docs/JVM_INLINE_BEFORE_CPS.md`.
//!
//! Carrying an `inline_body` is NOT by itself proof that the body will run in this frame: the
//! emitter may decline the splice, in which case the lambda is realized as an ordinary `invoke` and
//! the suspension belongs to that method instead. So this answer alone must never take a function
//! away from the IR machine. The routing gate is the conjunction — a function the IR machine finds
//! no suspension point in, which nevertheless has one here — because that is exactly the function
//! that would otherwise be emitted with no continuation to pass.

use super::super::is_suspension_point;
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use std::collections::HashSet;

/// Suspension points inside the `inline_body` of a lambda reached from `body`, in encounter order.
///
/// A nested lambda that is *not* inlined is a real closure boundary and stops the walk, exactly as
/// it does for the IR machine: its body belongs to a generated `invoke`, not to this frame.
pub(crate) fn spliced_inline_suspensions(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> Vec<ExprId> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    walk_frame(ir, body, suspend_set, false, &mut seen, &mut found);
    found
}

/// Whether any suspension reached from `body` is one the IR machine cannot see.
pub(crate) fn suspends_inside_a_spliced_inline_body(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> bool {
    !spliced_inline_suspensions(ir, body, suspend_set).is_empty()
}

/// `inlined` records whether this expression is already inside some lambda's `inline_body`; only
/// then does a suspension count, because only then is it invisible to the IR machine.
fn walk_frame(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    inlined: bool,
    seen: &mut HashSet<ExprId>,
    found: &mut Vec<ExprId>,
) {
    if !seen.insert(expression) {
        return;
    }
    if inlined && is_suspension_point(ir, expression, suspend_set) {
        found.push(expression);
    }
    if let IrExpr::Lambda {
        captures,
        inline_body,
        ..
    } = &ir.exprs[expression as usize]
    {
        // The captures are evaluated by THIS frame, whatever the lambda is.
        for &capture in captures {
            walk_frame(ir, capture, suspend_set, inlined, seen, found);
        }
        // A spliced body runs in this frame; a real closure's does not.
        if let Some(&body) = inline_body.as_ref() {
            walk_frame(ir, body, suspend_set, true, seen, found);
        }
        return;
    }
    for_each_child(&ir.exprs, expression, &mut |child| {
        walk_frame(ir, child, suspend_set, inlined, seen, found)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrIntrinsicSuspensionKind, IrIntrinsicSuspensionPoint};
    use crate::types::Ty;

    /// Mark `point` as a suspension the way an intrinsic one is recorded, so these tests need no
    /// function table.
    fn suspension(ir: &mut IrFile) -> ExprId {
        let point = ir.add_expr(IrExpr::UnitInstance);
        ir.intrinsic_suspension_points.insert(
            point,
            IrIntrinsicSuspensionPoint {
                result: Ty::Unit,
                kind: IrIntrinsicSuspensionKind::Safe,
            },
        );
        point
    }

    fn lambda(ir: &mut IrFile, captures: Vec<ExprId>, inline_body: Option<ExprId>) -> ExprId {
        ir.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures,
            sam: None,
            inline_body,
        })
    }

    #[test]
    fn a_suspension_in_this_frame_is_not_one_the_ir_machine_misses() {
        let mut ir = IrFile::default();
        let point = suspension(&mut ir);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![point],
            value: None,
        });
        assert!(spliced_inline_suspensions(&ir, body, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_suspension_in_a_spliced_inline_body_is_found() {
        let mut ir = IrFile::default();
        let point = suspension(&mut ir);
        let lam = lambda(&mut ir, Vec::new(), Some(point));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![lam],
            value: None,
        });
        assert_eq!(
            spliced_inline_suspensions(&ir, body, &HashSet::new()),
            [point]
        );
        assert!(suspends_inside_a_spliced_inline_body(
            &ir,
            body,
            &HashSet::new()
        ));
    }

    /// A lambda with no `inline_body` is a real closure: its body belongs to a generated `invoke`
    /// and is not an expression child at all, so nothing about it reaches this frame. Its captures
    /// still do — and those the IR machine already sees.
    #[test]
    fn the_captures_of_a_lambda_belong_to_the_enclosing_frame() {
        for spliced in [false, true] {
            let mut ir = IrFile::default();
            let capture = suspension(&mut ir);
            let inner = suspension(&mut ir);
            let lam = lambda(&mut ir, vec![capture], spliced.then_some(inner));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![lam],
                value: None,
            });
            let found = spliced_inline_suspensions(&ir, body, &HashSet::new());
            assert!(
                !found.contains(&capture),
                "a capture is evaluated by this frame and is already visible to the IR machine"
            );
            assert_eq!(found.contains(&inner), spliced);
        }
    }

    #[test]
    fn nested_spliced_bodies_are_all_in_this_frame() {
        let mut ir = IrFile::default();
        let inner_point = suspension(&mut ir);
        let inner = lambda(&mut ir, Vec::new(), Some(inner_point));
        let outer = lambda(&mut ir, Vec::new(), Some(inner));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![outer],
            value: None,
        });
        assert_eq!(
            spliced_inline_suspensions(&ir, body, &HashSet::new()),
            [inner_point]
        );
    }
}
