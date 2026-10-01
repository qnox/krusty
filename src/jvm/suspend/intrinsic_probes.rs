//! The continuation an unintercepted intrinsic suspension point probes.
//!
//! kotlinc follows a `suspendCoroutineUninterceptedOrReturn` block with
//! `dup; getCOROUTINE_SUSPENDED; if_acmpne; <continuation>; probeCoroutineSuspended`, reporting the
//! suspension to the debug probes with the continuation the block was given. The CPS pass knows
//! which value holds that continuation once it binds the block's placeholder, so it records that
//! JVM value index in emission facts keyed by the semantic point.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile, IrIntrinsicSuspensionKind};

/// Bind every current-coroutine placeholder in `root` to the continuation value at `slot`: a read
/// of it, `coroutineContext` as its interface call, and the value an unintercepted block probes.
pub(super) fn bind_current_continuation(
    ir: &mut IrFile,
    root: ExprId,
    slot: u32,
    continuations: &mut super::IntrinsicProbeContinuations,
) {
    bind_probed_continuations(ir, root, slot, continuations);
    super::realize_coroutine_context(ir, root, IrExpr::GetValue(slot));
    super::rewrite_subtree(ir, root, &mut |node| {
        if matches!(node, IrExpr::CurrentContinuation) {
            *node = IrExpr::GetValue(slot);
        }
    });
}

/// Record `slot` as the continuation of every unintercepted intrinsic point reachable from `root`.
fn bind_probed_continuations(
    ir: &IrFile,
    root: ExprId,
    slot: u32,
    continuations: &mut super::IntrinsicProbeContinuations,
) {
    let mut seen = HashSet::new();
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if ir
            .intrinsic_suspension_points
            .get(&expression)
            .is_some_and(|point| point.kind == IrIntrinsicSuspensionKind::Unintercepted)
        {
            continuations.insert(expression, slot);
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}
