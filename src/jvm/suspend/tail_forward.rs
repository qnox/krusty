//! Detection and body rewriting for a suspend function that directly forwards its continuation.

use super::bottom_completion::unwrap_suspend_cast;
use super::{
    expr_calls_suspend, is_suspension_point, recorded_suspension_result, suspend_call_fid,
};
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;
use std::collections::HashSet;

/// Whether exactly one suspension point is reachable from `expression`.
///
/// The arena can share or deeply nest nodes, so this is iterative and visits each expression once.
fn exactly_one_suspension_point(
    ir: &IrFile,
    expression: ExprId,
    suspend_functions: &HashSet<u32>,
) -> bool {
    let mut seen = HashSet::new();
    let mut pending = vec![expression];
    let mut count = 0usize;
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if is_suspension_point(ir, current, suspend_functions) {
            count += 1;
            if count > 1 {
                return false;
            }
        }
        for_each_child(&ir.exprs, current, &mut |child| pending.push(child));
    }
    count == 1
}

/// The suspend calls a body forwards its own continuation to instead of building a state machine.
pub(super) enum TailForward {
    /// The one suspension point, in tail position; rewritten by [`make_forward_body`].
    Single(ExprId),
    /// Every suspension point of a value-returning body, each returned; rewritten by
    /// [`forward_returned_tail_calls`].
    Returned(Vec<ExprId>),
}

impl TailForward {
    pub(super) fn calls(&self) -> &[ExprId] {
        match self {
            TailForward::Single(call) => std::slice::from_ref(call),
            TailForward::Returned(calls) => calls,
        }
    }
}

/// The calls kotlinc forwards `$completion` to, when it compiles the body without a state machine:
/// the single tail call [`tail_forward_call`] finds, or otherwise every suspension point of a
/// value-returning body when each one is returned ([`all_returned_tail_calls`]).
pub(super) fn tail_forward(
    ir: &IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    original_returns: &[Ty],
) -> Option<TailForward> {
    if let Some(call) = tail_forward_call(
        ir,
        body,
        suspend_functions,
        declared_return,
        original_returns,
    ) {
        return Some(TailForward::Single(call));
    }
    all_returned_tail_calls(ir, body, suspend_functions, declared_return).map(TailForward::Returned)
}

/// Rewrite the body so each forwarded call's CPS `Object` is what the function returns.
pub(super) fn rewrite_forward_body(
    ir: &mut IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    forward: &TailForward,
) {
    match forward {
        TailForward::Single(call) => make_forward_body(ir, body, *call),
        TailForward::Returned(calls) => {
            forward_returned_tail_calls(ir, body, suspend_functions, declared_return, calls)
        }
    }
}

/// A value class or unsigned result: its carrier, not the declared type, travels in the `Object`.
fn uses_carrier(ir: &IrFile, ty: Ty) -> bool {
    let ty = ty.non_null();
    ty.is_unsigned()
        || ty.obj_internal().is_some_and(|name| {
            ir.classes
                .iter()
                .any(|class| class.is_value && class.fq_name == name)
                || ir.has_external_value_class_name(name)
        })
}

/// If the body contains one suspension directly in tail position, select the physical call whose
/// result can be returned with this function's continuation and no local state machine.
fn tail_forward_call(
    ir: &IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    original_returns: &[Ty],
) -> Option<ExprId> {
    let unit_return = declared_return == Ty::Unit;
    if !exactly_one_suspension_point(ir, body, suspend_functions) {
        return None;
    }
    let tail = tail_expression(ir, body, suspend_functions, unit_return, original_returns)?;

    // A dependency call's CPS `Object` result is coerced to its declared type. When that is this
    // function's own result, the physical value can be forwarded verbatim. Value-class and unsigned
    // results keep the coercion because their carrier, not their declared type, crosses the ABI.
    let tail = match ir.exprs[tail as usize] {
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if type_operand == declared_return && !uses_carrier(ir, type_operand) => arg,
        _ => tail,
    };
    // A generic suspend call can carry a redundant reference cast around its erased result.
    let tail = unwrap_suspend_cast(ir, tail, suspend_functions, true).point;
    is_suspension_point(ir, tail, suspend_functions).then_some(tail)
}

fn tail_expression(
    ir: &IrFile,
    expression: ExprId,
    suspend_functions: &HashSet<u32>,
    unit_return: bool,
    original_returns: &[Ty],
) -> Option<ExprId> {
    let tail = match &ir.exprs[expression as usize] {
        IrExpr::Return(Some(expression)) => *expression,
        IrExpr::Block {
            value: Some(value), ..
        } if matches!(ir.exprs[*value as usize], IrExpr::Block { .. }) => {
            return tail_expression(ir, *value, suspend_functions, unit_return, original_returns);
        }
        IrExpr::Block {
            value: Some(value), ..
        } => *value,
        IrExpr::Block { value: None, stmts } => match stmts.last() {
            Some(&last) => match ir.exprs[last as usize] {
                IrExpr::Return(Some(expression)) => expression,
                // `= unitCall(…)` lowers to the call statement followed by a bare return.
                IrExpr::Return(None) if unit_return && stmts.len() >= 2 => {
                    let statement = stmts[stmts.len() - 2];
                    let point = match ir.exprs[statement as usize] {
                        IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg,
                            type_operand: Ty::Unit,
                        } => arg,
                        _ => statement,
                    };
                    if is_suspension_point(ir, point, suspend_functions)
                        && suspension_returns_unit(ir, point, suspend_functions, original_returns)
                    {
                        point
                    } else {
                        return None;
                    }
                }
                _ if unit_return
                    && is_suspension_point(ir, last, suspend_functions)
                    && suspension_returns_unit(ir, last, suspend_functions, original_returns) =>
                {
                    last
                }
                // Source grouping can leave statement-only blocks nested at the tail.
                _ if matches!(ir.exprs[last as usize], IrExpr::Block { .. }) => {
                    return tail_expression(
                        ir,
                        last,
                        suspend_functions,
                        unit_return,
                        original_returns,
                    );
                }
                _ => return None,
            },
            None => return None,
        },
        _ => return None,
    };
    Some(tail)
}

fn suspension_returns_unit(
    ir: &IrFile,
    expression: ExprId,
    suspend_functions: &HashSet<u32>,
    original_returns: &[Ty],
) -> bool {
    if let Some(function) = suspend_call_fid(ir, expression, suspend_functions) {
        return original_returns.get(function as usize) == Some(&Ty::Unit);
    }
    recorded_suspension_result(ir, expression).as_ref() == Some(&Ty::Unit)
}

/// Replace the semantic tail with a return of the selected physical CPS call.
fn make_forward_body(ir: &mut IrFile, body: ExprId, call: ExprId) {
    match ir.exprs[body as usize].clone() {
        IrExpr::Return(Some(_)) => {
            ir.exprs[body as usize] = IrExpr::Return(Some(call));
        }
        IrExpr::Block {
            mut stmts,
            value: Some(_),
        } => {
            stmts.push(ir.add_expr(IrExpr::Return(Some(call))));
            ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
        }
        IrExpr::Block { stmts, value: None }
            if stmts.last().is_some_and(|last| {
                matches!(ir.exprs[*last as usize], IrExpr::Return(Some(_)))
            }) =>
        {
            let last = *stmts.last().expect("guard proved a trailing return");
            ir.exprs[last as usize] = IrExpr::Return(Some(call));
        }
        IrExpr::Block {
            mut stmts,
            value: None,
        } if stmts.last() == Some(&call) => {
            stmts.pop();
            stmts.push(ir.add_expr(IrExpr::Return(Some(call))));
            ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
        }
        // `= unitCall(…)`: remove its (possibly coerced) call statement and the bare return.
        IrExpr::Block {
            mut stmts,
            value: None,
        } if stmts.len() >= 2
            && matches!(
                ir.exprs[*stmts.last().expect("guard proved statements") as usize],
                IrExpr::Return(None)
            )
            && {
                let statement = stmts[stmts.len() - 2];
                statement == call
                    || matches!(
                        ir.exprs[statement as usize],
                        IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg, .. } if arg == call
                    )
            } =>
        {
            stmts.truncate(stmts.len() - 2);
            stmts.push(ir.add_expr(IrExpr::Return(Some(call))));
            ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
        }
        _ => {}
    }
}

/// Every expression reachable from `root`, each once, with whether a `try` encloses it. The arena
/// can share or deeply nest nodes, so this is iterative; a node shared inside and outside a `try`
/// is reported both ways.
fn reachable(ir: &IrFile, root: ExprId) -> Vec<(ExprId, bool)> {
    let mut seen = HashSet::new();
    let mut pending = vec![(root, false)];
    let mut reached = Vec::new();
    while let Some((current, in_try)) = pending.pop() {
        if !seen.insert((current, in_try)) {
            continue;
        }
        reached.push((current, in_try));
        let in_try = in_try || matches!(ir.exprs[current as usize], IrExpr::Try { .. });
        for_each_child(&ir.exprs, current, &mut |child| {
            pending.push((child, in_try))
        });
    }
    reached
}

/// A returned value with its coercions to the function's declared result peeled, and then the
/// reference cast of a generic suspend result: what a tail call leaves on the stack to return.
fn peel_returned_tail(
    ir: &IrFile,
    value: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
) -> ExprId {
    let mut value = value;
    // A dependency result is first bridged to its own declared type, then widened to a nullable
    // declared result (`Result` then `Result?`); both are identities on the forwarded `Object`.
    while let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion | IrTypeOp::Cast,
        arg,
        type_operand,
    } = ir.exprs[value as usize]
    {
        if type_operand.non_null() != declared_return.non_null()
            || uses_carrier(ir, declared_return)
        {
            break;
        }
        value = arg;
    }
    unwrap_suspend_cast(ir, value, suspend_functions, true).point
}

/// Collect the tail calls a returned `value` ends in: the value itself, each branch of a `when`
/// whose conditions do not suspend, or the value of a block whose statements do not suspend. False
/// when anything else in it suspends.
fn returned_tails(
    ir: &IrFile,
    value: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    calls: &mut Vec<ExprId>,
) -> bool {
    let peeled = peel_returned_tail(ir, value, suspend_functions, declared_return);
    if is_suspension_point(ir, peeled, suspend_functions) {
        calls.push(peeled);
        return true;
    }
    match &ir.exprs[value as usize] {
        IrExpr::When { branches } => branches.iter().all(|&(condition, branch)| {
            condition.is_none_or(|condition| !expr_calls_suspend(ir, condition, suspend_functions))
                && returned_tails(ir, branch, suspend_functions, declared_return, calls)
        }),
        IrExpr::Block {
            stmts,
            value: Some(result),
        } => {
            stmts
                .iter()
                .all(|&statement| !expr_calls_suspend(ir, statement, suspend_functions))
                && returned_tails(ir, *result, suspend_functions, declared_return, calls)
        }
        _ => !expr_calls_suspend(ir, value, suspend_functions),
    }
}

/// kotlinc's `allSuspensionPointsAreTailCalls`, for a value-returning body: every suspension point
/// is a returned tail ([`returned_tails`]) and none is inside a `try`. Then nothing runs after any
/// of them but the return itself, and kotlinc forwards `$completion` to each. The calls, or `None`.
fn all_returned_tail_calls(
    ir: &IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
) -> Option<Vec<ExprId>> {
    if declared_return == Ty::Unit {
        return None;
    }
    let reached = reachable(ir, body);
    let mut points = HashSet::new();
    for &(expression, in_try) in &reached {
        if is_suspension_point(ir, expression, suspend_functions) {
            if in_try {
                return None;
            }
            points.insert(expression);
        }
    }
    if points.is_empty() {
        return None;
    }
    let mut calls = Vec::new();
    let mut returns = HashSet::new();
    for &(expression, _) in &reached {
        let IrExpr::Return(Some(value)) = ir.exprs[expression as usize] else {
            continue;
        };
        if returns.insert(expression)
            && !returned_tails(ir, value, suspend_functions, declared_return, &mut calls)
        {
            return None;
        }
    }
    let forwarded: HashSet<ExprId> = calls.iter().copied().collect();
    (forwarded == points).then_some(calls)
}

/// Rewrite every `return` of a body [`all_returned_tail_calls`] accepted so each tail call is
/// returned as it is, the callee's CPS `Object`. A returned `when` becomes a statement whose
/// branches each return, which is how kotlinc lays it out (`invoke…; areturn` per branch); it keeps
/// its identity, so an exhaustive one still ends in its no-match throw.
fn forward_returned_tail_calls(
    ir: &mut IrFile,
    body: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    calls: &[ExprId],
) {
    let returns: Vec<ExprId> = reachable(ir, body)
        .into_iter()
        .map(|(expression, _)| expression)
        .filter(|&expression| matches!(ir.exprs[expression as usize], IrExpr::Return(Some(_))))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    for returned in returns {
        return_tail_calls(ir, returned, suspend_functions, declared_return, calls);
    }
}

fn return_tail_calls(
    ir: &mut IrFile,
    returned: ExprId,
    suspend_functions: &HashSet<u32>,
    declared_return: Ty,
    calls: &[ExprId],
) {
    let IrExpr::Return(Some(value)) = ir.exprs[returned as usize] else {
        return;
    };
    let peeled = peel_returned_tail(ir, value, suspend_functions, declared_return);
    if calls.contains(&peeled) {
        ir.exprs[returned as usize] = IrExpr::Return(Some(peeled));
        return;
    }
    if !expr_calls_suspend(ir, value, suspend_functions) {
        return;
    }
    match ir.exprs[value as usize].clone() {
        IrExpr::When { branches } => {
            let branches = branches
                .into_iter()
                .map(|(condition, branch)| {
                    let branch_return = ir.add_expr(IrExpr::Return(Some(branch)));
                    return_tail_calls(ir, branch_return, suspend_functions, declared_return, calls);
                    (condition, branch_return)
                })
                .collect();
            ir.exprs[value as usize] = IrExpr::When { branches };
        }
        IrExpr::Block {
            mut stmts,
            value: Some(result),
        } => {
            let result_return = ir.add_expr(IrExpr::Return(Some(result)));
            return_tail_calls(ir, result_return, suspend_functions, declared_return, calls);
            stmts.push(result_return);
            ir.exprs[value as usize] = IrExpr::Block { stmts, value: None };
        }
        _ => return,
    }
    ir.exprs[returned as usize] = IrExpr::Block {
        stmts: vec![value],
        value: None,
    };
}
