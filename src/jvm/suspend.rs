//! JVM coroutine (`suspend fun`) IR lowering pass — an **optional, JVM-only** IR→IR transform.
//!
//! `ir_lower` keeps a `suspend fun` as a plain function (its declared Kotlin signature) and records its
//! `FunId` in `ir.suspend_funs`, so the platform-agnostic IR stays neutral (a JS backend realizes
//! suspension differently). This pass realizes kotlinc's JVM continuation-passing-style (CPS) ABI:
//!
//!   * append a `kotlin.coroutines.Continuation` parameter (the caller's continuation);
//!   * erase the return type to `java.lang.Object` — the resume value, *boxed* (a primitive return is
//!     boxed; a reference return widens to `Object` for free).
//!
//! Slice 1 handles a **leaf** suspend function (no suspension point): the body is straight-line and
//! returns its value, exactly like kotlinc's `static Object foo(Continuation)` with no state machine.
//! ir_lower's suspend gate guarantees only leaf, top-level suspend functions reach a lowerable file, so
//! this pass never sees a suspension point yet. A later slice adds the state machine + `Foo$fn$1`
//! continuation class (built here) for functions that DO suspend.

use crate::ir::{ExprId, IrExpr, IrFile, IrType, IrTypeOp};

/// `java.lang.Object` — a suspend function's erased (boxed) return type.
fn object_ty() -> IrType {
    IrType::Class {
        fq_name: "kotlin/Any".to_string(),
        type_args: vec![],
        nullable: true,
    }
}

/// `kotlin.coroutines.Continuation` — the appended CPS parameter.
fn continuation_ty() -> IrType {
    IrType::Class {
        fq_name: "kotlin/coroutines/Continuation".to_string(),
        type_args: vec![],
        nullable: false,
    }
}

/// Rewrite every `suspend fun` in `ir` to the JVM CPS ABI. Returns `false` (skip the whole file, never
/// miscompile) if a body contains an IR shape this pass can't confidently transform — ir_lower's gate
/// already restricts suspend bodies to leaf forms, so that is a defensive backstop, not an expected path.
#[must_use]
pub fn lower_suspend(ir: &mut IrFile) -> bool {
    let fids = ir.suspend_funs.clone();
    for fid in fids {
        let f = &mut ir.functions[fid as usize];
        // CPS signature: append the continuation parameter (no null-check — kotlinc emits none on it)
        // and erase the return type to `Object`.
        f.params.push(continuation_ty());
        f.param_checks.push(None);
        f.ret = object_ty();
        // Box each returned value to `Object` so the `areturn` is type-correct (a primitive return now
        // needs boxing; a reference return widens for free via the coercion).
        if let Some(body) = f.body {
            if !box_returns(ir, body) {
                return false;
            }
        }
    }
    true
}

/// Wrap the value of every `Return` reachable from `e` in an `ImplicitCoercion` to `Object`. Recurses
/// through the control-flow nodes a leaf suspend body can contain; returns `false` on any other node so
/// the caller skips the file rather than leave a `Return` unboxed (which would emit an invalid `areturn`).
fn box_returns(ir: &mut IrFile, e: ExprId) -> bool {
    match ir.exprs[e as usize].clone() {
        IrExpr::Return(None) => true,
        IrExpr::Return(Some(v)) => {
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: v,
                type_operand: object_ty(),
            });
            ir.exprs[e as usize] = IrExpr::Return(Some(boxed));
            // A nested `return` (e.g. `return if (c) return a else b`) lives inside `v` — recurse so it
            // is boxed too.
            box_returns(ir, v)
        }
        IrExpr::Block { stmts, value } => {
            for s in stmts {
                if !box_returns(ir, s) {
                    return false;
                }
            }
            match value {
                Some(val) => box_returns(ir, val),
                None => true,
            }
        }
        IrExpr::When { branches } => {
            for (cond, body) in branches {
                if let Some(c) = cond {
                    if !box_returns(ir, c) {
                        return false;
                    }
                }
                if !box_returns(ir, body) {
                    return false;
                }
            }
            true
        }
        // Value/leaf nodes that cannot contain a `return` in a leaf suspend body.
        IrExpr::Const(_) | IrExpr::GetValue(_) | IrExpr::GetStatic(_) | IrExpr::UnitInstance => {
            true
        }
        // Single-operand value wrappers — recurse (cheap; they hold no `return` in practice).
        IrExpr::TypeOp { arg, .. } | IrExpr::NotNullAssert { operand: arg } => box_returns(ir, arg),
        IrExpr::Throw { operand } => box_returns(ir, operand),
        IrExpr::StringConcat(parts) => parts.into_iter().all(|p| box_returns(ir, p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => box_returns(ir, lhs) && box_returns(ir, rhs),
        // Any other shape shouldn't appear in a gated leaf body; bail rather than risk a missed return.
        _ => false,
    }
}
