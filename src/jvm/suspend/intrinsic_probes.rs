//! The continuation an unintercepted intrinsic suspension point probes.
//!
//! kotlinc follows a `suspendCoroutineUninterceptedOrReturn` block with
//! `dup; getCOROUTINE_SUSPENDED; if_acmpne; <continuation>; probeCoroutineSuspended`, reporting the
//! suspension to the debug probes with the continuation the block was given. The CPS pass knows
//! which value holds that continuation once it binds the block's placeholder, so it records that
//! JVM value index in emission facts keyed by the semantic point.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};

/// Bind every current-coroutine placeholder in `root` to the continuation value at `slot`: a read
/// of it, `coroutineContext` as its interface call, and the value an unintercepted block probes.
pub(super) fn bind_current_continuation(
    ir: &mut IrFile,
    root: ExprId,
    slot: u32,
    continuations: &mut super::IntrinsicProbeContinuations,
) {
    bind_probed_continuations(
        ir,
        root,
        super::ProbedContinuation::Value(slot),
        continuations,
    );
    super::realize_coroutine_context(ir, root, IrExpr::GetValue(slot));
    super::rewrite_subtree(ir, root, &mut |node| {
        if matches!(node, IrExpr::CurrentContinuation) {
            *node = IrExpr::GetValue(slot);
        }
    });
}

/// Bind every unintercepted intrinsic point in `root`, a body whose machine emission owns, to that
/// machine's continuation. The block reads it as the same `CurrentContinuation` emission resolves.
pub(super) fn bind_machine_continuation(
    ir: &IrFile,
    root: ExprId,
    continuations: &mut super::IntrinsicProbeContinuations,
) {
    bind_probed_continuations(ir, root, super::ProbedContinuation::Machine, continuations);
}

/// Record `continuation` as the one every unintercepted intrinsic point reachable from `root`
/// probes.
fn bind_probed_continuations(
    ir: &IrFile,
    root: ExprId,
    continuation: super::ProbedContinuation,
    continuations: &mut super::IntrinsicProbeContinuations,
) {
    let mut seen = HashSet::new();
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if ir.is_unintercepted_suspension(expression) {
            continuations.insert(expression, continuation);
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}
