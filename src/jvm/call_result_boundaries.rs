//! JVM result slots for checked generic calls.
//!
//! Common lowering retains the declaration result, the substituted result, and their semantic
//! coercion. Once JVM generic erasure has realized source function returns, this pass records the
//! physical type only when the value produced by the selected call differs from that coercion's JVM
//! target. Value-class representation remains owned by `jvm::value_classes`: a declaration that
//! returns `Tagged<A>` and a call read as `Tagged<Int>` have the same pre-value-class JVM shape, so
//! no physical override can hide the declaration identity from that pass. A bare `T` result, by
//! contrast, is already realized as its erased bound here and keeps that physical boundary.

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

fn terminal_call(exprs: &[IrExpr], mut expression: ExprId) -> Option<ExprId> {
    loop {
        match exprs.get(expression as usize)? {
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } => return Some(expression),
            IrExpr::Block {
                value: Some(value), ..
            } => expression = *value,
            _ => return None,
        }
    }
}

fn realized_call_result(ir: &IrFile, call: ExprId) -> Option<Ty> {
    let expression = ir.exprs.get(call as usize)?;
    let IrExpr::Call { callee, .. } = expression else {
        let IrExpr::MethodCall { class, index, .. } = expression else {
            return None;
        };
        let function = *ir
            .classes
            .get(*class as usize)?
            .methods
            .get(*index as usize)?;
        return ir
            .functions
            .get(function as usize)
            .map(|function| function.ret);
    };
    if let Some(function) = callee.source_function() {
        return ir
            .functions
            .get(function as usize)
            .map(|function| function.ret);
    }
    match callee {
        Callee::Intrinsic { ret, .. }
        | Callee::CrossFile { ret, .. }
        | Callee::Module { ret, .. }
        | Callee::ModuleWithDefaults { ret, .. }
        | Callee::External { ret, .. }
        | Callee::Super { ret, .. } => Some(*ret),
        Callee::Virtual {
            params: Some((_, ret)),
            ..
        } => Some(*ret),
        _ => None,
    }
}

pub(super) fn realize_call_result_boundaries(ir: &mut IrFile) {
    // Only the coercion common lowering placed between a declaration's result and its call-site
    // substitution is a result slot. Any other coercion over the same call (a reference adaptation
    // widening `Id` to `Any`, a nullability widening) converts an already-realized result.
    let mut coercions = ir
        .declaration_result_coercions
        .iter()
        .copied()
        .collect::<Vec<_>>();
    coercions.sort_unstable();
    let boundaries = coercions
        .into_iter()
        .filter_map(|coercion| match ir.exprs.get(coercion as usize)? {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => Some((coercion, *arg, *type_operand)),
            _ => None,
        })
        .filter_map(|(coercion, expression, target)| {
            // A retained inline body has already crossed and removed its declaration ABI; its
            // result slot is specialized to the call-site type.
            if ir.inline_regions.contains(&expression) {
                return None;
            }
            let call = terminal_call(&ir.exprs, expression)?;
            // This semantic declaration fact distinguishes a checked generic call from unrelated
            // coercions. It was recorded while the stable selected callable identity was live.
            ir.call_declared_ret.get(&call)?;
            let result = realized_call_result(ir, call)?;
            let physical = crate::jvm::ir_emit::ir_ty_to_jvm(&result);
            let jvm_target = crate::jvm::ir_emit::ir_ty_to_jvm(&target);
            (physical != jvm_target).then_some((coercion, expression, target, physical))
        })
        .collect::<Vec<_>>();

    for &(_, expression, _, physical) in &boundaries {
        ir.physical_types.insert(expression, physical);
    }
    fold_nullable_widenings(ir, &boundaries);
}

/// An erased reference slot read as a primitive and then widened to that primitive's nullable
/// type (`Object -> Int -> Int?`) is one reference conversion: realizing both would unbox a
/// possibly-null reference only to box it again. The widening is retargeted to the slot itself.
fn fold_nullable_widenings(ir: &mut IrFile, boundaries: &[(ExprId, ExprId, Ty, Ty)]) {
    let carriers = boundaries
        .iter()
        .filter(|(_, _, target, physical)| !target.is_reference() && physical.is_reference())
        .map(|&(coercion, expression, target, _)| (coercion, (expression, target)))
        .collect::<std::collections::HashMap<_, _>>();
    if carriers.is_empty() {
        return;
    }
    for expression in &mut ir.exprs {
        if let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = expression
        {
            if let Some(&(slot, primitive)) = carriers.get(arg) {
                if type_operand.nullable_primitive() == Some(primitive) {
                    *arg = slot;
                }
            }
        }
    }
}
