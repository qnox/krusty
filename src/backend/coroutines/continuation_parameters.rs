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
//! A body that can suspend is moved to a generated continuation class with its live value frame.

use crate::fx_hash::HashSet;
use crate::ir::{
    for_each_child, shift_value_indices, Callee, ExprId, IrConst, IrExpr, IrFile,
    IrGeneratedParameterRole, IrParameterIdentity, IrTypeOp,
};
use crate::types::{continuation_passing, wk, Ty};

use super::control_flow::stmt_diverges;

/// Why a file's suspend functions cannot take the continuation-passing form yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuationDecline {
    /// A body reaches a suspension point, which needs the coroutine state machine.
    Suspends,
}

impl ContinuationDecline {
    /// The construct a target names when it declines the file.
    pub fn construct(&self) -> &'static str {
        match self {
            Self::Suspends => "a suspend function that suspends",
        }
    }
}

/// Give every suspend function of `ir` its continuation parameter and its `Any?` result.
pub fn add_continuation_parameters(ir: &mut IrFile) -> Result<(), ContinuationDecline> {
    let runtime = ir.coroutine_runtime;
    add_continuation_parameters_with_runtime(ir, runtime)
}

/// Give every suspend function its continuation parameter and realize bodies that suspend using
/// the exact runtime identities selected at the backend handoff.
pub(crate) fn add_continuation_parameters_with_runtime(
    ir: &mut IrFile,
    runtime: Option<crate::ir::IrCoroutineRuntime>,
) -> Result<(), ContinuationDecline> {
    if ir.coroutine_runtime.is_none() {
        ir.coroutine_runtime = runtime;
    }
    let mut suspend_functions = ir.suspend_funs.clone();
    suspend_functions.sort_unstable();
    suspend_functions.dedup();
    let suspend_set: HashSet<u32> = suspend_functions.iter().copied().collect();
    let completing = completing_suspend_functions(ir, &suspend_set);
    let machine_functions: Vec<u32> = suspend_functions
        .iter()
        .copied()
        .filter(|&function| {
            ir.functions[function as usize]
                .body
                .is_some_and(|body| body_needs_state_machine(ir, body, &suspend_set, &completing))
        })
        .collect();
    // A non-suspend synthetic entry (most notably a default stub) can also contain a call that
    // reaches a suspension. It has no continuation parameter to capture, so keep declining that
    // shape until its producer supplies the call-frame contract.
    if ir
        .functions
        .iter()
        .enumerate()
        .any(|(function, declaration)| {
            !suspend_set.contains(&(function as u32))
                && declaration.body.is_some_and(|body| {
                    body_needs_state_machine(ir, body, &suspend_set, &completing)
                })
        })
    {
        return Err(ContinuationDecline::Suspends);
    }
    if !machine_functions.is_empty() && runtime.is_none() {
        return Err(ContinuationDecline::Suspends);
    }
    for function in suspend_functions {
        add_continuation_parameter(ir, function);
    }
    for function in machine_functions {
        super::state_machine::realize(ir, function, &suspend_set)
            .map_err(|_| ContinuationDecline::Suspends)?;
    }
    pass_continuations(ir, &suspend_set);
    Ok(())
}

/// Suspend functions whose bodies return their value and never resume a continuation.
///
/// A function is rejected when its body has no implementation, reads the current continuation,
/// contains an intrinsic suspension, calls a suspend function outside this file, or calls a
/// rejected function. Calls among the rest complete in place, including a cycle of them.
fn completing_suspend_functions(ir: &IrFile, suspend_set: &HashSet<u32>) -> HashSet<u32> {
    let mut rejected = HashSet::default();
    for &function in suspend_set {
        match ir
            .functions
            .get(function as usize)
            .and_then(|declaration| declaration.body)
        {
            None => {
                rejected.insert(function);
            }
            Some(body) if has_unrealized_suspension(ir, body, suspend_set) => {
                rejected.insert(function);
            }
            Some(_) => {}
        }
    }
    loop {
        let decided = rejected.len();
        for &function in suspend_set {
            if rejected.contains(&function) {
                continue;
            }
            let Some(body) = ir.functions[function as usize].body else {
                continue;
            };
            if calls_rejected_suspend(ir, body, suspend_set, &rejected) {
                rejected.insert(function);
            }
        }
        if rejected.len() == decided {
            break;
        }
    }
    suspend_set
        .iter()
        .copied()
        .filter(|function| !rejected.contains(function))
        .collect()
}

/// Whether `body` reaches a suspension this pass cannot turn into a direct call.
fn body_needs_state_machine(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    completing: &HashSet<u32>,
) -> bool {
    has_unrealized_suspension(ir, body, suspend_set)
        || any_in_body(ir, body, |ir, expression| {
            direct_suspend_callee(ir, expression, suspend_set)
                .is_some_and(|function| !completing.contains(&function))
        })
}

/// A continuation read, an intrinsic suspension, a suspend call whose callee is not a function of
/// this file, or a suspend method call whose implementation is selected at run time. A lambda body
/// belongs to the lambda's function, so only its captures count here.
fn has_unrealized_suspension(ir: &IrFile, body: ExprId, suspend_set: &HashSet<u32>) -> bool {
    any_in_body(ir, body, |ir, expression| {
        matches!(ir.expr(expression), IrExpr::CurrentContinuation)
            || ir.intrinsic_suspension_points.contains_key(&expression)
            || (ir.suspend_calls.contains_key(&expression)
                && direct_suspend_callee(ir, expression, suspend_set).is_none())
            || suspend_method_slot(ir, expression, suspend_set).is_some_and(|(_, closed)| !closed)
    })
}

/// Whether `body` calls a suspend function already known to need the state machine.
fn calls_rejected_suspend(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    rejected: &HashSet<u32>,
) -> bool {
    any_in_body(ir, body, |ir, expression| {
        direct_suspend_callee(ir, expression, suspend_set)
            .is_some_and(|function| rejected.contains(&function))
    })
}

/// Whether any node of `body` makes `hit` true. A shared child is visited once.
fn any_in_body(ir: &IrFile, body: ExprId, mut hit: impl FnMut(&IrFile, ExprId) -> bool) -> bool {
    fn walk(
        ir: &IrFile,
        expression: ExprId,
        hit: &mut impl FnMut(&IrFile, ExprId) -> bool,
        seen: &mut HashSet<ExprId>,
    ) -> bool {
        if !seen.insert(expression) {
            return false;
        }
        if hit(ir, expression) {
            return true;
        }
        let lambda_captures = match ir.expr(expression) {
            IrExpr::Lambda { captures, .. } => Some(captures.clone()),
            _ => None,
        };
        if let Some(captures) = lambda_captures {
            return captures
                .into_iter()
                .any(|capture| walk(ir, capture, hit, seen));
        }
        let mut children = Vec::new();
        for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        children.into_iter().any(|child| walk(ir, child, hit, seen))
    }
    walk(ir, body, &mut hit, &mut HashSet::default())
}

/// The same-file suspend function a direct call runs, when `expression` is such a call.
///
/// A `$default` entry keeps the callee's identity but is its own function: its parameters were not
/// given a continuation, and the call it makes to the declaration is the one that needs one.
///
/// A method call names a slot of its static class; the implementation that runs is selected at
/// run time. The slot's function is the one that runs only when no override can replace it: the
/// method is closed (`open_methods` does not hold it) or its class can have no subclass. Any other
/// method call keeps its unknown implementation, which may suspend.
fn direct_suspend_callee(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<u32> {
    match ir.expr(expression) {
        IrExpr::Call { callee, .. } => match &**callee {
            Callee::Local(function) | Callee::ClassStatic { function, .. }
                if suspend_set.contains(function) =>
            {
                Some(*function)
            }
            _ => None,
        },
        IrExpr::MethodCall { .. } => match suspend_method_slot(ir, expression, suspend_set)? {
            (function, true) => Some(function),
            (_, false) => None,
        },
        _ => None,
    }
}

/// The same-file suspend function in the slot a method call names, and whether that function is
/// the implementation that runs.
fn suspend_method_slot(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<(u32, bool)> {
    let IrExpr::MethodCall { class, index, .. } = ir.expr(expression) else {
        return None;
    };
    let class = &ir.classes[*class as usize];
    let function = *class.methods.get(*index as usize)?;
    if !suspend_set.contains(&function) {
        return None;
    }
    let closed = !class.is_interface
        && (!ir.open_methods.contains(&function)
            || !(class.is_open || class.is_abstract || class.is_sealed));
    Some((function, closed))
}

/// Pass a continuation into every direct call of a suspend function and read the call at the
/// function's declared result. The callee completes in place and never resumes the continuation,
/// so the value is `null`.
fn pass_continuations(ir: &mut IrFile, suspend_set: &HashSet<u32>) {
    let sites: Vec<ExprId> = (0..ir.exprs.len() as ExprId)
        .filter(|&expression| {
            !ir.call_declared_ret.contains_key(&expression)
                && direct_suspend_callee(ir, expression, suspend_set).is_some()
        })
        .collect();
    if sites.is_empty() {
        return;
    }
    let continuation = ir.add_expr(IrExpr::Const(IrConst::Null));
    let suspended = Ty::nullable(Ty::obj_name(wk::any()));
    for site in sites {
        let Some(function) = direct_suspend_callee(ir, site, suspend_set) else {
            continue;
        };
        let Some(&(_, declared)) = ir.suspend_declared_sigs.get(&function) else {
            continue;
        };
        let logical = ir.checked_type(site).unwrap_or(declared);
        let parameters = ir.functions[function as usize].params.len();
        let call = match ir.exprs[site as usize].clone() {
            IrExpr::Call {
                callee,
                dispatch_receiver,
                mut args,
            } if args.len() + 1 == parameters => {
                args.push(continuation);
                ir.add_expr(IrExpr::Call {
                    callee,
                    dispatch_receiver,
                    args,
                })
            }
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                mut args,
            } if args.iter().all(Option::is_some) && args.len() + 1 == parameters => {
                args.push(Some(continuation));
                ir.add_expr(IrExpr::MethodCall {
                    class,
                    index,
                    receiver,
                    args,
                })
            }
            _ => continue,
        };
        ir.call_declared_ret.insert(call, suspended);
        ir.exprs[site as usize] = IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: logical,
        };
    }
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
    let (continuation, completed) = continuation_passing(result);
    declaration.params.push(continuation);
    declaration.param_checks.push(None);
    declaration.ret = completed;
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
/// `return`, the body's value, and a `Unit` body falling off its end. A non-`Unit` block with no
/// result expression keeps the returns it already has. Returns the body to install, which is
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
        // A non-Unit block with no result expression completes through the returns already in it.
        // A `tailrec` body is that shape: `do { …; break } while (true)`, and the value leaves by
        // `return` inside the loop. There is no value to invent after the loop.
        None if !unit || stmts.last().is_some_and(|&last| stmt_diverges(ir, last)) => {}
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
