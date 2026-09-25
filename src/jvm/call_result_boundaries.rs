//! Erased JVM result slots for checked generic calls.
//!
//! Common lowering marks the exact coercion between a selected declaration result and its
//! substituted semantic result. This JVM boundary realizes only those marked coercions: other
//! coercions over the same call already consume a realized result. Value-class lowering later
//! refines marked slots after its carrier inventory exists.

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

pub(super) fn terminal_call(exprs: &[IrExpr], mut expression: ExprId) -> Option<ExprId> {
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

/// The declaration's selected JVM erasure, retaining whether its erased reference slot admits
/// null. Later boundary passes consume that representation fact even though nullability does not
/// change the descriptor.
pub(super) fn erased_result_slot(ir: &IrFile, call: ExprId, declared: Ty) -> Option<Ty> {
    let erased = realized_call_result(ir, call)?;
    let nullable = declared.is_nullable()
        || matches!(declared.non_null(), Ty::TyParam(_, bound) if bound.is_nullable());
    Some(if nullable {
        Ty::nullable(erased.non_null())
    } else {
        erased
    })
}

pub(super) fn realize_call_result_boundaries(ir: &mut IrFile) {
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
            if ir.inline_regions.contains(&expression) {
                return None;
            }
            let call = terminal_call(&ir.exprs, expression)?;
            let declared = *ir.call_declared_ret.get(&call)?;
            let physical = erased_result_slot(ir, call, declared)?;
            let physical_jvm = crate::jvm::ir_emit::ir_ty_to_jvm(&physical);
            let target_jvm = crate::jvm::ir_emit::ir_ty_to_jvm(&target);
            (physical_jvm != target_jvm).then_some((coercion, expression, target, physical))
        })
        .collect::<Vec<_>>();

    for &(_, expression, _, physical) in &boundaries {
        ir.physical_types.insert(expression, physical);
    }
    fold_nullable_widenings(ir, &boundaries);
}

/// An erased reference slot read as a primitive and then widened to that primitive's nullable type
/// is one reference conversion. Retarget the widening to the slot so a null is not unboxed and
/// immediately reboxed.
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
