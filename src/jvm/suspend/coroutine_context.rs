//! The checked `coroutineContext` intrinsic, realized on the continuation that carries it.

use crate::ir::{for_each_child, Callee, ExprId, IrExpr, IrFile};
use crate::types::type_name;

/// Realize every checked `coroutineContext` intrinsic in `e` as `getContext()` on `continuation`.
///
/// kotlinc inlines the `coroutineContext` getter in place, so the realization ends like an inlined
/// call: the line in effect is written again at the next instruction that marks it.
pub(super) fn realize_coroutine_context(ir: &mut IrFile, e: ExprId, continuation: IrExpr) {
    let mut reads = Vec::new();
    collect_coroutine_context_reads(&ir.exprs, e, &mut reads);
    if reads.is_empty() {
        return;
    }
    let context_receiver = ir.add_expr(continuation);
    for read in reads {
        let IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } = &ir.exprs[read as usize]
        else {
            unreachable!("collected coroutine-context call")
        };
        debug_assert!(dispatch_receiver.is_none());
        debug_assert!(args.is_empty());
        ir.exprs[read as usize] = IrExpr::Call {
            callee: Callee::realized_virtual(
                type_name("kotlin/coroutines/Continuation"),
                "getContext".to_string(),
                "()Lkotlin/coroutines/CoroutineContext;".to_string(),
                None,
                true,
            ),
            dispatch_receiver: Some(context_receiver),
            args: Vec::new(),
        };
        if let Some(
            crate::ir::IrNodeOrigin::Fir(cause) | crate::ir::IrNodeOrigin::Synthetic { cause, .. },
        ) = ir.fir_origins.get(&read).copied()
        {
            ir.fir_origins.insert(
                read,
                crate::ir::IrNodeOrigin::Synthetic {
                    cause,
                    kind: crate::fir::SyntheticOriginKind::InlinedCall,
                },
            );
        }
    }
}

fn collect_coroutine_context_reads(exprs: &[IrExpr], e: ExprId, reads: &mut Vec<ExprId>) {
    if let IrExpr::Call {
        callee:
            Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::CoroutineContext,
                ..
            },
        ..
    } = exprs[e as usize]
    {
        reads.push(e);
    }
    for_each_child(exprs, e, &mut |child| {
        collect_coroutine_context_reads(exprs, child, reads)
    });
}
