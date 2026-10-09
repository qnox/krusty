//! A `try` with a `finally` the state machine can split.
//!
//! The machine splits `try`/`catch` by handler regions; a `finally` is not a handler but cleanup
//! that runs on every exit. These rewrites express it with the constructs the machine already
//! splits: catch-and-remember the exception, run the cleanup, rethrow; and route each `return`
//! through the cleanup by storing the value and leaving a one-shot loop.

use std::collections::HashSet;

use crate::ir::{for_each_child, ExprId, IrBinOp, IrConst, IrExpr, IrFile};
use crate::types::Ty;

use super::control_flow::{expr_contains_owned_loop_jump, expr_has_return};
use super::suspension_points::expr_calls_suspend;
use super::value_namespace::{max_value_index, zero_value};
use super::CoroutineRepresentation;

/// Which `try`/`finally` regions [`linearize_suspending_finally`] rewrites.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinallySuspension {
    /// Only one whose cleanup suspends: the target's machine splits a protected body itself.
    InCleanup,
    /// Any one with a suspension in its body or its cleanup.
    Anywhere,
}

impl FinallySuspension {
    fn region(self, whole: ExprId, cleanup: ExprId) -> ExprId {
        match self {
            Self::InCleanup => cleanup,
            Self::Anywhere => whole,
        }
    }
}

/// Canonicalize `try { body } catch { arms } finally { cleanup }` as an inner `try/catch` wrapped by
/// an outer `try/finally`. This is Kotlin's exact control-flow composition: the cleanup observes normal
/// completion of either the body or a selected catch, and also every exception escaping the inner
/// region. Keeping one handler responsibility per `IrExpr::Try` lets the state-machine handler stack
/// compose nested regions instead of growing a second catch-plus-finally implementation.
pub(crate) fn separate_catches_from_finally(ir: &mut IrFile, expression: ExprId) {
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        separate_catches_from_finally(ir, child);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if catches.is_empty() {
        return;
    }
    let inner = ir.add_expr(IrExpr::Try {
        body,
        catches,
        finally: None,
        result,
    });
    let outer_body = if result == Ty::Unit {
        ir.add_expr(IrExpr::Block {
            stmts: vec![inner],
            value: None,
        })
    } else {
        ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(inner),
        })
    };
    ir.exprs[expression as usize] = IrExpr::Try {
        body: outer_body,
        catches: Vec::new(),
        finally: Some(finally),
        result,
    };
}

/// Make a suspending `finally` an ordinary state-machine sequence while preserving the exceptional
/// path. Value-producing tries have already been bound to storage before this runs, and combined
/// catch/finally regions have already been split, so the remaining node is statement-shaped:
///
/// ```text
/// var pending: Throwable? = null
/// try { body } catch (e: Throwable) { pending = e }
/// finallyBody
/// if (pending != null) throw pending
/// ```
///
/// `pending` is an ordinary compiler temp. The normal liveness pass sees its read after the finally
/// suspension and allocates the continuation spill; no backend-only field identity is invented here.
pub(crate) fn linearize_suspending_finally(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    scope: FinallySuspension,
) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        linearize_suspending_finally(ir, representation, child, suspend_set, scope);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if !catches.is_empty()
        || result != Ty::Unit
        || !expr_calls_suspend(ir, scope.region(expression, finally), suspend_set)
        || expr_has_return(ir, body)
        || expr_contains_owned_loop_jump(ir, body)
    {
        return;
    }

    let pending = max_value_index(ir) + 1;
    let pending_ty = Ty::nullable(Ty::obj_name(representation.throwable()));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let declaration = ir.add_expr(IrExpr::Variable {
        index: pending,
        ty: pending_ty,
        init: Some(null),
        named: false,
    });
    let catch_var = pending + 1;
    let caught = ir.add_expr(IrExpr::GetValue(catch_var));
    let remember = ir.add_expr(IrExpr::SetValue {
        var: pending,
        value: caught,
    });
    let catch_body = ir.add_expr(IrExpr::Block {
        stmts: vec![remember],
        value: None,
    });
    let protected = ir.add_expr(IrExpr::Try {
        body,
        catches: vec![crate::ir::IrCatch::generated(
            catch_var,
            representation.throwable(),
            catch_body,
        )],
        finally: None,
        result: Ty::Unit,
    });
    let pending_read = ir.add_expr(IrExpr::GetValue(pending));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let has_exception = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Ne,
        lhs: pending_read,
        rhs: null,
    });
    let exception = ir.add_expr(IrExpr::GetValue(pending));
    let throw = ir.add_expr(IrExpr::Throw { operand: exception });
    let throw_block = ir.add_expr(IrExpr::Block {
        stmts: vec![throw],
        value: None,
    });
    let no_exception = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let rethrow = ir.add_expr(IrExpr::When {
        branches: vec![(Some(has_exception), throw_block), (None, no_exception)],
    });
    ir.exprs[expression as usize] = IrExpr::Block {
        stmts: vec![declaration, protected, finally, rethrow],
        value: None,
    };
}

/// Route function returns through each enclosing `finally`. The return expression is evaluated once
/// into a typed pending-value local; a labeled break leaves a one-shot loop around the protected body;
/// after cleanup, the completion-kind dispatch performs the actual return. An exception or a return
/// from the finally itself naturally bypasses/overrides that pending completion.
pub(crate) fn linearize_finally_returns(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    expression: ExprId,
    return_ty: &Ty,
) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        linearize_finally_returns(ir, representation, child, return_ty);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if result != Ty::Unit || !expr_has_return(ir, body) {
        return;
    }

    let completion = max_value_index(ir) + 1;
    let pending_value = completion + 1;
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let completion_decl = ir.add_expr(IrExpr::Variable {
        index: completion,
        ty: Ty::Int,
        init: Some(zero),
        named: false,
    });
    let stored_return_ty = if *return_ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        *return_ty
    };
    let default_return = zero_value(ir, representation, &stored_return_ty);
    let return_decl = ir.add_expr(IrExpr::Variable {
        index: pending_value,
        ty: stored_return_ty,
        init: Some(default_return),
        named: false,
    });
    let label = format!("$finally$return${expression}");
    rewrite_returns_to_pending(
        ir,
        body,
        &label,
        completion,
        pending_value,
        *return_ty == Ty::Unit,
    );
    let leave = ir.add_expr(IrExpr::Break {
        label: Some(label.clone()),
    });
    let loop_body = ir.add_expr(IrExpr::Block {
        stmts: vec![body, leave],
        value: None,
    });
    let always = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let protected_loop = ir.add_expr(IrExpr::While {
        cond: always,
        body: loop_body,
        update: None,
        post_test: false,
        label: Some(label),
    });
    let protected_body = ir.add_expr(IrExpr::Block {
        stmts: vec![protected_loop],
        value: None,
    });
    let protected = ir.add_expr(IrExpr::Try {
        body: protected_body,
        catches,
        finally: Some(finally),
        result: Ty::Unit,
    });
    let completion_read = ir.add_expr(IrExpr::GetValue(completion));
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let has_return = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Ne,
        lhs: completion_read,
        rhs: zero,
    });
    let return_value = ir.add_expr(IrExpr::GetValue(pending_value));
    let perform_return = ir.add_expr(IrExpr::Return(Some(return_value)));
    let return_block = ir.add_expr(IrExpr::Block {
        stmts: vec![perform_return],
        value: None,
    });
    let fallthrough = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let dispatch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(has_return), return_block), (None, fallthrough)],
    });
    ir.exprs[expression as usize] = IrExpr::Block {
        stmts: vec![completion_decl, return_decl, protected, dispatch],
        value: None,
    };
}

fn rewrite_returns_to_pending(
    ir: &mut IrFile,
    expression: ExprId,
    label: &str,
    completion: u32,
    pending_value: u32,
    unit_return: bool,
) {
    match ir.exprs[expression as usize].clone() {
        IrExpr::Lambda { .. } => return,
        IrExpr::Return(value) => {
            // A checked `Unit` expression may have a physical void coercion: it must execute for
            // effect, but it cannot be used as the operand of the pending-value store. Materialize
            // the semantic singleton after evaluation, exactly as an ordinary JVM `Unit` return.
            let effect = unit_return.then_some(value).flatten();
            let value = if unit_return {
                ir.add_expr(IrExpr::UnitInstance)
            } else {
                value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance))
            };
            let store_value = ir.add_expr(IrExpr::SetValue {
                var: pending_value,
                value,
            });
            let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            let mark_return = ir.add_expr(IrExpr::SetValue {
                var: completion,
                value: one,
            });
            let leave = ir.add_expr(IrExpr::Break {
                label: Some(label.to_string()),
            });
            let stmts = effect
                .into_iter()
                .chain([store_value, mark_return, leave])
                .collect();
            ir.exprs[expression as usize] = IrExpr::Block { stmts, value: None };
        }
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
            for child in children {
                rewrite_returns_to_pending(
                    ir,
                    child,
                    label,
                    completion,
                    pending_value,
                    unit_return,
                );
            }
        }
    }
}
