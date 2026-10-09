//! The value-index namespace of one function: the types of its parameters and locals, the next
//! free index, and the default a fresh local starts from.

use std::collections::HashMap;

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::Ty;

use super::CoroutineRepresentation;

/// The maximum value-index referenced anywhere in the arena (params, locals). New state-machine locals
/// are allocated above this so they never collide with an existing index in any function.
pub(crate) fn max_value_index(ir: &IrFile) -> u32 {
    let mut m = 0u32;
    for e in &ir.exprs {
        match e {
            IrExpr::GetValue(i) | IrExpr::GetStatic(i) => m = m.max(*i),
            IrExpr::SetValue { var, .. } => m = m.max(*var),
            IrExpr::Variable { index, .. } => m = m.max(*index),
            // A handler binds its exception to a value index of its own.
            IrExpr::Try { catches, .. } => {
                m = catches.iter().map(|catch| catch.var).fold(m, u32::max);
            }
            _ => {}
        }
    }
    m
}

pub(crate) fn function_value_types(ir: &IrFile, fid: u32, body: ExprId) -> HashMap<u32, Ty> {
    function_value_types_with(ir, fid, &ir.functions[fid as usize].params, body)
}

/// The value-type table of a lambda's `inline_body`, numbered as the impl method is: captures, then
/// the lambda's own parameters, then the locals the body declares. A `suspend`-typed lambda may
/// already have been through this pass — it precedes the frame it is spliced into in `suspend_funs`
/// — and then carries a trailing `Continuation` at the index its body's first local uses. The
/// declared signature is the one the body was numbered against.
pub(super) fn spliced_body_value_types(
    ir: &IrFile,
    impl_fn: u32,
    body: ExprId,
) -> HashMap<u32, Ty> {
    let params = ir
        .suspend_declared_sigs
        .get(&impl_fn)
        .map(|(params, _)| params.as_slice())
        .unwrap_or(&ir.functions[impl_fn as usize].params);
    function_value_types_with(ir, impl_fn, params, body)
}

pub(super) fn function_value_types_with(
    ir: &IrFile,
    fid: u32,
    params: &[Ty],
    body: ExprId,
) -> HashMap<u32, Ty> {
    fn collect(ir: &IrFile, expression: ExprId, out: &mut HashMap<u32, Ty>) {
        match &ir.exprs[expression as usize] {
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                out.entry(*index).or_insert(*ty);
                if let Some(init) = init {
                    collect(ir, *init, out);
                }
            }
            IrExpr::Lambda { captures, .. } => {
                for &capture in captures {
                    collect(ir, capture, out);
                }
            }
            _ => for_each_child(&ir.exprs, expression, &mut |child| collect(ir, child, out)),
        }
    }

    let function = &ir.functions[fid as usize];
    let physical_receiver = function.dispatch_receiver.filter(|_| !function.is_static);
    let receiver_offset = u32::from(physical_receiver.is_some());
    let mut out = HashMap::new();
    if let Some(receiver) = physical_receiver {
        out.insert(0, Ty::obj_name(receiver));
    }
    for (index, ty) in params.iter().copied().enumerate() {
        out.insert(receiver_offset + index as u32, ty);
    }
    collect(ir, body, &mut out);
    out
}

/// The default a fresh temporary of `ty` starts from. Every such temporary is assigned before it is
/// read; the target decides how the placeholder is represented.
pub(crate) fn zero_value(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    ty: &Ty,
) -> ExprId {
    ir.add_expr(IrExpr::Const(representation.zero(ty)))
}

/// A local of the bottom type (`var x = null` — `Ty::Null`) has exactly one possible value, so
/// kotlinc gives it no continuation field and rematerializes it in every resume arm. Keeping it out
/// of the spill layout also preserves its verifier `null` type instead of widening it to `Object`.
pub(crate) fn is_rematerialized_null(ty: &Ty) -> bool {
    matches!(ty.non_null(), Ty::Null)
}
