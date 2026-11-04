//! kotlinc's `AddContinuationToNonLocalSuspendFunctionsLowering` for the targets that realize
//! coroutines on common IR: every suspend function takes its caller's continuation as a trailing
//! `$completion: Continuation<R>` parameter and returns `Any?`, the value it completed with or the
//! `COROUTINE_SUSPENDED` marker.
//!
//! A suspend function value takes the continuation the same way. Common lowering already counts it
//! in a suspend lambda's arity (`suspend (A) -> R` is invoked as a two-argument function value, as
//! kotlinc's `SuspendFunction1.invoke(p1, $completion)` is), so after this pass the lambda's
//! implementation has the parameters its function value is invoked with.
//!
//! A body with no suspension point completes in place: it returns its result, coerced to `Any?`.
//! A body that suspends needs the state machine, which is not modeled on these targets yet; the
//! pass declines it rather than emit a body that cannot resume.

use crate::fx_hash::HashSet;
use crate::ir::{
    for_each_child, shift_value_indices, ExprId, IrExpr, IrFile, IrGeneratedParameterRole,
    IrParameterIdentity, IrTypeOp,
};
use crate::types::{wk, Ty};

use super::control_flow::stmt_diverges;
use super::suspension_points::expr_calls_suspend;

/// Why a file's suspend functions cannot take the continuation-passing form yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContinuationDecline {
    /// A body reaches a suspension point, which needs the coroutine state machine.
    Suspends,
}

impl ContinuationDecline {
    /// The construct a target names when it declines the file.
    pub(crate) fn construct(&self) -> &'static str {
        match self {
            Self::Suspends => "a suspend function that suspends",
        }
    }
}

/// Give every suspend function of `ir` its continuation parameter and its `Any?` result.
pub(crate) fn add_continuation_parameters(ir: &mut IrFile) -> Result<(), ContinuationDecline> {
    let mut suspend_functions = ir.suspend_funs.clone();
    suspend_functions.sort_unstable();
    suspend_functions.dedup();
    let suspend_set: HashSet<u32> = suspend_functions.iter().copied().collect();
    // Any body may reach a suspension point: a suspend function's own, and a non-suspend one that
    // forwards to a suspend declaration (a `$default` entry). Neither can complete in place.
    if ir
        .functions
        .iter()
        .filter_map(|function| function.body)
        .any(|body| suspends(ir, body, &suspend_set))
    {
        return Err(ContinuationDecline::Suspends);
    }
    for function in suspend_functions {
        add_continuation_parameter(ir, function);
    }
    Ok(())
}

/// Whether evaluating `body` can suspend the function that owns it.
fn suspends(ir: &IrFile, body: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if expr_calls_suspend(ir, body, suspend_set) {
        return true;
    }
    // The enclosing continuation read by `suspendCoroutineUninterceptedOrReturn`'s block. A lambda
    // body is its own function, so only captures are this frame's.
    fn reads_continuation(ir: &IrFile, expression: ExprId) -> bool {
        match &ir.exprs[expression as usize] {
            IrExpr::CurrentContinuation => true,
            IrExpr::Lambda { captures, .. } => captures
                .iter()
                .any(|&capture| reads_continuation(ir, capture)),
            _ => {
                let mut found = false;
                for_each_child(&ir.exprs, expression, &mut |child| {
                    found |= reads_continuation(ir, child);
                });
                found
            }
        }
    }
    reads_continuation(ir, body)
}

fn add_continuation_parameter(ir: &mut IrFile, function: u32) {
    let declared = {
        let declaration = &ir.functions[function as usize];
        (declaration.params.clone(), declaration.ret)
    };
    let result = declared.1;
    ir.suspend_declared_sigs.entry(function).or_insert(declared);
    let declaration = &mut ir.functions[function as usize];
    let receiver = u32::from(declaration.dispatch_receiver.is_some() && !declaration.is_static);
    let completion = receiver + declaration.params.len() as u32;
    declaration
        .params
        .push(Ty::obj_args_name(wk::continuation(), &[result]));
    declaration.param_checks.push(None);
    declaration.ret = Ty::nullable(Ty::obj_name(wk::any()));
    let body = declaration.body;
    if let Some(parameters) = ir.fn_params.get_mut(&function) {
        parameters.identities.push(IrParameterIdentity::generated(
            IrGeneratedParameterRole::Continuation,
            None,
        ));
    }
    let Some(body) = body else {
        return;
    };
    // Common lowering numbered the body's locals from the index the continuation now takes.
    shift_value_indices(ir, body, completion, 1);
    let unit = result == Ty::Unit;
    let completed = complete_in_place(ir, body, unit);
    ir.functions[function as usize].body = Some(completed);
}

/// Make every completion of a body that does not suspend return its result as `Any?`: an explicit
/// `return`, the body's value, and falling off its end. Returns the body to install, which is
/// `body` itself when it is a block.
///
/// An expression body keeps its node: the new block returns it rather than taking its arena slot,
/// so the facts recorded for the expression stay on it and the block cannot contain itself.
fn complete_in_place(ir: &mut IrFile, body: ExprId, unit: bool) -> ExprId {
    let mut visited = HashSet::default();
    coerce_returns(ir, body, unit, &mut visited);
    let IrExpr::Block { mut stmts, value } = ir.exprs[body as usize].clone() else {
        let completed = completed_value(ir, Some(body), unit);
        let returned = ir.add_expr(IrExpr::Return(Some(completed)));
        return ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
    };
    match value {
        Some(value) => {
            let completed = completed_value(ir, Some(value), unit);
            stmts.push(ir.add_expr(IrExpr::Return(Some(completed))));
        }
        None if stmts.last().is_some_and(|&last| stmt_diverges(ir, last)) => {}
        None => {
            let completed = completed_value(ir, None, unit);
            stmts.push(ir.add_expr(IrExpr::Return(Some(completed))));
        }
    }
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
    body
}

/// The `Any?` a completion with `value` returns. A `Unit` function evaluates its value for effect
/// and completes with `Unit`.
fn completed_value(ir: &mut IrFile, value: Option<ExprId>, unit: bool) -> ExprId {
    if unit {
        let unit_value = ir.add_expr(IrExpr::UnitInstance);
        return match value {
            Some(value) => ir.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit_value),
            }),
            None => unit_value,
        };
    }
    let value = value.expect("a non-Unit completion has a value");
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: Ty::nullable(Ty::obj_name(wk::any())),
    })
}

/// Coerce the value of every `return` this body owns to `Any?`. A lambda's implementation is its
/// own function; only a retained `inline_body`, whose returns leave this function, is walked.
fn coerce_returns(ir: &mut IrFile, expression: ExprId, unit: bool, visited: &mut HashSet<ExprId>) {
    if !visited.insert(expression) {
        return;
    }
    match ir.exprs[expression as usize] {
        IrExpr::Return(value) => {
            if let Some(value) = value {
                coerce_returns(ir, value, unit, visited);
            }
            let completed = completed_value(ir, value, unit);
            ir.exprs[expression as usize] = IrExpr::Return(Some(completed));
        }
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
            for child in children {
                coerce_returns(ir, child, unit, visited);
            }
        }
    }
}

#[cfg(test)]
mod tests;
