//! Detection and body rewriting for a suspend function that directly forwards its continuation.

use super::bottom_completion::unwrap_suspend_cast;
use super::{is_suspension_point, recorded_suspension_result, suspend_call_fid};
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

/// If the body contains one suspension directly in tail position, select the physical call whose
/// result can be returned with this function's continuation and no local state machine.
pub(super) fn tail_forward_call(
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
    let uses_carrier = |ty: Ty| {
        let ty = ty.non_null();
        ty.is_unsigned()
            || ty.obj_internal().is_some_and(|name| {
                ir.classes
                    .iter()
                    .any(|class| class.is_value && class.fq_name == name)
                    || ir.has_external_value_class_name(name)
            })
    };
    let tail = match ir.exprs[tail as usize] {
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if type_operand == declared_return && !uses_carrier(type_operand) => arg,
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
pub(super) fn make_forward_body(ir: &mut IrFile, body: ExprId, call: ExprId) {
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
