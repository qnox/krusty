//! Which expressions are coroutine suspension points, and what a suspension yields.
//!
//! A suspension point is a direct call to a suspend function (same-file through its `FunId`,
//! cross-unit through `IrFile::suspend_calls`) or an intrinsic block recorded in
//! `IrFile::intrinsic_suspension_points`. Lambda bodies are never part of the enclosing body's
//! suspension analysis: they run in their own function.

use std::collections::HashSet;

use crate::ir::{for_each_child, Callee, ExprId, IrExpr, IrFile};
use crate::types::Ty;

use super::bottom_completion::unwrap_suspend_cast;
use super::value_when::value_when;

/// For a same-file suspend call, the callee `FunId` — used to recover the callee's LOGICAL return type
/// (its index into `orig_rets`). Handles a static call (`Call{Local}`), the same function's `$default`
/// edge after omitted arguments are realized (`LocalDefault` / `ClassStaticDefault`), and a same-file
/// member call (`MethodCall`, whose `FunId` is the class's method at `index`). The default edge is
/// still that suspend declaration: its continuation belongs before the mask and marker, and dropping
/// it here leaves the call with the pre-CPS arity. Returns `None` for a cross-unit suspend call (a
/// `Callee::Static` to another file / the classpath) — that call has no local `FunId`; its logical
/// type comes from `ir.suspend_calls` instead.
pub(crate) fn suspend_call_fid(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> Option<u32> {
    match &ir.exprs[e as usize] {
        IrExpr::Call {
            callee: Callee::Local(fid) | Callee::LocalDefault(fid),
            ..
        } if suspend_set.contains(fid) => Some(*fid),
        IrExpr::Call {
            callee:
                Callee::ClassStatic { function, .. } | Callee::ClassStaticDefault { function, .. },
            ..
        } if suspend_set.contains(function) => Some(*function),
        IrExpr::MethodCall { class, index, .. } => {
            let fid = *ir.classes[*class as usize].methods.get(*index as usize)?;
            suspend_set.contains(&fid).then_some(fid)
        }
        _ => None,
    }
}

/// Whether evaluating `e` is one atomic SUSPENSION POINT: either a direct suspend-function call
/// (same-file via [`suspend_call_fid`], or cross-unit via `ir.suspend_calls`) or an inlined intrinsic
/// expression recorded in `ir.intrinsic_suspension_points`. The latter already contains its
/// continuation behavior, so [`append_continuation`] deliberately leaves its non-call node unchanged.
pub(crate) fn is_suspension_point(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    suspend_call_fid(ir, e, suspend_set).is_some()
        || ir.suspend_calls.contains_key(&e)
        || ir.intrinsic_suspension_points.contains_key(&e)
}

/// The logical source result of a non-local call or intrinsic suspension point. Same-file calls use
/// `orig_rets` through [`suspend_call_fid`]; the two side maps cover only nodes whose result cannot be
/// recovered from a local `FunId`.
pub(crate) fn recorded_suspension_result(ir: &IrFile, e: ExprId) -> Option<Ty> {
    ir.suspend_calls.get(&e).copied().or_else(|| {
        ir.intrinsic_suspension_points
            .get(&e)
            .map(|point| point.result)
    })
}

pub(crate) fn value_class_suspension_result(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<crate::ir::IrValueClassSuspendResult> {
    suspend_call_fid(ir, e, suspend_set)
        .and_then(|fid| ir.value_class_suspend_returns.get(&fid).copied())
        .or_else(|| ir.value_class_suspend_calls.get(&e).copied())
}

pub(super) fn when_has_non_direct_suspending_branch(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) -> bool {
    let Some(when) = value_when(ir, expression) else {
        return false;
    };
    let IrExpr::When { branches } = &ir.exprs[when as usize] else {
        return false;
    };
    branches.iter().any(|(_, branch)| {
        expr_calls_suspend(ir, *branch, suspend_set)
            && !is_suspension_point(
                ir,
                unwrap_suspend_cast(ir, *branch, suspend_set, false).point,
                suspend_set,
            )
    })
}

/// Whether `e`'s subtree contains any call to a suspend function (used to reject shapes this pass can't
/// restructure — a suspend call nested in an expression, a branch, a loop, etc.).
pub(crate) fn expr_calls_suspend(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if is_suspension_point(ir, e, suspend_set) {
        return true;
    }
    // Constructing a lambda evaluates its captures, but its body belongs to the generated invoke
    // method and cannot suspend the enclosing body. `for_each_child` deliberately exposes the
    // `inline_body` for generic IR transforms, so suspension analysis must enforce this semantic
    // ownership boundary itself.
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        return captures
            .iter()
            .any(|&capture| expr_calls_suspend(ir, capture, suspend_set));
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if expr_calls_suspend(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

/// Hoist each suspension call that sits at an *unconditional* position inside a top-level statement's
/// expression (e.g. `val a = foo() + 2`, `sum = sum + foo()`) into a preceding `val tmp = foo()`, so the
/// flattener only meets a suspension as a bound-local / bare statement (the positions it models). A
/// suspension inside a conditional sub-expression (an `if`/`when`/elvis/loop) is left in place — those
/// are handled structurally by the flattener (or skip the file if not yet modeled). Order of hoisted
/// temps follows left-to-right evaluation.
/// Whether `e` is an `if`/`when` EXPRESSION at least one of whose CONDITIONS calls a suspension — the
/// pure guard for the arms that route to [`hoist_when_cond_suspensions`].
pub(super) fn when_cond_suspends(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    matches!(&ir.exprs[e as usize], IrExpr::When { branches }
        if branches.iter().any(|(c, _)| c.is_some_and(|c| expr_calls_suspend(ir, c, suspend_set))))
}

/// The number of suspend-call nodes in the subtree under `e`.
pub(crate) fn count_suspensions(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> usize {
    let mut n = usize::from(is_suspension_point(ir, e, suspend_set));
    for_each_child(&ir.exprs, e, &mut |c| {
        n += count_suspensions(ir, c, suspend_set);
    });
    n
}
