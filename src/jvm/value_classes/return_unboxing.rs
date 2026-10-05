//! Unboxing a value-class result at a function's return. A function declared to return `X`, or an
//! `X?` whose carrier holds null itself, returns the carrier; a tail that yields the box (the `!!`
//! of a safe call, a function value's `invoke` through the `FunctionN` `Object` slot) is unboxed.

use super::{unbox_wrap, unbox_wrap_nullable, ReprInputs};
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::TypeName;

/// Values returned non-locally from an inline lambda into the function that owns `body`.
///
/// The call that receives an inline lambda normally carries the enclosing function's return
/// representation adapter. A non-local `return` exits before that call produces a value, so its
/// own value must cross the same boundary. Checked return depth is the authoritative ownership
/// fact; walking retained `inline_body` edges follows only templates that execute in this frame.
pub(super) fn non_local_inline_return_values(ir: &IrFile, body: ExprId) -> Vec<ExprId> {
    let mut values = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![(body, false)];
    while let Some((expression, in_inline_body)) = pending.pop() {
        if !seen.insert((expression, in_inline_body)) {
            continue;
        }
        if in_inline_body && ir.checked_return_depths.get(&expression) == Some(&0) {
            if let IrExpr::Return(Some(value)) = ir.expr(expression) {
                values.push(*value);
            }
        }
        match ir.expr(expression) {
            IrExpr::Lambda {
                captures,
                inline_body,
                ..
            } => {
                pending.extend(captures.iter().map(|&capture| (capture, in_inline_body)));
                pending.extend(inline_body.map(|body| (body, true)));
            }
            _ => crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                pending.push((child, in_inline_body))
            }),
        }
    }
    values
}

/// At a function's return tail (recursing `return`/block tails), `unbox-impl` a BOXED `x` so it
/// matches the erased carrier return: `fun f(): Z = a?.foo()!!` returns the box. `null_slot` is set
/// for an `X?` return, whose box may be null: it is unboxed through that temporary behind a null
/// check, as kotlinc's safe `unbox-impl` call (`dup; ifnull`) after temporary elimination.
pub(super) fn unbox_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    inputs: ReprInputs<'_>,
    null_slot: Option<u32>,
) {
    let boxed = ir.physical_types.get(&id).is_some_and(|ty| {
        ty.non_null()
            .obj_internal()
            .is_some_and(|classifier| classifier == x)
    });
    if !boxed {
        match &ir.exprs[id as usize] {
            IrExpr::Return(Some(v)) | IrExpr::Block { value: Some(v), .. } => {
                let v = *v;
                return unbox_tail(ir, v, x, inputs, null_slot);
            }
            IrExpr::Block { value: None, stmts } => {
                if let Some(&last) = stmts.last() {
                    unbox_tail(ir, last, x, inputs, null_slot);
                }
                return;
            }
            _ if !inputs.over(ir).is_boxed_vc(id, x) => return,
            _ => {}
        }
    }
    match null_slot {
        Some(slot) => unbox_wrap_nullable(ir, id, x, inputs.under, slot),
        None => unbox_wrap(ir, id, x, inputs.under),
    }
}
