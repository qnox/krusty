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
        callee: Box::new(Callee::Local(FUNCTION)),
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
        false,
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
        ty: Ty::obj_name(crate::types::type_name("C")),
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
        false,
        OriginId::from_raw(0),
    )
    .expect("the body has a tail");
}

/// Whether the rewrite produced a LOOP STEP anywhere in the body.
///
/// A `continue` carrying the synthetic loop's label is written by `loop_step` and by nothing
/// else.
fn steps(ir: &IrFile) -> bool {
    ir.exprs.iter().any(|expression| {
        matches!(expression, IrExpr::Continue { label: Some(label) } if label == LOOP_LABEL)
    })
}

/// `return step(n)` whose value reaches the return through a coercion (an erased result, an
/// elvis operand widened to the declared type) is still a tail call. The step takes the
/// RETURN's place: no coercion is left holding it, and nothing is returned.
#[test]
fn a_self_call_under_a_coercion_replaces_its_return() {
    let mut ir = file();
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let coerced = ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: call,
        type_operand: Ty::nullable(Ty::Int),
    });
    let returned = ir.add_expr(IrExpr::Return(Some(coerced)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    let body = finish_tailrec_body(
        &mut ir,
        vec![returned, tail],
        Frame::of_body(
            FUNCTION,
            crate::fir_lower::BodySlots {
                dispatch_receiver: None,
                first_parameter: 0,
            },
            1,
        ),
        false,
        OriginId::from_raw(0),
    )
    .expect("the body has a tail");

    assert!(steps(&ir), "the coerced self call is the loop step");
    let mut reachable = vec![body];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = reachable.pop() {
        if seen.insert(expression) {
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| reachable.push(child));
        }
    }
    assert!(
        !seen.contains(&returned) && !seen.contains(&coerced) && !seen.contains(&call),
        "the step took the place of the return, its coercion and the call"
    );
}

/// `return step(n)!!`-shaped: a platform-narrowing assertion wraps the self call when a
/// sibling branch yields a platform type. The assertion is as transparent as a coercion —
/// the step takes its place, and nothing remains to assert.
#[test]
fn a_self_call_under_a_not_null_assertion_replaces_its_return() {
    let mut ir = file();
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let asserted = ir.add_expr(IrExpr::NotNullAssert {
        operand: call,
        check: crate::ir::NullCheck::Named("step(n)".to_string()),
    });
    let returned = ir.add_expr(IrExpr::Return(Some(asserted)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    let body = finish_tailrec_body(
        &mut ir,
        vec![returned, tail],
        Frame::of_body(
            FUNCTION,
            crate::fir_lower::BodySlots {
                dispatch_receiver: None,
                first_parameter: 0,
            },
            1,
        ),
        false,
        OriginId::from_raw(0),
    )
    .expect("the body has a tail");

    assert!(steps(&ir), "the asserted self call is the loop step");
    let mut reachable = vec![body];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = reachable.pop() {
        if seen.insert(expression) {
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| reachable.push(child));
        }
    }
    assert!(
        !seen.contains(&returned) && !seen.contains(&asserted) && !seen.contains(&call),
        "the step took the place of the return, its assertion and the call"
    );
}

/// An EXPLICIT `!!` (`NullCheck::Source`) is not transparent: kotlinc reports the call under it as
/// not a tail call and runs the check on every frame, so the sweep leaves the shape alone.
#[test]
fn a_self_call_under_an_explicit_bang_bang_is_left_alone() {
    let mut ir = file();
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let asserted = ir.add_expr(IrExpr::NotNullAssert {
        operand: call,
        check: crate::ir::NullCheck::Source,
    });
    let returned = ir.add_expr(IrExpr::Return(Some(asserted)));
    ir.checked_return_depths.insert(returned, 0);
    finish(&mut ir, vec![returned]);

    assert!(!steps(&ir), "an explicit `!!` keeps its call a call");
}

/// The control for the suspend guards below: a self-call whose argument is read back from a
/// spilled operand still steps when nothing in that operand suspends. Without it, a "left
/// alone" below could be the sweep failing to follow the spill rather than the guard firing.
#[test]
fn a_self_call_on_a_spilled_pure_argument_still_steps() {
    let mut ir = file();
    let operand = ir.add_expr(IrExpr::GetValue(0));
    let spill = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(operand),
        named: false,
    });
    let read = ir.add_expr(IrExpr::GetValue(1));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![read],
    });
    let block = ir.add_expr(IrExpr::Block {
        stmts: vec![spill],
        value: Some(call),
    });
    let returned = ir.add_expr(IrExpr::Return(Some(block)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![returned, tail]);

    assert!(steps(&ir), "a pure spilled argument does not stop the step");
}

/// A `tailrec suspend fun` whose self-call's stepped argument evaluation SUSPENDS stays a
/// call. The spill is the shape the call lowering produces for exactly this case:
/// `{ t = escapeChar(c); return step(t) }`. kotlinc re-invokes this shape through the
/// continuation too, and krusty's state machine cannot resume into the middle of the turn's
/// argument reassignment — stepping it miscompiled at run time.
#[test]
fn a_self_call_whose_spilled_argument_suspends_stays_a_call() {
    let mut ir = file();
    let suspendee = ir.add_fun(IrFunction {
        name: "escapeChar".to_string(),
        params: Vec::new(),
        ret: Ty::Int,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.suspend_funs.push(suspendee);
    let suspension = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(suspendee)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let spill = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(suspension),
        named: false,
    });
    let read = ir.add_expr(IrExpr::GetValue(1));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![read],
    });
    let block = ir.add_expr(IrExpr::Block {
        stmts: vec![spill],
        value: Some(call),
    });
    let returned = ir.add_expr(IrExpr::Return(Some(block)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![returned, tail]);

    assert!(
        !steps(&ir),
        "a stepped argument that suspends keeps the call a call"
    );
}

/// The same decline when the suspension is recorded per expression instead of by callee — a
/// CROSS-UNIT suspend call, whose callee has no local `FunId` for `suspend_funs` to hold. Its
/// `suspend_calls` entry is written as the call site lowers, before the sweep runs.
#[test]
fn a_self_call_whose_argument_suspends_across_a_unit_boundary_stays_a_call() {
    let mut ir = file();
    let suspension = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    ir.suspend_calls.insert(suspension, Ty::Int);
    let nested = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![suspension],
    });
    let returned = ir.add_expr(IrExpr::Return(Some(nested)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![returned, tail]);

    assert!(
        !steps(&ir),
        "a per-expression suspend record in the argument region keeps the call a call"
    );
}

/// A suspension the body runs as its OWN statement is not the step's argument region: the
/// state machine handles a suspension inside the loop, so the self-call still steps.
#[test]
fn a_suspend_statement_before_the_tail_call_does_not_stop_the_step() {
    let mut ir = file();
    let suspendee = ir.add_fun(IrFunction {
        name: "mark".to_string(),
        params: Vec::new(),
        ret: Ty::Int,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.suspend_funs.push(suspendee);
    let suspension = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(suspendee)),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let named = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(suspension),
        named: true,
    });
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let returned = ir.add_expr(IrExpr::Return(Some(call)));
    ir.checked_return_depths.insert(returned, 0);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![named, returned, tail]);

    assert!(
        steps(&ir),
        "a suspension outside the stepped argument region keeps the loop"
    );
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
        ty: Ty::obj_name(crate::types::type_name("C")),
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
        false,
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
fn a_bare_return_retargets_the_block_edge_without_retyping_the_selected_call() {
    let mut ir = file();
    ir.functions[FUNCTION as usize].ret = Ty::Unit;
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    ir.logical_types.insert(call, Ty::Unit);
    let returned = ir.add_expr(IrExpr::Return(None));
    let nested = ir.add_expr(IrExpr::Block {
        stmts: vec![call, returned],
        value: None,
    });
    let tail = ir.add_expr(IrExpr::Return(None));

    finish(&mut ir, vec![nested, tail]);

    let IrExpr::Block { stmts, value: None } = ir.expr(nested) else {
        panic!("the nested statement block remains")
    };
    assert_eq!(stmts.len(), 2);
    let step = stmts[0];
    assert_ne!(step, call);
    assert!(matches!(ir.expr(call), IrExpr::Call { .. }));
    assert_eq!(ir.logical_types.get(&call), Some(&Ty::Unit));
    assert!(matches!(
        ir.fir_origins.get(&step),
        Some(IrNodeOrigin::Synthetic {
            kind: SyntheticOriginKind::GeneratedControlFlow,
            ..
        })
    ));
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
        steps(&ir),
        "a uniquely owned `return` of a self call is the loop step"
    );
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

    let IrExpr::Block { stmts, .. } = ir.expr(owned) else {
        panic!("the owning block remains")
    };
    assert_ne!(
        stmts[0], returned,
        "one path to the `return` is the licence to replace it where it stands"
    );
    assert!(steps(&ir));
    assert!(depths_describe_returns(&ir));
}

/// An inline lambda implemented by a function taking `captures` values, capturing them and
/// holding `body` as its prepared inline template.
fn inline_lambda(ir: &mut IrFile, captures: Vec<ExprId>, body: ExprId) -> ExprId {
    let impl_fn = ir.add_fun(IrFunction {
        name: "step$lambda".to_string(),
        params: vec![Ty::Int; captures.len()],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    // The lambda producer's boundary: every parameter is a capture, before its own (none).
    ir.lambda_own_params_from.insert(
        impl_fn,
        u32::try_from(captures.len()).expect("test capture count fits u32"),
    );
    ir.add_expr(IrExpr::Lambda {
        impl_fn,
        arity: 0,
        captures,
        sam: None,
        inline_body: Some(body),
    })
}

#[test]
fn a_lambdas_capture_is_swept_in_this_frame() {
    // A lambda's captures are evaluated by THIS function, so a `return` among them is this
    // function's tail position.
    let mut ir = file();
    let capture = returned_self_call(&mut ir);
    ir.checked_return_depths.insert(capture, 0);
    let body = ir.add_expr(IrExpr::UnitInstance);
    let lambda = inline_lambda(&mut ir, vec![capture], body);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![lambda, tail]);

    assert!(
        matches!(ir.expr(capture), IrExpr::Block { .. }),
        "a capture expression is this function's and is swept"
    );
    assert!(depths_describe_returns(&ir));
}

#[test]
fn a_return_of_the_function_in_an_inline_body_steps_through_the_lambdas_capture() {
    // The template was prepared for its boundary: a depth-zero return in it is the function's
    // (a non-local return). The body numbers its slots from the lambda's frame, so the step
    // writes the capture that holds `n` (lambda slot 0) and takes its temporary above the
    // lambda's own slots, then continues the function's loop.
    let mut ir = file();
    let captured = ir.add_expr(IrExpr::GetValue(0));
    let inner = returned_self_call(&mut ir);
    ir.checked_return_depths.insert(inner, 0);
    let lambda = inline_lambda(&mut ir, vec![captured], inner);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![lambda, tail]);

    let IrExpr::Block { stmts, value: None } = ir.expr(inner).clone() else {
        panic!(
            "the inline body's return becomes a loop step: {:?}",
            ir.expr(inner)
        );
    };
    let shapes = stmts
        .iter()
        .map(|&statement| ir.expr(statement).clone())
        .collect::<Vec<_>>();
    assert!(
        matches!(
            shapes.as_slice(),
            [
                IrExpr::Variable { index: 1, .. },
                IrExpr::SetValue { var: 0, .. },
                IrExpr::Continue { label: Some(label) },
            ] if label == LOOP_LABEL
        ),
        "{shapes:?}"
    );
    assert!(depths_describe_returns(&ir));
}

#[test]
fn an_inline_body_that_does_not_capture_a_parameter_captures_it_for_the_step() {
    // `act { val local = 1; return step(local) }`: the lambda reads no parameter, but the step
    // writes `n`. kotlinc's lowering runs before the lambda captures anything, so the write makes
    // the lambda capture `n`; here the lambda gains that capture after its own, its local moves
    // up past it, and the step writes the capture and continues the function's loop.
    let mut ir = file();
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let local = ir.add_expr(IrExpr::Variable {
        index: 0,
        ty: Ty::Int,
        init: Some(one),
        named: true,
    });
    let argument = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(FUNCTION)),
        dispatch_receiver: None,
        args: vec![argument],
    });
    let inner = ir.add_expr(IrExpr::Return(Some(call)));
    ir.checked_return_depths.insert(inner, 0);
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![local, inner],
        value: None,
    });
    let lambda = inline_lambda(&mut ir, Vec::new(), body);
    let tail = ir.add_expr(IrExpr::Return(None));
    finish(&mut ir, vec![lambda, tail]);

    let IrExpr::Lambda {
        impl_fn, captures, ..
    } = ir.expr(lambda).clone()
    else {
        panic!("the lambda stays a lambda");
    };
    let captured = captures
        .iter()
        .map(|&capture| ir.expr(capture).clone())
        .collect::<Vec<_>>();
    assert!(
        matches!(captured.as_slice(), [IrExpr::GetValue(0)]),
        "{captured:?}"
    );
    assert_eq!(ir.functions[impl_fn as usize].params, [Ty::Int]);
    assert_eq!(
        ir.lambda_own_params_from.get(&impl_fn),
        Some(&1),
        "the new capture extends the lambda's capture prefix"
    );
    assert!(matches!(ir.expr(local), IrExpr::Variable { index: 1, .. }));
    let IrExpr::Block { stmts, .. } = ir.expr(body).clone() else {
        panic!("the lambda's body stays a block");
    };
    let IrExpr::Block { stmts, value: None } = ir.expr(stmts[1]).clone() else {
        panic!(
            "the inline body's return becomes a loop step: {:?}",
            ir.expr(stmts[1])
        );
    };
    let shapes = stmts
        .iter()
        .map(|&statement| ir.expr(statement).clone())
        .collect::<Vec<_>>();
    assert!(
        matches!(
            shapes.as_slice(),
            [
                IrExpr::Variable { index: 2, .. },
                IrExpr::SetValue { var: 0, .. },
                IrExpr::Continue { label: Some(label) },
            ] if label == LOOP_LABEL
        ),
        "{shapes:?}"
    );
    let IrExpr::Variable {
        init: Some(next), ..
    } = shapes[0]
    else {
        unreachable!("matched above")
    };
    assert!(matches!(ir.expr(next), IrExpr::GetValue(1)));
    assert!(depths_describe_returns(&ir));
}

#[test]
fn a_return_of_the_lambda_in_an_inline_body_is_not_a_tail() {
    // Only a depth-zero return is the function's; one lowering generated in the template has no
    // depth and is the lambda's own.
    let mut ir = file();
    let captured = ir.add_expr(IrExpr::GetValue(0));
    let inner = returned_self_call(&mut ir);
    let lambda = inline_lambda(&mut ir, vec![captured], inner);
    let tail = returned_self_call(&mut ir);
    finish(&mut ir, vec![lambda, tail]);

    assert!(matches!(ir.expr(inner), IrExpr::Return(Some(_))));
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

/// A lambda's inline body numbers its slots from its own frame, so its slot 1 is not this
/// function's slot 1. The outer frame holds an unreassigned temporary in slot 1; the lambda
/// declares its own temporary in slot 2 and reassigns its own slot 1, and uses slot 9.
#[test]
fn slot_inventories_stay_in_this_frame() {
    let mut ir = file();
    let seed = ir.add_expr(IrExpr::GetValue(0));
    let outer = ir.add_expr(IrExpr::Variable {
        index: 1,
        ty: Ty::Int,
        init: Some(seed),
        named: false,
    });
    let inner_seed = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    let inner = ir.add_expr(IrExpr::Variable {
        index: 2,
        ty: Ty::Int,
        init: Some(inner_seed),
        named: false,
    });
    let inner_value = ir.add_expr(IrExpr::GetValue(9));
    let overwrite = ir.add_expr(IrExpr::SetValue {
        var: 1,
        value: inner_value,
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![inner, overwrite],
        value: None,
    });
    let lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: FUNCTION,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let roots = vec![outer, lambda];
    let frame = Frame::of_body(
        FUNCTION,
        crate::fir_lower::BodySlots {
            dispatch_receiver: None,
            first_parameter: 0,
        },
        1,
    );

    assert_eq!(
        fixed_temporaries(&ir, &roots),
        crate::fx_hash::HashSet::from_iter([1])
    );
    assert_eq!(first_free_slot(&ir, &roots, &frame), 2);
}
