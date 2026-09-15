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
    rewrite_returned_tail_calls(ir, &roots, function, parameter_count, result, origin)?;
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

/// Whether one or several root-to-node paths reach each node in the body.
///
/// Every structural edge is counted, including edges the rewrite itself will not follow — a `try`'s
/// contents and a lambda's inline body. Sharing propagates through descendants: if two paths reach a
/// block, they also reach the return stored inside that block even though the return has only one
/// direct parent node. Counts are capped at two because the rewrite needs only a uniqueness proof.
fn root_path_counts(ir: &IrFile, roots: &[ExprId]) -> std::collections::HashMap<ExprId, u8> {
    let mut paths: std::collections::HashMap<ExprId, u8> = std::collections::HashMap::new();
    // A root is owned by the body itself, which is an edge like any other.
    for &root in roots {
        let count = paths.entry(root).or_default();
        *count = count.saturating_add(1).min(2);
    }
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut expanded = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        // A node declares its children once however many edges reach it; expanding it twice would
        // count the same edges again rather than find new ones.
        if !expanded.insert(expression) {
            continue;
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            let count = paths.entry(child).or_default();
            *count = count.saturating_add(1).min(2);
            pending.push(child);
        }
    }

    // Direct incoming-edge counts do not expose sharing THROUGH an ancestor: expanding a shared
    // block once records one edge to its child even though two root paths observe that child. Mark
    // every descendant of a multiply reached node multiply reached as well.
    let mut pending = paths
        .iter()
        .filter_map(|(&expression, &count)| (count > 1).then_some(expression))
        .collect::<Vec<_>>();
    let mut propagated = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !propagated.insert(expression) {
            continue;
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            paths.insert(child, 2);
            pending.push(child);
        }
    }
    paths
}

/// Whether a `return` leaves THIS function.
///
/// The checked depth is the authority, not the shape the `return` sits in: zero is a return of this
/// callable and a deeper one targets a frame outside it. A `return` lowering generated itself
/// carries no depth and is this function's by construction.
fn returns_from_here(ir: &IrFile, expression: ExprId) -> bool {
    ir.checked_return_depths
        .get(&expression)
        .is_none_or(|&depth| depth == 0)
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
/// * An inlined lambda's body. Its `return`s answer to the lambda, and a depth-ZERO one there is
///   the lambda's own — the one shape the checked depth below cannot tell apart from this
///   function's, which is why the boundary and not the depth is what keeps the walk out. The
///   lambda's CAPTURES are ordinary expressions of this function and stay in the walk.
/// * A `try`. Its `finally` still has to run, so a `return` inside it does not leave directly.
///
/// The rewrite happens IN PLACE, at the `return`'s own id, and two facts make that sound rather
/// than convenient:
///
/// * The node is reached by exactly ONE root-to-node path. A `return` below a shared ancestor is
///   shared too even when it has one direct parent node. A multiply reached return is left alone —
///   the program keeps recursing, which is the answer this pass started from and is never a wrong
///   one. Rewriting it would change what another path sees, and that path may cross the `try` or
///   inline-body boundary this walk deliberately did not enter.
/// * The `return` is this function's, by its checked depth rather than by where it was found.
///
/// A slot that stops being a `Return` gives up its `checked_return_depths` entry with it: that fact
/// describes a return node, and the side table's contract is that only a return carries one.
fn rewrite_returned_tail_calls(
    ir: &mut IrFile,
    roots: &[ExprId],
    function: FunId,
    parameter_count: usize,
    result: Ty,
    origin: OriginId,
) -> Result<(), FirLoweringFailure> {
    let paths = root_path_counts(ir, roots);
    let mut pending: Vec<ExprId> = roots.to_vec();
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
            IrExpr::Return(Some(_))
                if returns_from_here(ir, expression) && paths.get(&expression) == Some(&1) =>
            {
                // The same rewriter the body's own tail goes through, asked about this `return`
                // instead: it is a tail position too, so whatever it makes of the body's last
                // expression it makes of this one. The answer replaces the `return` where it
                // stands, and a `return` it left a `return` is walked into like anything else.
                let rebuilt =
                    tail_value(ir, expression, function, parameter_count, result, origin)?;
                let node = ir.expr(rebuilt).clone();
                let stepped = !matches!(node, IrExpr::Return(_));
                if stepped {
                    ir.checked_return_depths.remove(&expression);
                }
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

/// Whether a self-call survives in `body`.
///
/// This is the one thing a backend needs to know about a `tailrec` the rewrite could not finish.
/// The `loopable` gate above answers whether the rewrite was ATTEMPTED; it cannot answer whether it
/// reached every self-call, because a call in a position the rewrite does not descend into — under
/// a `try`, inside an inline body, or one the DAG shares — stays a call in a function that was
/// loop-rewritten everywhere else. Such a function recurses to exactly the depth the source wrote
/// `tailrec` to avoid, so a backend that cannot make it constant-stack on its own has to hear about
/// it here rather than discover it as a stack overflow at run time.
pub(super) fn recurses_into_itself(ir: &IrFile, body: ExprId, function: FunId) -> bool {
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Call {
            callee: Callee::Local(target),
            ..
        } = ir.expr(expression)
        {
            if *target == function {
                return true;
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
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
        // A `Unit` function's block can end in a bare `return`. Everything that runs before it and
        // after nothing else is still in TAIL position — `f(x); return` is a tail call in Kotlin,
        // and the source wrote `tailrec` because that call recurses to a depth no stack survives.
        // Only the statement immediately before the `return` qualifies: anything earlier has code
        // after it.
        IrExpr::Block {
            mut stmts,
            value: None,
        } if result == Ty::Unit
            && matches!(
                stmts.last().map(|last| ir.expr(*last)),
                Some(IrExpr::Return(None))
            )
            && stmts.len() > 1 =>
        {
            let returned = stmts.pop().expect("checked non-empty just above");
            let tail = stmts
                .pop()
                .expect("checked for a second statement just above");
            stmts.push(tail_value(
                ir,
                tail,
                function,
                parameter_count,
                result,
                origin,
            )?);
            stmts.push(returned);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrFunction, IrNodeOrigin};

    const FUNCTION: FunId = 0;

    /// A one-parameter `tailrec fun step(n: Int): Int` with nothing in it yet. The body is supplied
    /// per test, because what each test is about is the SHAPE the sweep is handed.
    fn file() -> IrFile {
        let mut ir = IrFile::default();
        ir.add_fun(IrFunction {
            name: "step".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir
    }

    /// `return step(n)` — the shape the rewrite turns into a loop step.
    fn returned_self_call(ir: &mut IrFile) -> ExprId {
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(FUNCTION),
            dispatch_receiver: None,
            args: vec![argument],
        });
        ir.add_expr(IrExpr::Return(Some(call)))
    }

    fn finish(ir: &mut IrFile, roots: Vec<ExprId>) {
        finish_tailrec_body(ir, roots, FUNCTION, 1, OriginId::from_raw(0))
            .expect("the body has a tail");
    }

    /// Every node carrying a checked return depth is still a `Return`, which is the side table's
    /// documented contract.
    fn depths_describe_returns(ir: &IrFile) -> bool {
        ir.checked_return_depths
            .keys()
            .all(|&expression| matches!(ir.expr(expression), IrExpr::Return(_)))
    }

    #[test]
    fn a_return_reached_by_one_path_becomes_a_loop_step() {
        // The control for the two tests below: with nothing else pointing at it, the `return` is
        // rewritten, so a later "left alone" is the guard working and not the rewrite being off.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![returned, tail]);

        assert!(
            matches!(ir.expr(returned), IrExpr::Block { .. }),
            "a uniquely owned `return` of a self call is the loop step"
        );
        // The slot stopped being a `Return`, so the fact that described it went with it.
        assert_eq!(ir.checked_return_depths.get(&returned), None);
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_the_dag_shares_with_a_try_is_left_alone() {
        // Common IR is a DAG. This `return` is reachable both as an ordinary statement and from
        // inside a `try`, which the sweep deliberately does not enter — so rewriting it through
        // the edge the sweep DOES follow would change what the `try` holds, where a `finally` still
        // has to run. One edge is the whole licence to rewrite in place, and there are two here.
        let mut ir = file();
        let shared = returned_self_call(&mut ir);
        let guarded = ir.add_expr(IrExpr::Try {
            body: shared,
            catches: Vec::new(),
            finally: None,
            result: Ty::Int,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![shared, guarded, tail]);

        assert!(
            matches!(ir.expr(shared), IrExpr::Return(Some(_))),
            "a `return` the DAG shares is left as it was"
        );
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_below_a_shared_ancestor_is_left_alone() {
        // The return has one DIRECT parent (the block), but the block itself is reached both as an
        // ordinary root and through an opaque `try`. Path uniqueness must propagate through the
        // shared ancestor; direct incoming-edge counting alone incorrectly rewrites the return.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let shared_block = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        let guarded = ir.add_expr(IrExpr::Try {
            body: shared_block,
            catches: Vec::new(),
            finally: None,
            result: Ty::Int,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![shared_block, guarded, tail]);

        assert!(matches!(ir.expr(returned), IrExpr::Return(Some(_))));
        assert_eq!(ir.checked_return_depths.get(&returned), Some(&0));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_under_a_privately_owned_ancestor_is_still_swept() {
        // The control for the test above, and the one a descendant-marking rule needs most: the
        // same nesting with nothing else pointing at the block. One path reaches the return, so it
        // becomes the loop step. Without this, marking EVERY descendant of every node shared would
        // pass the test above while quietly switching the sweep off for anything inside a block.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let owned = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![owned, tail]);

        assert!(
            matches!(ir.expr(returned), IrExpr::Block { .. }),
            "one path to the `return` is the licence to rewrite it where it stands"
        );
        assert_eq!(ir.checked_return_depths.get(&returned), None);
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_lambdas_capture_is_swept_and_its_inline_body_is_not() {
        // The two halves of one boundary. A lambda's captures are evaluated by THIS function, so a
        // `return` among them is this function's tail position; its inline body belongs to the
        // lambda, and a depth-ZERO return there is the lambda's own — the one shape the checked
        // depth cannot tell apart from this function's, which is why the boundary and not the depth
        // is what keeps the sweep out of an inline body.
        let mut ir = file();
        let capture = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(capture, 0);
        let inner = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(inner, 0);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: FUNCTION,
            arity: 0,
            captures: vec![capture],
            sam: None,
            inline_body: Some(inner),
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![lambda, tail]);

        assert!(
            matches!(ir.expr(capture), IrExpr::Block { .. }),
            "a capture expression is this function's and is swept"
        );
        assert!(
            matches!(ir.expr(inner), IrExpr::Return(Some(_))),
            "an inline body is the lambda's and is left whole"
        );
        assert_eq!(ir.checked_return_depths.get(&inner), Some(&0));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_that_targets_an_outer_frame_is_left_alone() {
        // The ownership test is the checked DEPTH, not the shape the `return` was found in: a
        // non-zero depth names a frame outside this one, and stepping this function's loop would
        // answer a question nobody asked.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 1);
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![returned, tail]);

        assert!(matches!(ir.expr(returned), IrExpr::Return(Some(_))));
        assert_eq!(ir.checked_return_depths.get(&returned), Some(&1));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_generated_node_records_where_it_came_from() {
        let mut ir = file();
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![tail]);
        let generated = ir.exprs.len() as ExprId - 1;
        assert!(matches!(
            ir.fir_origins.get(&generated),
            Some(IrNodeOrigin::Synthetic { .. })
        ));
    }
}
