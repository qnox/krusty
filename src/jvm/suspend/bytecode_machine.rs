//! The suspend functions whose state machine kotlinc's coroutine transformer builds from their
//! finished bytecode, as kotlinc does (see `docs/JVM_INLINE_BEFORE_CPS.md` §1a).
//!
//! Such a function keeps its body. It only takes the CPS signature: each suspension point passes
//! the function's own `$completion` on, and the returns box to `Object`. The emitter marks every
//! suspension point the way kotlinc's codegen does, and the transformer builds the machine when the
//! class is written. This pass creates the continuation class with the fields every machine has;
//! the spill fields and `@DebugMetadata` come from the transformer.
//!
//! The functions taken so far are top-level functions and class members whose suspension points
//! are all plain calls and which call no inline function; a call to a member, on any receiver, is
//! as plain as a call to a top-level function. An overridable member's machine is built in the
//! `$suspendImpl` its body moves to (`jvm::suspend_impls`). Suspend lambdas of that shape go through
//! `suspend_lambda`, which shares the eligibility and suspension collection here. An interface
//! default whose own suspension points include a `super` call is taken the same way: the machine
//! lives in its `$suspendImpl` and the super call is `invokespecial`. Other interface bodies, and
//! spliced inline bodies, keep the IR machine.

use std::collections::HashSet;

use super::cps::{
    spliced_inline_suspensions, TransformedMachine, TransformedSuspension,
};
use super::emission_facts::{ContinuationMetadata, MachineOutputs};
use super::spill_layout::{suspension_points_in_order, SpillLayout};
use super::{
    adopt_cps_signature, append_continuation, box_returns, build_continuation_class,
    ensure_tail_return, realize_coroutine_context, recorded_suspension_result, shift_locals,
    suspend_call_fid, value_class_suspension_result, EmitTimeMachines, MachineContext,
};
use crate::ir::{for_each_child, Callee, ExprId, IrExpr, IrFile, IrValueClassSuspendResult};
use crate::jvm::local_class_names::name_continuation;
use crate::types::Ty;

/// What a routed function needs from the rest of the pass.
pub(super) struct Route<'a, 'b, 'o> {
    pub(super) facade: &'a str,
    pub(super) suspend_set: &'a HashSet<u32>,
    pub(super) context: &'a MachineContext<'a>,
    pub(super) machines: &'b mut EmitTimeMachines,
    pub(super) outputs: &'b mut MachineOutputs<'o>,
}

/// What the transformer is asked to take: a named function, whose continuation is a class of its
/// own, or a suspend lambda's body, whose class is the continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Subject {
    NamedFunction,
    SuspendLambda,
}

/// Whether the transformer takes `fid`.
pub(super) enum Routed {
    Taken,
    NotEligible,
    /// Taken, but a call could not be given its continuation: the file cannot be compiled.
    Failed,
}

/// Route `fid`, whose body is `body`, to the transformer when it is one of the shapes it takes.
pub(super) fn route(
    ir: &mut IrFile,
    fid: u32,
    body: ExprId,
    mut route: Route<'_, '_, '_>,
) -> Routed {
    if eligible_points(ir, fid, body, &route, Subject::NamedFunction).is_none() {
        return Routed::NotEligible;
    }
    let Some(suspensions) = owned_suspensions(ir, fid, body, &mut route, Subject::NamedFunction)
    else {
        return Routed::Failed;
    };
    let function = &ir.functions[fid as usize];
    let name = function.name.clone();
    let declared_params = function.params.clone();
    let unit_return = route.context.orig_rets[fid as usize] == Ty::Unit;

    let fake_continuations = unintercepted_continuation_reads(ir, body);
    let completion = adopt_cps_signature(ir, fid);
    shift_locals(ir, body, completion);
    // `coroutineContext` is the context of the continuation kotlinc's transformer puts in place
    // of the fake one: the machine's own, or `$completion` when there is no suspension point.
    realize_coroutine_context(ir, body, IrExpr::CurrentContinuation);
    // A `suspendCoroutineUninterceptedOrReturn` block is given the continuation already: it reads
    // it as a fake one.
    let calls = suspensions
        .iter()
        .filter(|suspension| !ir.is_unintercepted_suspension(suspension.call));
    for suspension in calls.collect::<Vec<_>>() {
        let continuation = ir.add_expr(IrExpr::CurrentContinuation);
        // A generated call that enters its line only where its operands begin (a reference
        // carrier's `invoke`) enters it at the continuation it now reads too.
        if let Some(line) = ir.dispatch_line(suspension.call) {
            ir.expr_source_lines.insert(continuation, line);
        }
        if !append_continuation(
            ir,
            suspension.call,
            continuation,
            route.outputs.default_call_operands,
        ) {
            return Routed::Failed;
        }
    }
    if !box_returns(ir, body) {
        return Routed::Failed;
    }
    // kotlinc maps a `Unit` function's implicit return to the body's closing `}`.
    if let (Some(unit), Some(&close)) = (
        ensure_tail_return(ir, body, unit_return),
        ir.fn_close_lines.get(&fid),
    ) {
        ir.expr_source_lines.insert(unit, close);
    }

    // A member's continuation nests under its class, a top-level function's under the facade.
    let semantic_owner = ir.functions[fid as usize].dispatch_receiver;
    let (receiver, static_owner) = super::continuation_class::reentry_owners(ir, fid);
    let class_owner = semantic_owner.or(static_owner);
    let owner = class_owner.map_or_else(|| route.facade.to_string(), |owner| owner.render());
    let continuation_class = name_continuation(ir, fid, &owner, class_owner, route.facade);
    build_continuation_class(
        ir,
        &continuation_class,
        fid,
        &SpillLayout::default(),
        route.outputs.suspended_result_returns,
        receiver,
        static_owner,
        &declared_params,
    );
    let function = &ir.functions[fid as usize];
    // The continuation belongs to the method that carries the machine: an overridable member's is
    // the `$suspendImpl` its body moves to, which takes the receiver first.
    let (method, method_params) =
        match receiver.filter(|_| crate::jvm::suspend_impls::moves_to_suspend_impl(ir, fid)) {
            Some(owner) => (
                crate::jvm::suspend_impls::suspend_impl_name(&name),
                std::iter::once(Ty::obj_name(owner))
                    .chain(function.params.iter().copied())
                    .collect(),
            ),
            None => (name, function.params.clone()),
        };
    // The arrays the transformer computes replace these when the continuation class is written.
    route.outputs.continuation_metadata.insert(
        continuation_class.clone(),
        ContinuationMetadata {
            m: method.clone(),
            c: owner.replace('/', "."),
            v: 2,
            enclosing_class: owner,
            enclosing_method: method,
            enclosing_descriptor: crate::jvm::method_descriptors::ir_method_desc(
                &method_params,
                &function.ret,
            ),
            ..ContinuationMetadata::default()
        },
    );
    route.machines.record_transformed(
        fid,
        TransformedMachine {
            continuation_class,
            suspensions,
            lambda: None,
            fake_continuations,
        },
    );
    Routed::Taken
}

/// Give `body`, which the transformer takes, one node per use, and its suspension points in order
/// with each callee's declared result. `None` when a call could not be given its own copy.
pub(super) fn owned_suspensions(
    ir: &mut IrFile,
    fid: u32,
    body: ExprId,
    route: &mut Route<'_, '_, '_>,
    subject: Subject,
) -> Option<Vec<TransformedSuspension>> {
    // Common IR is a DAG: the CPS rewrites change nodes in place, so this body first owns one node
    // per use, as it does on the IR machine's path.
    for (source, target) in crate::ir::make_expression_children_unique_tracked(ir, body) {
        let IrExpr::Call { args, .. } = &ir.exprs[target as usize] else {
            continue;
        };
        if !route
            .outputs
            .default_call_operands
            .clone_call(source, target, args)
        {
            return None;
        }
    }
    let points = eligible_points(ir, fid, body, route, subject)?;
    Some(
        points
            .iter()
            .map(|&call| TransformedSuspension {
                call,
                result: match suspend_call_fid(ir, call, route.suspend_set) {
                    Some(callee) => route.context.orig_rets[callee as usize],
                    None => recorded_suspension_result(ir, call)
                        .expect("an eligible suspension point records its result"),
                },
            })
            .collect(),
    )
}

/// The suspension points of `fid` in order, when the transformer takes it as `subject`.
pub(super) fn eligible_points(
    ir: &IrFile,
    fid: u32,
    body: ExprId,
    route: &Route<'_, '_, '_>,
    subject: Subject,
) -> Option<Vec<ExprId>> {
    let function = &ir.functions[fid as usize];
    let top_level = function.is_static && function.dispatch_receiver.is_none();
    let suspend_set = route.suspend_set;
    let points = suspension_points_in_order(ir, body, suspend_set);
    // A class member: its machine stays in the member, or, for an overridable one, moves with its
    // body to `$suspendImpl`. An interface default is the same machine when one of its own
    // suspension points is a `super` call; the IR machine declines that invokespecial.
    let super_suspension = points.iter().any(|&call| {
        matches!(
            &ir.exprs[call as usize],
            IrExpr::Call {
                callee: Callee::Special { .. },
                ..
            }
        )
    });
    let class_member = !function.is_static
        && function.dispatch_receiver.is_some_and(|owner| {
            ir.classes.iter().any(|class| {
                class.fq_name_id() == owner && (!class.is_interface || super_suspension)
            })
        });
    // Shapes the transformer cannot take at all: a suspension spliced in from an inline body, or
    // a read of the function's own continuation other than a named function's
    // `suspendCoroutineUninterceptedOrReturn` block, which kotlinc realizes through a fake one.
    let body_declines: [(&dyn Fn() -> bool, &str); 2] = [
        (
            &|| !spliced_inline_suspensions(ir, body, suspend_set).is_empty(),
            "suspends in a spliced inline body",
        ),
        (
            &|| match subject {
                Subject::NamedFunction => reads_continuation_outside_unintercepted_blocks(ir, body),
                Subject::SuspendLambda => reads_current_continuation(ir, body),
            },
            "reads its own continuation",
        ),
    ];
    // Gates of the state machine itself: its spills, its continuation class and that class's
    // `@DebugMetadata`. A named function with no suspension point has none of them, as kotlinc's
    // transformer returns before building them; a suspend lambda's `invokeSuspend` always has its
    // machine.
    let machine_declines: [(&dyn Fn() -> bool, &str); 5] = [
        (
            &|| !route.context.null_out_dead_spills,
            "no spill clean-up in the runtime",
        ),
        (
            &|| subject == Subject::NamedFunction && !(top_level || class_member),
            "not top-level or a class member",
        ),
        (
            &|| subject == Subject::NamedFunction && ir.method_visibility(fid).is_private(),
            "private",
        ),
        (
            &|| !ir.fn_decl_lines.contains_key(&fid),
            "no declaration line",
        ),
        (
            &|| ir.jvm_suspend_impl_bodies.contains_key(&fid),
            "an interface body",
        ),
    ];
    let machine_declines: &[(&dyn Fn() -> bool, &str)] =
        if points.is_empty() && subject == Subject::NamedFunction {
            &[]
        } else {
            &machine_declines
        };
    // Checked in order, each only while every earlier one holds.
    let declined = body_declines
        .iter()
        .chain(machine_declines)
        .find_map(|(declines, reason)| declines().then_some(*reason));
    if let Some(reason) = declined {
        crate::trace_compiler!(
            "suspend",
            "transformer declines {}: {reason}",
            function.name
        );
        return None;
    }
    let plain_call = |call: ExprId| {
        // A suspend function value is `InvokeFunction`: the continuation is appended and the
        // invoke is marked like any other suspension the transformer takes.
        let direct = matches!(
            ir.exprs[call as usize],
            IrExpr::Call { .. } | IrExpr::MethodCall { .. } | IrExpr::InvokeFunction { .. }
        );
        let block = ir.is_unintercepted_suspension(call) && subject == Subject::NamedFunction;
        (direct && !ir.intrinsic_suspension_points.contains_key(&call) || block)
            // A boxed result arrives as the box on either path, which the call's own unbox
            // consumes; a carrier result arrives boxed only on resume, which the IR machine handles.
            && !matches!(
                value_class_suspension_result(ir, call, route.suspend_set),
                Some(IrValueClassSuspendResult::Carrier { .. })
            )
            && (suspend_call_fid(ir, call, route.suspend_set).is_some()
                || recorded_suspension_result(ir, call).is_some())
    };
    if !points.iter().all(|&call| plain_call(call)) {
        crate::trace_compiler!(
            "suspend",
            "transformer declines {}: a suspension point is not a plain call",
            function.name
        );
        return None;
    }
    Some(points)
}

/// Whether the body reads its own continuation outside a `suspendCoroutineUninterceptedOrReturn`
/// block, the one read the transformer takes.
fn reads_continuation_outside_unintercepted_blocks(ir: &IrFile, expression: ExprId) -> bool {
    if ir.is_unintercepted_suspension(expression) {
        return false;
    }
    if let IrExpr::CurrentContinuation = ir.exprs[expression as usize] {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, expression, &mut |child| {
        found = found || reads_continuation_outside_unintercepted_blocks(ir, child);
    });
    found
}

/// The continuation reads inside the body's `suspendCoroutineUninterceptedOrReturn` blocks.
fn unintercepted_continuation_reads(ir: &IrFile, body: ExprId) -> Vec<ExprId> {
    fn collect(ir: &IrFile, expression: ExprId, inside: bool, reads: &mut Vec<ExprId>) {
        let inside = inside || ir.is_unintercepted_suspension(expression);
        if inside && matches!(ir.exprs[expression as usize], IrExpr::CurrentContinuation) {
            reads.push(expression);
        }
        for_each_child(&ir.exprs, expression, &mut |child| {
            collect(ir, child, inside, reads)
        });
    }
    let mut reads = Vec::new();
    collect(ir, body, false, &mut reads);
    reads
}

/// Whether the body reads its own continuation (`suspendCoroutineUninterceptedOrReturn`), which
/// kotlinc's transformer realizes through a fake continuation marker.
fn reads_current_continuation(ir: &IrFile, expression: ExprId) -> bool {
    if let IrExpr::CurrentContinuation = ir.exprs[expression as usize] {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, expression, &mut |child| {
        found = found || reads_current_continuation(ir, child);
    });
    found
}
