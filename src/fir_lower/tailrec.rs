//! Tail-position rewriting for checked self calls.
//!
//! Calls reach this module only after FIR has selected a stable callable identity and mapped every
//! argument to its declaration parameter. The transform therefore contains no name lookup or
//! overload logic.

use crate::fir::{OriginId, SyntheticOriginKind};
use crate::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile, IrNodeOrigin};
use crate::types::Ty;

use super::FirLoweringFailure;

const LOOP_LABEL: &str = "$tailrec";

pub(super) fn finish_tailrec_body(
    ir: &mut IrFile,
    mut roots: Vec<ExprId>,
    function: FunId,
    parameter_count: usize,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    let result = ir.functions[function as usize].ret;
    let tail = roots
        .pop()
        .ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
    let tail = tail_value(ir, tail, function, parameter_count, result, origin)?;
    roots.push(tail);
    // The body's tail is one tail position; a `return` is another, wherever it stands, because
    // nothing of this function runs after one. `tail_value` reads the first off the body's shape,
    // and this sweeps the rest out of the whole body — after the rebuild above, so a tail the
    // rebuild already turned into a loop step is not visited a second time.
    for &root in &roots {
        rewrite_returned_tail_calls(ir, root, function, parameter_count, result, origin)?;
    }
    let loop_body = generated(
        ir,
        IrExpr::Block {
            stmts: roots,
            value: None,
        },
        origin,
    );
    let condition = generated(ir, IrExpr::Const(IrConst::Boolean(true)), origin);
    let loop_expression = generated(
        ir,
        IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    );
    Ok(generated(
        ir,
        IrExpr::Block {
            stmts: vec![loop_expression],
            value: None,
        },
        origin,
    ))
}

/// Rewrite every self-call that a `return` puts in tail position, wherever in the body it stands.
///
/// What makes a `return` a tail position is not the shape it sits in: nothing of this function runs
/// after one, so the call it returns is a tail call in a block, in a `when` branch, and inside a
/// LOOP — Kotlin reads `while (…) { if (…) return f(x) }` as a tail call, and the `continue` this
/// writes carries the synthetic loop's own label, so leaving the inner loop is the rewrite working
/// rather than a reason to skip it.
///
/// What does stop the walk is OWNERSHIP of the `return`, and of what runs after it:
///
/// * An inlined lambda's body. Its `return`s are the lambda's to answer, and a non-local one that
///   is this function's reaches the surrounding statement anyway; a local one does not, and nothing
///   at this level tells them apart. The lambda's CAPTURES are ordinary expressions of this
///   function and stay in the walk.
/// * A `try`. Its `finally` still has to run, so a `return` inside it does not leave directly.
///
/// The rewrite happens IN PLACE, at the `return`'s own id: it is the same statement, saying the
/// same thing, and everything that referred to it still does.
fn rewrite_returned_tail_calls(
    ir: &mut IrFile,
    root: ExprId,
    function: FunId,
    parameter_count: usize,
    result: Ty,
    origin: OriginId,
) -> Result<(), FirLoweringFailure> {
    let mut pending = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            // A `try` keeps whatever it holds: its `finally` runs after the `return`.
            IrExpr::Try { .. } => continue,
            // An inlined lambda's body is the lambda's; its captures are this function's.
            IrExpr::Lambda { captures, .. } => {
                pending.extend(captures.clone());
                continue;
            }
            IrExpr::Return(Some(_)) => {
                // The same rewriter the body's own tail goes through, asked about this `return`
                // instead: it is a tail position too, so whatever it makes of the body's last
                // expression it makes of this one. The answer replaces the `return` where it
                // stands, and a `return` it left a `return` is walked into like anything else.
                let rebuilt =
                    tail_value(ir, expression, function, parameter_count, result, origin)?;
                let node = ir.expr(rebuilt).clone();
                let stepped = !matches!(node, IrExpr::Return(_));
                ir.exprs[expression as usize] = node;
                if stepped {
                    continue;
                }
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    Ok(())
}

/// Whether `call` is this function calling itself with its whole parameter list — the only shape
/// the loop can step. A partial list is somebody else's overload, and a call with a receiver to
/// re-bind is not the same frame.
fn is_self_call(ir: &IrFile, call: ExprId, function: FunId, parameter_count: usize) -> bool {
    matches!(
        ir.expr(call),
        IrExpr::Call {
            callee: Callee::Local(target),
            dispatch_receiver: None,
            args,
        } if *target == function && args.len() == parameter_count
    )
}

/// The loop step a self-call becomes: every parameter reassigned, then `continue` to the synthetic
/// loop. `call` must satisfy [`is_self_call`].
fn loop_step(ir: &mut IrFile, call: ExprId, parameter_count: usize, origin: OriginId) -> IrExpr {
    let IrExpr::Call { args, .. } = ir.expr(call).clone() else {
        unreachable!("a self call is a call")
    };
    let mut updates = Vec::with_capacity(parameter_count + 1);
    for (parameter, value) in args.into_iter().enumerate() {
        updates.push(generated(
            ir,
            IrExpr::SetValue {
                var: u32::try_from(parameter)
                    .expect("tailrec parameter count exceeds packed value ids"),
                value,
            },
            origin,
        ));
    }
    updates.push(generated(
        ir,
        IrExpr::Continue {
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    ));
    IrExpr::Block {
        stmts: updates,
        value: None,
    }
}

fn tail_value(
    ir: &mut IrFile,
    expression: ExprId,
    function: FunId,
    parameter_count: usize,
    result: Ty,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    match ir.expr(expression).clone() {
        IrExpr::Call { .. } if is_self_call(ir, expression, function, parameter_count) => {
            let step = loop_step(ir, expression, parameter_count, origin);
            Ok(generated(ir, step, origin))
        }
        IrExpr::Block {
            mut stmts,
            value: Some(value),
        } if result == Ty::Unit && matches!(ir.expr(value), IrExpr::UnitInstance) => {
            // A checked `CoerceToUnit` boundary is represented as `{ effect; Unit }`. The singleton
            // is not an effect after the recursive call, so the final statement remains the real
            // tail position. This is the shape produced by a Unit-returning `if`/`when` whose
            // recursive call occupies one arm.
            if let Some(tail) = stmts.pop() {
                stmts.push(tail_value(
                    ir,
                    tail,
                    function,
                    parameter_count,
                    result,
                    origin,
                )?);
            } else {
                stmts.push(generated(ir, IrExpr::Return(None), origin));
            }
            Ok(generated(ir, IrExpr::Block { stmts, value: None }, origin))
        }
        IrExpr::Block { mut stmts, value } => {
            if let Some(value) = value {
                stmts.push(tail_value(
                    ir,
                    value,
                    function,
                    parameter_count,
                    result,
                    origin,
                )?);
            } else if let Some(tail) = stmts.pop() {
                // A block-bodied function carries its explicit `return` (or Unit tail statement)
                // as the final statement rather than as the block value. It is still the sole tail
                // position of this block. The statements before it are tail positions only
                // where they `return`.
                stmts.push(tail_value(
                    ir,
                    tail,
                    function,
                    parameter_count,
                    result,
                    origin,
                )?);
            } else if result == Ty::Unit {
                stmts.push(generated(ir, IrExpr::Return(None), origin));
            } else {
                return Err(FirLoweringFailure::MissingBodyResult { origin });
            }
            Ok(generated(ir, IrExpr::Block { stmts, value: None }, origin))
        }
        IrExpr::Return(Some(value)) => {
            tail_value(ir, value, function, parameter_count, result, origin)
        }
        // A LOOP is not a value and has no tail position of its own: what leaves the function from
        // inside one is a `return`, which the sweep reaches wherever it stands. Wrapping the loop
        // in a `return` instead would return the loop — which is what a body ending in
        // `while (true) { … return f(x) }` used to compile to, and the verifier said so.
        IrExpr::While { .. } => Ok(expression),
        IrExpr::Return(None) => Ok(expression),
        IrExpr::When { branches } => {
            let branches = branches
                .into_iter()
                .map(|(condition, branch)| {
                    Ok((
                        condition,
                        tail_value(ir, branch, function, parameter_count, result, origin)?,
                    ))
                })
                .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
            Ok(generated(ir, IrExpr::When { branches }, origin))
        }
        _ if result == Ty::Unit => {
            let returned = generated(ir, IrExpr::Return(None), origin);
            Ok(generated(
                ir,
                IrExpr::Block {
                    stmts: vec![expression, returned],
                    value: None,
                },
                origin,
            ))
        }
        _ => Ok(generated(ir, IrExpr::Return(Some(expression)), origin)),
    }
}

fn generated(ir: &mut IrFile, expression: IrExpr, cause: OriginId) -> ExprId {
    let id = ir.add_expr(expression);
    ir.fir_origins.insert(
        id,
        IrNodeOrigin::Synthetic {
            cause,
            kind: SyntheticOriginKind::GeneratedControlFlow,
        },
    );
    id
}
