//! Finding the suspensions that the IR coroutine machine cannot see.
//!
//! A lambda passed to a classpath `inline` function is not a closure: once the call is spliced, its
//! body runs in the enclosing method's own frame. The IR machine nevertheless stops at every
//! `Lambda` (`expr_calls_suspend` does so deliberately), so a suspension in such a body is invisible
//! to it — the enclosing function is classified as having no suspension point at all, and the
//! spliced call is then emitted with no continuation to pass. That is the
//! `call arity mismatch` bail.
//!
//! These are the suspensions whose machine moves to emission, where the spliced body's own locals
//! exist. See `docs/JVM_INLINE_BEFORE_CPS.md`.
//!
//! Carrying an `inline_body` is NOT by itself proof that the body will run in this frame: the
//! emitter may decline the splice, in which case the lambda is realized as an ordinary `invoke` and
//! the suspension belongs to that method instead. So this answer alone must never take a function
//! away from the IR machine. The routing gate is the conjunction — a function the IR machine finds
//! no suspension point in, which nevertheless has one here — because that is exactly the function
//! that would otherwise be emitted with no continuation to pass.

use super::super::is_suspension_point;
use crate::ir::{for_each_child, Callee, ExprId, IrExpr, IrFile};
use crate::libraries::InlineKind;
use std::collections::HashSet;

/// Whether `expression` is a call whose callee the JVM backend may splice here.
///
/// Carrying an `inline_body` does NOT make a lambda's body part of this frame: lowering attaches
/// one wherever it can, including to the lambda of an ordinary function (`launch { … }`). Only the
/// argument of an INLINE call can be spliced, so only there does the body belong to this frame.
pub(crate) fn calls_an_inline_function(ir: &IrFile, expression: ExprId) -> bool {
    let IrExpr::Call {
        callee: Callee::Static { name, inline, .. },
        ..
    } = &ir.exprs[expression as usize]
    else {
        return false;
    };
    match inline {
        InlineKind::None => false,
        // The emitter's own rule: a `$default` synthetic is an ABI dispatcher whose mask prologue
        // has to run as emitted, so only a splice-only declaration bypasses it. Claiming one here
        // would promise a machine a state that never gets emitted.
        InlineKind::CanInline => !name.ends_with("$default"),
        InlineKind::MustInline => true,
    }
}

/// Suspension points inside the `inline_body` of a lambda reached from `body`, in encounter order.
///
/// A nested lambda that is *not* inlined is a real closure boundary and stops the walk, exactly as
/// it does for the IR machine: its body belongs to a generated `invoke`, not to this frame.
pub(crate) fn spliced_inline_suspensions(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> Vec<ExprId> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    walk_frame(ir, body, suspend_set, false, &mut seen, &mut found);
    found
}

/// EVERY suspension this frame runs, in encounter order: its own, and those inside the spliced
/// bodies it will contain.
///
/// A function with both kinds cannot be split between two machines — one method has one dispatch —
/// so when the emit-time machine takes such a function it takes all of them.
pub(crate) fn frame_suspensions(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> Vec<ExprId> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    walk(
        ir,
        body,
        suspend_set,
        true,
        false,
        true,
        &mut seen,
        &mut found,
    );
    found
}

/// Whether a non-local return in a body that will be spliced crosses a `finally` boundary.
///
/// Return boxing is sufficient for a direct non-local return, but the emit-time machine cannot yet
/// route a return through a `finally`: `linearize_finally_returns` stops at a lambda boundary, and
/// the machine deliberately rejects an unnormalized returning `try/finally`.
/// Keep that unsupported shape on the old fail-closed path while admitting the direct returns this
/// pass can represent.
pub(crate) fn spliced_return_crosses_finally(ir: &IrFile, body: ExprId) -> bool {
    let mut stack = vec![(body, false, false, false)];
    let mut seen = HashSet::new();
    while let Some((at, inlined, spliceable, crosses_finally)) = stack.pop() {
        if !seen.insert((at, inlined, spliceable, crosses_finally)) {
            continue;
        }
        match &ir.exprs[at as usize] {
            IrExpr::Return(_) if inlined && crosses_finally => return true,
            IrExpr::Lambda {
                captures,
                inline_body,
                ..
            } => {
                for &capture in captures {
                    stack.push((capture, inlined, false, crosses_finally));
                }
                if let (true, Some(&inner)) = (spliceable, inline_body.as_ref()) {
                    stack.push((inner, true, false, crosses_finally));
                }
                continue;
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } => {
                let protected_crosses = crosses_finally || finally.is_some();
                stack.push((*body, inlined, spliceable, protected_crosses));
                for catch in catches {
                    stack.push((catch.body, inlined, spliceable, protected_crosses));
                }
                if let Some(finally) = finally {
                    // A return FROM this finalizer does not cross the same finalizer, but it still
                    // crosses any one surrounding this `try`.
                    stack.push((*finally, inlined, spliceable, crosses_finally));
                }
                continue;
            }
            _ => {}
        }
        let operands_are_spliceable = match &ir.exprs[at as usize] {
            IrExpr::Call { .. } => calls_an_inline_function(ir, at),
            _ => spliceable,
        };
        for_each_child(&ir.exprs, at, &mut |child| {
            stack.push((child, inlined, operands_are_spliceable, crosses_finally))
        });
    }
    false
}

/// Whether a SPLICED body still suspends inside a `try` that produces a value, after normalization.
///
/// Such an arm keeps the call's raw `Object` result and stores it straight into the arm's local,
/// which the emitted frame then describes as a scalar. The value-`try` desugar the spliced body
/// gets (`desugar_spliced_value_try`) binds the arms of a `try` that is the body's own value or a
/// statement's operand; one nested deeper in an expression (`1 + try { … }`) is not reached, and a
/// machine must not claim it.
pub(crate) fn suspends_in_a_value_try(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> bool {
    let mut stack = vec![(body, false)];
    let mut seen = HashSet::new();
    while let Some((at, spliced)) = stack.pop() {
        if !seen.insert((at, spliced)) {
            continue;
        }
        match &ir.exprs[at as usize] {
            IrExpr::Lambda {
                captures,
                inline_body,
                ..
            } => {
                for &capture in captures {
                    stack.push((capture, spliced));
                }
                if let Some(&inner) = inline_body.as_ref() {
                    stack.push((inner, true));
                }
                continue;
            }
            IrExpr::Try { body, result, .. }
                if spliced
                    && *result != crate::types::Ty::Unit
                    && tail_value(ir, *body)
                        .is_some_and(|value| holds_a_suspension(ir, value, suspend_set)) =>
            {
                // Only when the VALUE is the suspension. A body that binds it first
                // (`try { val v = one(x); v }`) hands the arm an ordinary local, which is stored
                // with the adaptation the arm expects.
                return true;
            }
            _ => {}
        }
        for_each_child(&ir.exprs, at, &mut |child| stack.push((child, spliced)));
    }
    false
}

/// The expression a block yields, or the expression itself when it is not a block.
fn tail_value(ir: &IrFile, body: ExprId) -> Option<ExprId> {
    match &ir.exprs[body as usize] {
        IrExpr::Block { value, .. } => *value,
        _ => Some(body),
    }
}

/// Any suspension reachable in `body`, closure boundaries included — a conservative "this region
/// suspends" for deciding whether a shape is safe to claim.
fn holds_a_suspension(ir: &IrFile, body: ExprId, suspend_set: &HashSet<u32>) -> bool {
    let mut stack = vec![body];
    let mut seen = HashSet::new();
    while let Some(at) = stack.pop() {
        if !seen.insert(at) {
            continue;
        }
        if is_suspension_point(ir, at, suspend_set) {
            return true;
        }
        for_each_child(&ir.exprs, at, &mut |child| stack.push(child));
    }
    false
}

/// The implementation methods of the lambdas whose spliced bodies carry a suspension.
///
/// Such a lambda has two bodies: the template the splice consumes, and the standalone `invoke` the
/// emitter would otherwise write. Only the spliced one is given a continuation to pass — the
/// standalone copy cannot be, because it has no continuation of its own — so it must not be emitted.
/// It is dead in any case: every call to the lambda is spliced.
pub(crate) fn spliced_suspension_lambda_impls(
    ir: &IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
) -> Vec<u32> {
    let mut impls = Vec::new();
    let mut seen = HashSet::new();
    collect_impls(ir, body, suspend_set, &mut seen, &mut impls);
    impls
}

fn collect_impls(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    seen: &mut HashSet<ExprId>,
    out: &mut Vec<u32>,
) {
    collect(ir, expression, suspend_set, false, seen, out);
}

fn collect(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    spliceable: bool,
    seen: &mut HashSet<ExprId>,
    out: &mut Vec<u32>,
) {
    if !seen.insert(expression) {
        return;
    }
    if let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body: Some(inline_body),
        ..
    } = &ir.exprs[expression as usize]
    {
        // Only a lambda whose body is actually spliced loses its standalone `invoke`; one passed to
        // an ordinary function is a real closure and needs it.
        if !spliceable {
            for &capture in captures {
                collect(ir, capture, suspend_set, false, seen, out);
            }
            return;
        }
        let mut found = Vec::new();
        let mut inner = HashSet::new();
        walk(
            ir,
            *inline_body,
            suspend_set,
            true,
            false,
            false,
            &mut inner,
            &mut found,
        );
        if !found.is_empty() && !out.contains(impl_fn) {
            out.push(*impl_fn);
        }
        for &capture in captures {
            collect(ir, capture, suspend_set, false, seen, out);
        }
        collect(ir, *inline_body, suspend_set, false, seen, out);
        return;
    }
    let operands_are_spliceable = match &ir.exprs[expression as usize] {
        IrExpr::Call { .. } => calls_an_inline_function(ir, expression),
        _ => spliceable,
    };
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        collect(ir, child, suspend_set, operands_are_spliceable, seen, out);
    }
}

/// `inlined` records whether this expression is already inside some lambda's `inline_body`; only
/// then does a suspension count, because only then is it invisible to the IR machine.
fn walk_frame(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    inlined: bool,
    seen: &mut HashSet<ExprId>,
    found: &mut Vec<ExprId>,
) {
    walk(
        ir,
        expression,
        suspend_set,
        inlined,
        false,
        false,
        seen,
        found,
    );
}

/// `inlined`: this expression already sits inside a body that runs in this frame. `spliceable`: it
/// is an operand of an inline call, so a lambda here has its body spliced rather than realized as a
/// closure. `every`: record the suspensions this frame runs directly as well, not only the ones the
/// IR machine cannot see.
#[allow(clippy::too_many_arguments)]
fn walk(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    inlined: bool,
    spliceable: bool,
    every: bool,
    seen: &mut HashSet<ExprId>,
    found: &mut Vec<ExprId>,
) {
    if !seen.insert(expression) {
        return;
    }
    if (inlined || every) && is_suspension_point(ir, expression, suspend_set) {
        found.push(expression);
    }
    if let IrExpr::Lambda {
        captures,
        inline_body,
        ..
    } = &ir.exprs[expression as usize]
    {
        // The captures are evaluated by THIS frame, whatever the lambda is.
        for &capture in captures {
            walk(ir, capture, suspend_set, inlined, false, every, seen, found);
        }
        // A spliced body runs in this frame; a real closure's does not — and only the operand of an
        // inline call is spliced, however the lowering filled `inline_body` in.
        if let (true, Some(&body)) = (spliceable, inline_body.as_ref()) {
            walk(ir, body, suspend_set, true, false, every, seen, found);
        }
        return;
    }
    // A CALL decides this for its own operands; anything else — a coercion, a block, an argument
    // wrapper — passes the answer through, since the lambda it holds is still the operand of
    // whatever call encloses it.
    let operands_are_spliceable = match &ir.exprs[expression as usize] {
        IrExpr::Call { .. } => calls_an_inline_function(ir, expression),
        _ => spliceable,
    };
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        walk(
            ir,
            child,
            suspend_set,
            inlined,
            operands_are_spliceable,
            every,
            seen,
            found,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrIntrinsicSuspensionKind, IrIntrinsicSuspensionPoint};
    use crate::types::Ty;

    /// Mark `point` as a suspension the way an intrinsic one is recorded, so these tests need no
    /// function table.
    fn suspension(ir: &mut IrFile) -> ExprId {
        let point = ir.add_expr(IrExpr::UnitInstance);
        ir.intrinsic_suspension_points.insert(
            point,
            IrIntrinsicSuspensionPoint {
                result: Ty::Unit,
                kind: IrIntrinsicSuspensionKind::Safe,
            },
        );
        point
    }

    fn lambda(ir: &mut IrFile, captures: Vec<ExprId>, inline_body: Option<ExprId>) -> ExprId {
        ir.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures,
            sam: None,
            inline_body,
        })
    }

    /// A call to an `inline` function taking `argument` — the only position whose lambda is spliced.
    fn inline_call(ir: &mut IrFile, argument: ExprId) -> ExprId {
        ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Static {
                owner: crate::types::TypeName::from("LibKt"),
                name: "with".to_string(),
                descriptor: "(Lkotlin/jvm/functions/Function0;)Ljava/lang/Object;".to_string(),
                inline: InlineKind::CanInline,
            },
            dispatch_receiver: None,
            args: vec![argument],
        })
    }

    #[test]
    fn a_suspension_in_this_frame_is_not_one_the_ir_machine_misses() {
        let mut ir = IrFile::default();
        let point = suspension(&mut ir);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![point],
            value: None,
        });
        assert!(spliced_inline_suspensions(&ir, body, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_suspension_in_a_spliced_inline_body_is_found() {
        let mut ir = IrFile::default();
        let point = suspension(&mut ir);
        let lam = lambda(&mut ir, Vec::new(), Some(point));
        let call = inline_call(&mut ir, lam);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![call],
            value: None,
        });
        assert_eq!(
            spliced_inline_suspensions(&ir, body, &HashSet::new()),
            [point]
        );
    }

    /// A lambda with no `inline_body` is a real closure: its body belongs to a generated `invoke`
    /// and is not an expression child at all, so nothing about it reaches this frame. Its captures
    /// still do — and those the IR machine already sees.
    #[test]
    fn the_captures_of_a_lambda_belong_to_the_enclosing_frame() {
        for spliced in [false, true] {
            let mut ir = IrFile::default();
            let capture = suspension(&mut ir);
            let inner = suspension(&mut ir);
            let lam = lambda(&mut ir, vec![capture], spliced.then_some(inner));
            let call = inline_call(&mut ir, lam);
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![call],
                value: None,
            });
            let found = spliced_inline_suspensions(&ir, body, &HashSet::new());
            assert!(
                !found.contains(&capture),
                "a capture is evaluated by this frame and is already visible to the IR machine"
            );
            assert_eq!(found.contains(&inner), spliced);
        }
    }

    #[test]
    fn nested_spliced_bodies_are_all_in_this_frame() {
        let mut ir = IrFile::default();
        let inner_point = suspension(&mut ir);
        let inner_call = {
            let inner = lambda(&mut ir, Vec::new(), Some(inner_point));
            inline_call(&mut ir, inner)
        };
        let outer = lambda(&mut ir, Vec::new(), Some(inner_call));
        let call = inline_call(&mut ir, outer);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![call],
            value: None,
        });
        assert_eq!(
            spliced_inline_suspensions(&ir, body, &HashSet::new()),
            [inner_point]
        );
    }

    #[test]
    fn a_direct_return_from_a_spliced_body_crosses_no_finally() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::UnitInstance);
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        let lam = lambda(&mut ir, Vec::new(), Some(returned));
        let body = inline_call(&mut ir, lam);
        assert!(!spliced_return_crosses_finally(&ir, body));
    }

    #[test]
    fn a_return_from_a_spliced_try_body_crosses_its_finally() {
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::UnitInstance);
        let returned = ir.add_expr(IrExpr::Return(Some(value)));
        let finalizer = ir.add_expr(IrExpr::UnitInstance);
        let protected = ir.add_expr(IrExpr::Try {
            body: returned,
            catches: Vec::new(),
            finally: Some(finalizer),
            result: Ty::Unit,
        });
        let lam = lambda(&mut ir, Vec::new(), Some(protected));
        let body = inline_call(&mut ir, lam);
        assert!(spliced_return_crosses_finally(&ir, body));
    }
}
