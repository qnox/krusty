//! How kotlinc realizes a `suspend inline` function's body on the JVM.
//!
//! An inlining caller splices the body kotlinc's codegen wrote, suspension markers and all, and
//! its own coroutine transformation builds the machine over the result. A callable function keeps
//! that body in a private `name$$forInline` copy beside its own state machine. A function that is
//! only ever inlined (kotlinc's `isEffectivelyInlineOnly`) has no state machine: its one method is
//! that body.

use crate::ir::{ExprId, IrExpr, IrFile};
use crate::jvm::suspend::TransformedMachine;
use crate::types::InlineExpansionMode;

/// Whether `fid` is a `suspend inline` function only ever inlined: a reified one, whose body
/// needs its call site's types, or a private one. Its method keeps the marked body and no
/// continuation class is written for it.
pub(crate) fn keeps_marked_body(ir: &IrFile, fid: u32) -> bool {
    is_suspend_inline(ir, fid)
        && (crate::jvm::ir_emit::is_reifiable(ir, fid) || ir.method_visibility(fid).is_private())
}

/// Whether `fid` is a callable `suspend inline` function, written as its state machine with the
/// marked body in a private `name$$forInline` copy.
pub(crate) fn has_for_inline_copy(ir: &IrFile, fid: u32) -> bool {
    is_suspend_inline(ir, fid) && !keeps_marked_body(ir, fid)
}

fn is_suspend_inline(ir: &IrFile, fid: u32) -> bool {
    ir.inline_fns.contains(&fid) && ir.suspend_funs.contains(&fid)
}

/// The suspension points of `machine` a body kept for inlining leaves unmarked: an inline
/// function-typed parameter's invoke, which the inlining caller replaces with the lambda's body.
pub(crate) fn inlined_invokes(ir: &IrFile, fid: u32, machine: &TransformedMachine) -> Vec<ExprId> {
    // A member's value 0 is its receiver; the modes cover the parameters after it. They cover the
    // declared parameters only, not the trailing `$completion` the CPS signature added.
    let receiver = usize::from(!ir.functions[fid as usize].is_static);
    let modes = ir
        .fn_params
        .get(&fid)
        .map_or(&[][..], |info| info.expansion_modes.as_slice());
    let spliced = |call: ExprId| {
        let IrExpr::InvokeFunction { func, .. } = ir.expr(call) else {
            return false;
        };
        let IrExpr::GetValue(value) = ir.expr(*func) else {
            return false;
        };
        (*value as usize)
            .checked_sub(receiver)
            .and_then(|parameter| modes.get(parameter))
            == Some(&InlineExpansionMode::Splice)
    };
    machine
        .suspensions
        .iter()
        .map(|suspension| suspension.call)
        .filter(|&call| spliced(call))
        .collect()
}
