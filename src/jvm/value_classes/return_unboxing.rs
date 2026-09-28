//! Unboxing a value-class result at a function's return. A function declared to return `X`, or an
//! `X?` whose carrier holds null itself, returns the carrier; a tail that yields the box (the `!!`
//! of a safe call, a function value's `invoke` through the `FunctionN` `Object` slot) is unboxed.

use super::{unbox_wrap, unbox_wrap_nullable, ReprInputs};
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::TypeName;

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
