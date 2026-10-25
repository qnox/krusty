//! The catch bodies of a function that suspend, whose caught exception the state machine spills.

use crate::backend::coroutines::expr_calls_suspend;
use crate::fx_hash::HashSet;
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};

/// Collect every suspending catch body so the state machine can spill its caught exception across
/// that body's suspension (`r_v` no longer holds it once the catch resumes). Catch BODY identity—not
/// the lexically reused catch-variable slot—distinguishes sibling arms. Does not descend into lambdas,
/// whose bodies own separate state machines and value namespaces.
pub(super) fn find_suspending_catch_tries(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    out: &mut Vec<(u32, ExprId, crate::types::TypeName)>,
) {
    match &ir.exprs[e as usize] {
        IrExpr::Lambda { .. } => return,
        IrExpr::Try {
            catches, finally, ..
        } if finally.is_none() => {
            for catch in catches {
                if expr_calls_suspend(ir, catch.body, suspend_set)
                    && !catch_body_nests_suspending_catch(ir, catch.body, suspend_set)
                {
                    out.push((catch.var, catch.body, catch.jvm_class()));
                }
            }
        }
        _ => {}
    }
    for_each_child(&ir.exprs, e, &mut |c| {
        find_suspending_catch_tries(ir, c, suspend_set, out)
    });
}

/// Whether `e` (a catch body) is, or contains (excluding `Lambda` bodies), a `try/catch` whose own catch
/// body suspends — a nested suspending catch whose exception variable could alias the enclosing one.
fn catch_body_nests_suspending_catch(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::Lambda { .. } => return false,
        IrExpr::Try { catches, .. }
            if catches
                .iter()
                .any(|c| expr_calls_suspend(ir, c.body, suspend_set)) =>
        {
            return true;
        }
        _ => {}
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if !found && catch_body_nests_suspending_catch(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}
