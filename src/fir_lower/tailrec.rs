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
    mut frame: Frame,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    let result = ir.functions[frame.function as usize].ret;
    collect_this_slots(ir, &roots, &mut frame);
    let frame = &frame;
    let tail = roots
        .pop()
        .ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
    let tail = tail_value(ir, tail, frame, result, origin)?;
    roots.push(tail);
    // The body's tail is one tail position; a `return` is another, wherever it stands, because
    // nothing of this function runs after one. `tail_value` reads the first off the body's shape,
    // and this sweeps the rest out of the whole body — after the rebuild above, so a tail the
    // rebuild already turned into a loop step is not visited a second time.
    rewrite_returned_tail_calls(ir, &roots, frame, result, origin)?;
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

/// Find every value slot that holds THIS frame's instance.
///
/// A member call does not read `this` at the call node. The lowering spills the receiver and each
/// argument into a generated temporary first, so the shape reaching the rewrite is
/// `{ t0 = this; t1 = n - 1; t2 = acc; this.f(t0, t1, t2) }` with the call reading `GetValue(t0)` —
/// evaluation order being the point of the spill. A receiver test that demanded the literal
/// receiver slot would therefore answer "different instance" for every member self-call.
///
/// So the aliases are followed: a generated, unnamed `Variable` initialized from a slot already
/// known to hold this instance holds it too, to a fixed point. Only `named: false` temporaries
/// qualify — a source `val` is the programmer's and the rewrite has no business assuming what stays
/// in it — and a slot the body ever reassigns is dropped, so an alias that is true at one point in
/// the body cannot be relied on at another.
///
/// Being conservative here costs nothing but a missed rewrite: a self-call this does not recognize
/// stays an ordinary call, which is what the program did before.
fn collect_this_slots(ir: &IrFile, roots: &[ExprId], frame: &mut Frame) {
    if frame.receiver.is_none() {
        return;
    }
    let mut bindings: Vec<(u32, ExprId)> = Vec::new();
    let mut assigned: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Variable {
                index,
                init: Some(init),
                named: false,
                ..
            } => bindings.push((*index, *init)),
            IrExpr::SetValue { var, .. } => {
                assigned.insert(*var);
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    loop {
        let mut grew = false;
        for &(slot, init) in &bindings {
            if frame.this_slots.contains(&slot) || assigned.contains(&slot) {
                continue;
            }
            if matches!(ir.expr(init), IrExpr::GetValue(source) if frame.this_slots.contains(source))
            {
                frame.this_slots.insert(slot);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    frame.this_slots.retain(|slot| !assigned.contains(slot));
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
    frame: &Frame,
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
                let rebuilt = tail_value(ir, expression, frame, result, origin)?;
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

/// The frame a `tailrec` loop steps: which function a self-call must name, and which value slots
/// hold the parameters it reassigns.
///
/// The slots are not always `0..count`. A body's dispatch receiver sits at `capture_count` with the
/// parameters after it, so a MEMBER's first parameter is one above its `this` — reassigning from
/// zero there would write the receiver and shift every argument by one.
pub(super) struct Frame {
    function: FunId,
    /// Value slot of the first parameter.
    first_parameter: u32,
    count: usize,
    /// A member's `this` slot. `None` for a static function, including an extension: an extension
    /// receiver is an ordinary parameter inside `first_parameter..count`, which is what lets a
    /// self-call re-bind it by stepping.
    receiver: Option<u32>,
    /// Slots that hold this frame's own instance — `receiver` plus the generated temporaries a call
    /// spills it into. Empty for a static frame. Filled by [`finish_tailrec_body`].
    this_slots: std::collections::HashSet<u32>,
}

impl Frame {
    /// The frame a body reported, for the function it lowers and its parameter count.
    ///
    /// The slots come from [`super::BodySlots`] and are NOT recomputed here. Deriving them a second
    /// time is what this constructor exists to prevent: a static frame is not always slot zero and a
    /// member's parameters are not always `this + 1` — captures, a local class's constructor
    /// captures and its context values all take slots first, and only the lowering knows how many.
    pub(super) fn of_body(function: FunId, slots: super::BodySlots, count: usize) -> Self {
        Self {
            function,
            first_parameter: slots.first_parameter,
            count,
            receiver: slots.dispatch_receiver,
            this_slots: slots.dispatch_receiver.into_iter().collect(),
        }
    }
}

/// Whether `call` is this function calling itself with its whole parameter list — the only shape
/// the loop can step. A partial list is somebody else's overload, or a call that leaves a defaulted
/// argument for the callee to fill, and neither reassigns everything the next turn reads.
///
/// For a MEMBER the frame is the instance too: `count(n - 1)` on `this` is the same frame and steps,
/// while `Other().count(n - 1)` is a different one and has to stay a call.
fn is_self_call(ir: &IrFile, call: ExprId, frame: &Frame) -> bool {
    match ir.expr(call) {
        IrExpr::Call {
            callee: Callee::Local(target),
            dispatch_receiver: None,
            args,
        } => frame.receiver.is_none() && *target == frame.function && args.len() == frame.count,
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => {
            if frame.receiver.is_none() {
                return false;
            }
            ir.classes
                .get(*class as usize)
                .and_then(|class| class.methods.get(*index as usize))
                == Some(&frame.function)
                && matches!(ir.expr(*receiver), IrExpr::GetValue(slot) if frame.this_slots.contains(slot))
                && args.len() == frame.count
                && args.iter().all(Option::is_some)
        }
        _ => false,
    }
}

/// The arguments a self-call passes, in parameter order. `call` must satisfy [`is_self_call`],
/// which is what makes the `MethodCall` unwrap total.
fn self_call_arguments(ir: &IrFile, call: ExprId) -> Vec<ExprId> {
    match ir.expr(call) {
        IrExpr::Call { args, .. } => args.clone(),
        IrExpr::MethodCall { args, .. } => args
            .iter()
            .map(|argument| argument.expect("a self call passes every parameter"))
            .collect(),
        _ => unreachable!("a self call is a call"),
    }
}

/// The loop step a self-call becomes: every parameter reassigned, then `continue` to the synthetic
/// loop. `call` must satisfy [`is_self_call`].
///
/// The receiver is deliberately NOT reassigned. A static frame has none, and a member self-call is
/// the same frame only when it already dispatches on `this` — so the slot holding it is already
/// correct, and writing it would be a store with nothing to store.
/// Whether `expression` is a self call reached through nothing but value-carrying structure.
///
/// A representation coercion can stand between a tail position and the call that fills it: an
/// elvis lowers to `{ tmp = lhs; when { tmp == null -> rhs; else -> tmp } }`, and each arm is
/// coerced to the elvis's own type, so `return a ?: f(x)` puts the tail call under a
/// `TypeOp(ImplicitCoercion)`. The rewrite has to see through that wrapper to reach the call, and
/// then drop it: what replaces the call is a loop STEP, which ends in `continue` and yields no
/// value for a coercion to convert.
///
/// Dropping it is only sound where the whole expression becomes that step, which is what this
/// establishes — the call is the value of every block on the way down, so rewriting the tail
/// rewrites the lot. It deliberately does not look inside a `when` or a `try`: there the rewrite
/// turns SOME arms into steps and leaves others producing a value, and that value still needs its
/// coercion.
fn coerced_self_call(ir: &IrFile, expression: ExprId, frame: &Frame) -> bool {
    match ir.expr(expression) {
        IrExpr::Call { .. } | IrExpr::MethodCall { .. } => is_self_call(ir, expression, frame),
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            let value = *value;
            let _ = stmts;
            coerced_self_call(ir, value, frame)
        }
        IrExpr::Block { stmts, value: None } => stmts
            .last()
            .is_some_and(|tail| coerced_self_call(ir, *tail, frame)),
        _ => false,
    }
}

/// Whether a self call sits anywhere in the tail STRUCTURE of `expression`.
///
/// Broader than [`coerced_self_call`]: this looks through a `when`'s arms as well, because a
/// coercion over a `when` can be distributed into them. It deliberately stops at a `try` for the
/// reason the sweep does — a `finally` runs after the value is produced — and at a lambda, whose
/// body is not this function's tail.
fn reaches_self_call(ir: &IrFile, expression: ExprId, frame: &Frame) -> bool {
    match ir.expr(expression) {
        IrExpr::Call { .. } | IrExpr::MethodCall { .. } => is_self_call(ir, expression, frame),
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            let value = *value;
            let _ = stmts;
            reaches_self_call(ir, value, frame)
        }
        IrExpr::Block { stmts, value: None } => stmts
            .last()
            .is_some_and(|tail| reaches_self_call(ir, *tail, frame)),
        IrExpr::When { branches } => branches
            .clone()
            .iter()
            .any(|(_, branch)| reaches_self_call(ir, *branch, frame)),
        IrExpr::Return(Some(value)) => reaches_self_call(ir, *value, frame),
        // Only a coercion to the SAME type is looked through, because that is the one
        // `distribute_coercion` collapses. Looking through one it keeps would let this arm fire on
        // a node distribution rebuilds unchanged, which does not terminate.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if *type_operand == ir.functions[frame.function as usize].ret => {
            reaches_self_call(ir, *arg, frame)
        }
        _ => false,
    }
}

/// Push a representation coercion down to the values it actually converts.
///
/// `coerce(when { a -> x; else -> y })` and `when { a -> coerce(x); else -> coerce(y) }` answer the
/// same thing, because the coercion is a pure function of the value and neither arm's effects move.
/// The same holds for a block: its statements run either way, and only its value is converted.
///
/// Doing this is what lets a tail call under a CHAIN of coercions be found — `a ?: b ?: c` coerces
/// the inner elvis and then coerces that again, so the call filling the tail sits under two
/// wrappers and a `when` in between. Distributing leaves each wrapper on the leaf it converts,
/// where a leaf that is the tail call is recognised and the wrapper dropped with it, and every
/// other leaf keeps the conversion it needs.
fn distribute_coercion(
    ir: &mut IrFile,
    expression: ExprId,
    target: &Ty,
    origin: OriginId,
) -> ExprId {
    match ir.expr(expression).clone() {
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            let value = distribute_coercion(ir, value, target, origin);
            generated(
                ir,
                IrExpr::Block {
                    stmts,
                    value: Some(value),
                },
                origin,
            )
        }
        IrExpr::Block {
            mut stmts,
            value: None,
        } if !stmts.is_empty() => {
            let tail = stmts.pop().expect("checked non-empty just above");
            let tail = distribute_coercion(ir, tail, target, origin);
            stmts.push(tail);
            generated(ir, IrExpr::Block { stmts, value: None }, origin)
        }
        IrExpr::When { branches } => {
            let branches = branches
                .into_iter()
                .map(|(condition, branch)| {
                    (condition, distribute_coercion(ir, branch, target, origin))
                })
                .collect();
            generated(ir, IrExpr::When { branches }, origin)
        }
        IrExpr::Return(Some(value)) => {
            let value = distribute_coercion(ir, value, target, origin);
            generated(ir, IrExpr::Return(Some(value)), origin)
        }
        // The same conversion twice is the conversion once, so the outer one is dropped and the
        // walk continues past the inner. A chained elvis produces exactly this: each link coerces
        // its own result to the type they all share.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if type_operand == *target => distribute_coercion(ir, arg, target, origin),
        _ => generated(
            ir,
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: expression,
                type_operand: target.clone(),
            },
            origin,
        ),
    }
}

fn loop_step(ir: &mut IrFile, call: ExprId, frame: &Frame, origin: OriginId) -> IrExpr {
    let args = self_call_arguments(ir, call);
    let mut updates = Vec::with_capacity(frame.count + 1);
    for (parameter, value) in args.into_iter().enumerate() {
        let parameter =
            u32::try_from(parameter).expect("tailrec parameter count exceeds packed value ids");
        updates.push(generated(
            ir,
            IrExpr::SetValue {
                var: frame.first_parameter + parameter,
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
    frame: &Frame,
    result: Ty,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    match ir.expr(expression).clone() {
        IrExpr::Call { .. } | IrExpr::MethodCall { .. } if is_self_call(ir, expression, frame) => {
            let step = loop_step(ir, expression, frame, origin);
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
                stmts.push(tail_value(ir, tail, frame, result, origin)?);
            } else {
                stmts.push(generated(ir, IrExpr::Return(None), origin));
            }
            Ok(generated(ir, IrExpr::Block { stmts, value: None }, origin))
        }
        IrExpr::Block { mut stmts, value } => {
            if let Some(value) = value {
                stmts.push(tail_value(ir, value, frame, result, origin)?);
            } else if let Some(tail) = stmts.pop() {
                // A block-bodied function carries its explicit `return` (or Unit tail statement)
                // as the final statement rather than as the block value. It is still the sole tail
                // position of this block. The statements before it are tail positions only
                // where they `return`.
                stmts.push(tail_value(ir, tail, frame, result, origin)?);
            } else if result == Ty::Unit {
                stmts.push(generated(ir, IrExpr::Return(None), origin));
            } else {
                return Err(FirLoweringFailure::MissingBodyResult { origin });
            }
            Ok(generated(ir, IrExpr::Block { stmts, value: None }, origin))
        }
        IrExpr::Return(Some(value)) => tail_value(ir, value, frame, result, origin),
        // A LOOP is not a value and has no tail position of its own: what leaves the function from
        // inside one is a `return`, which the sweep reaches wherever it stands. Wrapping the loop
        // in a `return` instead would return the loop — which is what a body ending in
        // `while (true) { … return f(x) }` used to compile to, and the verifier said so.
        IrExpr::While { .. } => Ok(expression),
        IrExpr::Return(None) => Ok(expression),
        // The coercion a tail position puts on the value that fills it. Its argument is the tail
        // call itself (see `coerced_self_call`), so what this returns is the loop step, and the
        // coercion goes with the call it was converting. Restricted to a coercion to THIS
        // function's return type: that is the one a tail position inserts, and it is the one whose
        // disappearance cannot change what the function answers.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ref type_operand,
        } if *type_operand == result && coerced_self_call(ir, arg, frame) => {
            tail_value(ir, arg, frame, result, origin)
        }
        // The same coercion over STRUCTURE rather than directly over the call: `a ?: b ?: c`
        // wraps the inner elvis, so the tail call sits under two coercions with a `when` between
        // them. Distributing puts each coercion on the leaf it converts, and the arm above then
        // recognises the leaf that is the call.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ref type_operand,
        } if *type_operand == result && reaches_self_call(ir, arg, frame) => {
            let target = type_operand.clone();
            let distributed = distribute_coercion(ir, arg, &target, origin);
            tail_value(ir, distributed, frame, result, origin)
        }
        IrExpr::When { branches } => {
            let branches = branches
                .into_iter()
                .map(|(condition, branch)| {
                    Ok((condition, tail_value(ir, branch, frame, result, origin)?))
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
        finish_tailrec_body(
            ir,
            roots,
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: None,
                    first_parameter: 0,
                },
                1,
            ),
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");
    }

    /// A one-parameter MEMBER `tailrec fun step(n: Int): Int` on a class that declares it, with
    /// `this` in slot 0 and the parameter in slot 1 — the layout a member body is lowered with.
    fn member_file() -> IrFile {
        let mut ir = file();
        ir.functions[FUNCTION as usize].is_static = false;
        ir.functions[FUNCTION as usize].dispatch_receiver = Some(crate::types::type_name("C"));
        let class = ir.add_class(crate::ir::IrClass::synthetic(crate::types::type_name("C")));
        ir.classes[class as usize].methods.push(FUNCTION);
        ir
    }

    /// `{ t = <receiver>; this.step(n) }` returned — the shape a member call is lowered into, with
    /// the receiver spilled into temporary slot `temp`.
    fn returned_member_call(ir: &mut IrFile, receiver: ExprId, temp: u32) -> ExprId {
        let binding = ir.add_expr(IrExpr::Variable {
            index: temp,
            ty: Ty::Obj(crate::types::type_name("C"), &[]),
            init: Some(receiver),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(temp));
        let argument = ir.add_expr(IrExpr::GetValue(1));
        let call = ir.add_expr(IrExpr::MethodCall {
            class: 0,
            index: 0,
            receiver: read,
            args: vec![Some(argument)],
        });
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![binding],
            value: Some(call),
        });
        ir.add_expr(IrExpr::Return(Some(block)))
    }

    fn finish_member(ir: &mut IrFile, roots: Vec<ExprId>) {
        finish_tailrec_body(
            ir,
            roots,
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: Some(0),
                    first_parameter: 1,
                },
                1,
            ),
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");
    }

    /// Whether the rewrite produced a LOOP STEP anywhere in the body.
    ///
    /// The node a `return` leaves behind is not the signal: `rewrite_returned_tail_calls` replaces a
    /// block-shaped `return` with the rebuilt block whether or not anything stepped, so "still a
    /// `Return`" answers a question about shape rather than about the rewrite. A `continue` carrying
    /// the synthetic loop's label is written by `loop_step` and by nothing else.
    fn steps(ir: &IrFile) -> bool {
        ir.exprs.iter().any(|expression| {
            matches!(expression, IrExpr::Continue { label: Some(label) } if label == LOOP_LABEL)
        })
    }

    /// The control for the two member tests below: a receiver spilled from `this` is still `this`,
    /// so the call is the same frame and steps. No Kotlin source reaches the rewrite WITHOUT this
    /// spill, so a member rewrite that did not follow the alias would never fire at all.
    #[test]
    fn a_receiver_spilled_from_this_is_still_this() {
        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(0));
        let returned = returned_member_call(&mut ir, this, 2);
        ir.checked_return_depths.insert(returned, 0);
        // A trailing statement keeps `returned` off the body's last root, so what rewrites it is
        // the SWEEP, in place — the same path the static control above takes.
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![returned, tail]);

        assert!(
            steps(&ir),
            "a self call on a temporary holding `this` is the loop step"
        );
    }

    /// A temporary the body REASSIGNS is not a reliable alias: what it held where the alias was
    /// established is not what it holds at the call. The slot is dropped and the call keeps
    /// recursing, which is the answer the program had before.
    #[test]
    fn a_reassigned_temporary_is_not_an_alias_for_this() {
        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(0));
        let returned = returned_member_call(&mut ir, this, 2);
        ir.checked_return_depths.insert(returned, 0);
        let other = ir.add_expr(IrExpr::GetValue(1));
        let overwrite = ir.add_expr(IrExpr::SetValue {
            var: 2,
            value: other,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![overwrite, returned, tail]);

        assert!(
            !steps(&ir),
            "a reassigned slot is not followed, so the call stays a call"
        );
    }

    /// The loop writes the slots the BODY reported, not slots derived from the receiver.
    ///
    /// `first_parameter` is not always `0` and not always `this + 1`. Captures, a local class's
    /// constructor captures and its context values all take slots before the parameters, so a body
    /// can put `this` at 3 and its first parameter at 7. Re-deriving the layout here — the thing
    /// `BodySlots` exists to stop — would write slot 4 and shift every argument by three, which is
    /// a miscompile no source-level test in this file would show, because the shapes that produce a
    /// prefix are the ones that decline for other reasons today.
    ///
    /// So this asserts on the store the step emits: reported slot in, same slot out.
    #[test]
    fn the_loop_step_writes_the_reported_parameter_slots() {
        const RECEIVER: u32 = 3;
        const FIRST_PARAMETER: u32 = 7;

        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(RECEIVER));
        let binding = ir.add_expr(IrExpr::Variable {
            index: 11,
            ty: Ty::Obj(crate::types::type_name("C"), &[]),
            init: Some(this),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(11));
        let argument = ir.add_expr(IrExpr::GetValue(FIRST_PARAMETER));
        let call = ir.add_expr(IrExpr::MethodCall {
            class: 0,
            index: 0,
            receiver: read,
            args: vec![Some(argument)],
        });
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![binding],
            value: Some(call),
        });
        let returned = ir.add_expr(IrExpr::Return(Some(block)));
        ir.checked_return_depths.insert(returned, 0);
        let tail = ir.add_expr(IrExpr::Return(None));

        finish_tailrec_body(
            &mut ir,
            vec![returned, tail],
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: Some(RECEIVER),
                    first_parameter: FIRST_PARAMETER,
                },
                1,
            ),
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");

        assert!(steps(&ir), "the spilled receiver is still this frame");
        let written: Vec<u32> = ir
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::SetValue { var, .. } => Some(*var),
                _ => None,
            })
            .collect();
        assert_eq!(
            written,
            vec![FIRST_PARAMETER],
            "the step must write the reported first-parameter slot, and must not write the receiver"
        );
    }

    /// A call on an instance the frame did not supply is a different frame however it was reached,
    /// so it stays a call — this is what stops `Other().step(n)` from being stepped as if it were
    /// `this`.
    #[test]
    fn a_receiver_that_is_not_this_is_a_different_frame() {
        let mut ir = member_file();
        let other = ir.add_expr(IrExpr::GetValue(1));
        let returned = returned_member_call(&mut ir, other, 2);
        ir.checked_return_depths.insert(returned, 0);
        // A trailing statement keeps `returned` off the body's last root, so what rewrites it is
        // the SWEEP, in place — the same path the static control above takes.
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![returned, tail]);

        assert!(
            !steps(&ir),
            "a self call on another instance is not this frame"
        );
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
