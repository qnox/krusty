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
//! are all plain calls outside any `try`, and which call no inline function; an overridable
//! member's machine is built in the `$suspendImpl` its body moves to (`jvm::suspend_impls`).
//! Suspend lambdas of that shape go through `suspend_lambda`, which shares the eligibility and
//! suspension collection here. Interface bodies, `try` and spliced inline bodies are the next steps
//! of the plan; until then they keep the IR machine.

use std::collections::HashSet;

use super::cps::{
    calls_an_inline_function, spliced_inline_suspensions, TransformedMachine, TransformedSuspension,
};
use super::emission_facts::{ContinuationMetadata, ContinuationMetadataMap};
use super::spill_layout::{suspension_points_in_order, SpillLayout};
use super::{
    adopt_cps_signature, append_continuation, box_returns, build_continuation_class,
    continuation_class_name, continuation_ordinal, continuation_source_name, ensure_tail_return,
    expr_calls_suspend, recorded_suspension_result, shift_locals, suspend_call_fid,
    value_class_suspension_result, EmitTimeMachines, MachineContext,
};
use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// What a routed function needs from the rest of the pass.
pub(super) struct Route<'a, 'b> {
    pub(super) facade: &'a str,
    pub(super) suspend_set: &'a HashSet<u32>,
    pub(super) context: &'a MachineContext<'a>,
    pub(super) machines: &'b mut EmitTimeMachines,
    pub(super) continuation_metadata: &'b mut ContinuationMetadataMap,
    pub(super) default_call_operands:
        &'b mut crate::jvm::default_call_operands::DefaultCallOperands,
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
pub(super) fn route(ir: &mut IrFile, fid: u32, body: ExprId, mut route: Route<'_, '_>) -> Routed {
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

    let completion = adopt_cps_signature(ir, fid);
    shift_locals(ir, body, completion);
    for suspension in &suspensions {
        let continuation = ir.add_expr(IrExpr::CurrentContinuation);
        if !append_continuation(
            ir,
            suspension.call,
            continuation,
            route.default_call_operands,
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

    // The continuation is named from the recorded source identity, never reconstructed from a
    // value-class-mangled JVM name (or truncated at a valid backticked `-`).
    let source_name = continuation_source_name(ir, fid);
    // A member's continuation nests under its class, a top-level function's under the facade.
    let receiver = ir.functions[fid as usize].dispatch_receiver;
    let owner = receiver.map_or_else(|| route.facade.to_string(), |owner| owner.render());
    let continuation_class =
        continuation_class_name(&owner, source_name, continuation_ordinal(ir, fid));
    build_continuation_class(
        ir,
        &continuation_class,
        fid,
        &SpillLayout::default(),
        &[],
        receiver,
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
    route.continuation_metadata.insert(
        continuation_class.clone(),
        ContinuationMetadata {
            m: method.clone(),
            c: owner.replace('/', "."),
            v: 2,
            enclosing_class: owner,
            enclosing_method: method,
            enclosing_descriptor: crate::jvm::names::method_descriptor(
                &method_params,
                function.ret,
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
    route: &mut Route<'_, '_>,
    subject: Subject,
) -> Option<Vec<TransformedSuspension>> {
    // Common IR is a DAG: the CPS rewrites change nodes in place, so this body first owns one node
    // per use, as it does on the IR machine's path.
    for (source, target) in crate::ir::make_expression_children_unique_tracked(ir, body) {
        let IrExpr::Call { args, .. } = &ir.exprs[target as usize] else {
            continue;
        };
        if !route.default_call_operands.clone_call(source, target, args) {
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
    route: &Route<'_, '_>,
    subject: Subject,
) -> Option<Vec<ExprId>> {
    let function = &ir.functions[fid as usize];
    let top_level = function.is_static && function.dispatch_receiver.is_none();
    // A class member: its machine stays in the member, or, for an overridable one, moves with its
    // body to `$suspendImpl`. An interface body's is a later step.
    let class_member = !function.is_static
        && function.dispatch_receiver.is_some_and(|owner| {
            ir.classes
                .iter()
                .any(|class| class.fq_name_id() == owner && !class.is_interface)
        });
    let suspend_set = route.suspend_set;
    // Checked in order, each only while every earlier one holds.
    let declines: [(&dyn Fn() -> bool, &str); 9] = [
        (
            &|| !route.context.null_out_dead_spills,
            "no spill clean-up in the runtime",
        ),
        (
            &|| subject == Subject::NamedFunction && !(top_level || class_member),
            "not top-level or a class member",
        ),
        (
            &|| subject == Subject::NamedFunction && ir.private_methods.contains(&fid),
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
        // Spliced inline bodies are a later step: the splice does not mark the call's own line
        // yet, which the transformer's `@DebugMetadata` reads off the body.
        (&|| splices_inline_code(ir, body), "splices an inline body"),
        // A classpath inline body or an inline lambda that suspends is spliced into this frame.
        (
            &|| !spliced_inline_suspensions(ir, body, suspend_set).is_empty(),
            "suspends in a spliced inline body",
        ),
        (
            &|| suspends_under_try(ir, body, suspend_set),
            "suspends under a try",
        ),
        (
            &|| reads_current_continuation(ir, body),
            "reads its own continuation",
        ),
    ];
    let declined = declines
        .iter()
        .find_map(|(declines, reason)| declines().then_some(*reason));
    if let Some(reason) = declined {
        crate::trace_compiler!(
            "suspend",
            "transformer declines {}: {reason}",
            function.name
        );
        return None;
    }
    let points = suspension_points_in_order(ir, body, route.suspend_set);
    let plain_call = |call: ExprId| {
        matches!(ir.exprs[call as usize], IrExpr::Call { .. })
            && !ir.intrinsic_suspension_points.contains_key(&call)
            && value_class_suspension_result(ir, call, route.suspend_set).is_none()
            && (suspend_call_fid(ir, call, route.suspend_set).is_some()
                || recorded_suspension_result(ir, call).is_some())
    };
    if points.is_empty() {
        crate::trace_compiler!(
            "suspend",
            "transformer declines {}: it has no suspension point",
            function.name
        );
        return None;
    }
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

/// Whether a suspension point runs inside a `try`, which FixStack's handler bookkeeping and the
/// transformer's try/catch splitting do not take yet.
fn suspends_under_try(ir: &IrFile, expression: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if let IrExpr::Try { .. } = ir.exprs[expression as usize] {
        return expr_calls_suspend(ir, expression, suspend_set);
    }
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[expression as usize] {
        return captures
            .iter()
            .any(|&capture| suspends_under_try(ir, capture, suspend_set));
    }
    let mut found = false;
    for_each_child(&ir.exprs, expression, &mut |child| {
        found = found || suspends_under_try(ir, child, suspend_set);
    });
    found
}

/// Whether the body calls an inline function, whose body the emitter splices into this one.
fn splices_inline_code(ir: &IrFile, expression: ExprId) -> bool {
    if calls_an_inline_function(ir, expression) {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, expression, &mut |child| {
        found = found || splices_inline_code(ir, child);
    });
    found
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
