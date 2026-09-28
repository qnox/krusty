//! Kotlin's safe `suspendCoroutine` protocol, realized around the block common lowering keeps.

use super::{max_value_index, rewrite_subtree};
use crate::ir::{Callee, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty};

/// Realize Kotlin's safe `suspendCoroutine` protocol around each already-inlined user block.
///
/// Common lowering preserves the selected primitive and its checked block as one semantic suspension
/// point. The JVM realization uses the stdlib's actual protocol: intercept the current machine
/// continuation, wrap it in `SafeContinuation`, invoke the block once with that wrapper, then read
/// `getOrThrow()`. An immediate resume therefore produces the value synchronously; an asynchronous
/// resume first returns `COROUTINE_SUSPENDED` and later re-enters the enclosing machine. No callable
/// lookup or inline-body recovery happens here—the frontend supplied both the exact intrinsic kind and
/// the already-spliced block.
pub(super) fn realize_safe_coroutine_points(ir: &mut IrFile) {
    let points = ir
        .intrinsic_suspension_points
        .iter()
        .filter_map(|(&expression, point)| {
            (point.kind == crate::ir::IrIntrinsicSuspensionKind::Safe).then_some(expression)
        })
        .collect::<Vec<_>>();
    for expression in points {
        let safe_slot = max_value_index(ir).saturating_add(1);
        let block = ir.add_expr(ir.exprs[expression as usize].clone());
        rewrite_subtree(ir, block, &mut |node| {
            if matches!(node, IrExpr::CurrentContinuation) {
                *node = IrExpr::GetValue(safe_slot);
            }
        });

        let current = ir.add_expr(IrExpr::CurrentContinuation);
        let intercepted = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
                name: "intercepted".to_string(),
                descriptor: "(Lkotlin/coroutines/Continuation;)Lkotlin/coroutines/Continuation;"
                    .to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![current],
        });
        let safe_ty = Ty::obj("kotlin/coroutines/SafeContinuation");
        let safe = ir.add_expr(IrExpr::New {
            internal: type_name("kotlin/coroutines/SafeContinuation"),
            args: vec![intercepted],
            ctor_params: None,
            ctor_desc: Some("(Lkotlin/coroutines/Continuation;)V".to_string()),
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let declare_safe = ir.add_expr(IrExpr::Variable {
            index: safe_slot,
            ty: safe_ty,
            init: Some(safe),
            named: false,
        });
        let safe_for_result = ir.add_expr(IrExpr::GetValue(safe_slot));
        let result = ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: type_name("kotlin/coroutines/SafeContinuation"),
                name: "getOrThrow".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                params: None,
                interface: false,
                module_target: None,
            },
            dispatch_receiver: Some(safe_for_result),
            args: Vec::new(),
        });
        ir.exprs[expression as usize] = IrExpr::Block {
            stmts: vec![declare_safe, block],
            value: Some(result),
        };
        crate::trace_compiler!(
            "suspend",
            "realize safe coroutine point expression={expression} safe_slot={safe_slot}"
        );
    }
}
