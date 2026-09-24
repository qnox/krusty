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
    let boundaries = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => Some((*arg, *type_operand)),
            _ => None,
        })
        .filter_map(|(expression, target)| {
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
            let target = crate::jvm::ir_emit::ir_ty_to_jvm(&target);
            (physical != target).then_some((expression, physical))
        })
        .collect::<Vec<_>>();

    for (expression, physical) in boundaries {
        ir.physical_types.insert(expression, physical);
    }
}
