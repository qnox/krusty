//! JVM coroutine (`suspend fun`) IR lowering pass — an **optional, JVM-only** IR→IR transform.
//!
//! Common lowering keeps a `suspend fun` as a plain function with its declared Kotlin signature and
//! records its `FunId` in `ir.suspend_funs`, so common IR stays target-neutral (a JS backend realizes
//! suspension differently). This pass realizes kotlinc's JVM continuation-passing-style (CPS) ABI:
//!
//!   * every suspend function gains a trailing `kotlin.coroutines.Continuation` parameter and erases
//!     its return type to `java.lang.Object` (the resume value, *boxed*);
//!   * a **leaf** suspend function (no suspension point) is just that — straight-line, boxed return,
//!     no state machine (matches kotlinc's `static Object foo(Continuation)`);
//!   * a suspend function WITH a suspension point (a call to another suspend function) becomes a state
//!     machine: a synthesized `Facade$fn$1 extends ContinuationImpl` continuation class holds the
//!     `result`/`label` across resumes, and the function body dispatches on `label`, threading its own
//!     continuation into each suspend call and returning `COROUTINE_SUSPENDED` when a callee suspends.
//!
//! The body is *flattened* into a flat state graph (`Flat`): each suspension point — including ones
//! inside an `if`/`when` (branch value or statement) and inside a `while` loop — ends a state and begins
//! a resume state, control flow becomes `label = next` transitions, and a local live across a suspension
//! point is spilled to a continuation field. A suspension nested at an unconditional position in an
//! expression (`foo() + 2`) is hoisted to a temp first (`hoist_suspensions`). The whole thing is ordinary
//! IR (`while(true){ when(label){…} }`), so the existing emitter produces the bytecode + stack-map
//! frames; it is runtime-equivalent to kotlinc's `tableswitch` (an `if`-chain dispatch). A member suspend
//! fn is supported: its continuation captures the receiver (`this$0`), and on resume `invokeSuspend` does
//! `receiver.m(continuation)` (invokevirtual). A suspend body may call a (static or member) suspend fn —
//! the continuation is threaded into the `Call`/`MethodCall`. Shapes not yet modeled (a suspension under
//! a conditional sub-expression like elvis/`&&`, an extension suspend fn, or a member suspend fn with its
//! own parameters — its continuation would also have to capture them) skip the file.

mod bottom_completion;
pub(crate) mod cps;
pub(crate) use cps::EmitTimeMachines;
mod debug_metadata;
mod get_or_create;
mod hoisting;
mod live_scopes;
use hoisting::{hoist_spliced_inline_bodies, hoist_suspensions};
use live_scopes::{
    live_temp_scopes, merge_live_temps, reconcile_positional_spill_locals, ScopeWalk,
};
mod spill_layout;
use spill_layout::{suspension_points_in_order, SpillLayout};
mod statement_normalization;
mod value_liveness;

use crate::ir::{
    for_each_child, Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrCtorArg, IrExpr, IrFile,
    IrFunction, IrTypeOp,
};
use crate::kt_string::KtString;
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty, TypeName};
use bottom_completion::{suspension_completion, unwrap_suspend_cast, SuspensionCompletion};
use debug_metadata::capture_suspension_lines;
use get_or_create::build_get_or_create;
use statement_normalization::{
    demote_block_value_to_statement, normalize_block_inits, normalize_statement_try_results,
    split_unit_conditional_returns,
};
use std::collections::{HashMap, HashSet};
use value_liveness::{kills_value, pending_reads_after};

const I32_MIN: i32 = i32::MIN;
/// `when` branches: each `(condition, body)` (an `else` branch has `condition = None`).
type Branches = Vec<(Option<ExprId>, ExprId)>;
/// The semantic pieces of a value-position `try`, after peeling the optional result coercion that
/// must instead be applied to each selected branch. Keeping this extraction in one place ensures
/// return-bound and local-bound forms recognize exactly the same IR shapes.
struct ValueTryParts {
    body: ExprId,
    catches: Vec<crate::ir::IrCatch>,
    finally: Option<ExprId>,
    branch_wrap: Option<ValueBranchWrap>,
}

#[derive(Clone)]
enum ValueBranchWrap {
    TypeOp(IrTypeOp, Ty),
    /// A JVM value-class representation wrapper already selected and emitted by the preceding target
    /// pass. It is a pure one-argument conversion, so applying it to each selected `try` value preserves
    /// the wrapper around the value while exposing the suspension to the state-machine normalizer.
    ValueClassBox(Callee),
}
/// A direct suspension at a statement: `(optional bound local + type, the call ExprId, completion)`.
/// The call is reused and receives the continuation; completion preserves the two independent facts
/// owned by a peeled [`IrExpr::BottomValue`] in the statement's exact use context.
type Suspension = (Option<(u32, Ty)>, ExprId, SuspensionCompletion);
#[derive(Clone, Default)]
struct SuspensionScope {
    values: Vec<(u32, Ty)>,
    names: std::collections::HashMap<u32, String>,
}
type SuspensionScopes = std::collections::HashMap<ExprId, SuspensionScope>;
const CONTINUATION: &str = "kotlin/coroutines/Continuation";
const CONTINUATION_IMPL: &str = "kotlin/coroutines/jvm/internal/ContinuationImpl";

/// JVM class metadata computed before continuation spill scopes are discarded.
#[derive(Clone, Debug, Default)]
pub struct ContinuationMetadata {
    pub l: Vec<i32>,
    pub nl: Vec<i32>,
    pub i: Vec<i32>,
    pub s: Vec<String>,
    pub n: Vec<String>,
    pub m: String,
    pub c: String,
    pub v: i32,
    pub enclosing_class: String,
    pub enclosing_method: String,
    pub enclosing_descriptor: String,
}

pub type ContinuationMetadataMap = std::collections::HashMap<String, ContinuationMetadata>;

fn object_ty() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}
fn int_ty() -> Ty {
    Ty::obj("kotlin/Int")
}
fn continuation_ty() -> Ty {
    Ty::obj(CONTINUATION)
}

/// Physical JVM type of one suspend declaration parameter. Shared mutable captures remain their
/// source element type in common IR and carry a sparse marker; every coroutine layout consumer must
/// apply that marker identically so pre-normalization scope snapshots and final spill fields agree.
fn suspend_parameter_ty(ir: &IrFile, function: u32, parameter: usize, semantic: Ty) -> Ty {
    ir.shared_capture_parameters
        .get(&(function, parameter as u32))
        .map(crate::jvm::shared_captures::holder_ty)
        .unwrap_or(semantic)
}

/// Trace a residual suspending expression as an id-labelled tree. This is intentionally kept next to
/// the suspend pass rather than in the generic IR printer: it is only useful when normalization leaves
/// a suspension below a statement shape the state-machine flattener does not model.
fn trace_residual_suspension(ir: &IrFile, expression: ExprId, depth: usize) {
    crate::trace_compiler!(
        "suspend",
        "flatten residual {}{expression}: {:?}",
        "  ".repeat(depth),
        ir.exprs[expression as usize]
    );
    if depth >= 12 {
        return;
    }
    let mut children = Vec::new();
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        trace_residual_suspension(ir, child, depth + 1);
    }
}

fn trace_residual_parents(ir: &IrFile, expression: ExprId) {
    fn walk(ir: &IrFile, child: ExprId, depth: usize, seen: &mut HashSet<ExprId>) {
        if depth >= 10 || !seen.insert(child) {
            return;
        }
        for (parent, node) in ir.exprs.iter().enumerate() {
            let mut contains = false;
            for_each_child(&ir.exprs, parent as ExprId, &mut |candidate| {
                contains = contains || candidate == child;
            });
            if contains {
                crate::trace_compiler!(
                    "suspend",
                    "flatten parent {}{}: {:?}",
                    "  ".repeat(depth),
                    parent,
                    node
                );
                walk(ir, parent as ExprId, depth + 1, seen);
            }
        }
    }
    walk(ir, expression, 0, &mut HashSet::new());
}

/// Rewrite every `suspend fun` in `ir` to the JVM CPS ABI. `facade` is the file's facade class internal
/// name (e.g. `SKt`) — the continuation class for `bar` is `SKt$bar$1`. Returns `false` (skip the whole
/// file, never miscompile) on any suspend shape this pass can't yet transform.
#[must_use]
pub(crate) fn lower_suspend(
    ir: &mut IrFile,
    facade: &str,
    continuation_metadata: &mut ContinuationMetadataMap,
    default_call_operands: &mut crate::jvm::default_call_operands::DefaultCallOperands,
    emit_time_machines: &mut EmitTimeMachines,
) -> bool {
    realize_safe_coroutine_points(ir);
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    // Snapshot every function's *declared* (pre-CPS) return type, so hoisted suspension temps are typed
    // by the callee's logical result type even after the callee has itself been CPS-rewritten to `Object`.
    let orig_rets: Vec<Ty> = ir.functions.iter().map(|f| f.ret.clone()).collect();
    let fids = ir.suspend_funs.clone();
    let mut pre_splice_scopes: std::collections::HashMap<u32, SuspensionScopes> =
        std::collections::HashMap::new();
    crate::trace_compiler!(
        "suspend",
        "lower_suspend facade={facade} suspend_funs={fids:?} suspend_lambda_sm={}",
        ir.suspend_lambda_sm.len()
    );
    for fid in fids {
        let body = ir.functions[fid as usize].body;
        // A pure TAIL-CALL forward (`suspend fun f(…) = g(…)` where `g` is the sole suspension) needs NO
        // state machine — kotlinc forwards `$completion` to the callee and returns its result. Detect it on
        // the RAW body, before splice/hoist/desugar reshape the tail suspension into a bound resume point.
        // The call `ExprId` is stable across the later `shift_locals`, so remember it and thread
        // `$completion` in below. When it matches, the splice/hoist/desugar normalizations are all skipped.
        let fn_unit_ret = orig_rets[fid as usize] == Ty::Unit;
        let forward =
            body.and_then(|b| tail_forward_call(ir, b, &suspend_set, fn_unit_ret, &orig_rets));
        // Common IR is a DAG and may share one operand between several evaluation sites. Hoisting
        // rewrites descendants in place and installs each suspension temp in the current parent's
        // prelude, so every non-forward body that can reach a suspension must own one node per use.
        // Do this before scope/debug-line capture: cloned suspension identities then become the
        // authoritative keys used by every later coroutine phase.
        if let (Some(b), None) = (body, forward) {
            if expr_calls_suspend(ir, b, &suspend_set) {
                let clones = crate::ir::make_expression_children_unique_tracked(ir, b);
                for (source, target) in clones {
                    let IrExpr::Call { args, .. } = &ir.exprs[target as usize] else {
                        continue;
                    };
                    if !default_call_operands.clone_call(source, target, args) {
                        return false;
                    }
                }
            }
        }
        // Capture the per-suspension lexical scope lists BEFORE splice/hoist reshape the body:
        // `splice_return_blocks` flattens block STATEMENTS into their parent, which would leak a
        // block-scoped local (a `for`-loop iterator) into every later suspension's scope. Suspend-call
        // expr ids are stable through the transforms; keyed per function.
        if let (Some(b), None) = (body, forward) {
            if let std::collections::hash_map::Entry::Vacant(entry) = pre_splice_scopes.entry(fid) {
                let is_lambda = ir.suspend_lambda_sm.iter().any(|(f2, _, _)| *f2 == fid);
                let prefix: Vec<(u32, Ty)> = if is_lambda {
                    Vec::new()
                } else {
                    let f = &ir.functions[fid as usize];
                    // `dispatch_receiver` retains the semantic owner for a value-class member even
                    // after the JVM value-class pass turns that member into a static `*-impl`
                    // method whose carrier is parameter zero. Only a physically non-static method
                    // owns a JVM `this` slot.
                    let this_offset = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
                    // Capture runs BEFORE the CPS rewrite appends the `Continuation` — only strip a
                    // trailing continuation when it is already there.
                    let has_cont = f.params.last().is_some_and(|t| {
                        t.obj_internal()
                            .is_some_and(|n| n.matches("kotlin/coroutines/Continuation"))
                    });
                    let n_real = f.params.len().saturating_sub(usize::from(has_cont));
                    // ALL value parameters — kotlinc spills primitive params too (`Z$0` for a
                    // Boolean), per-kind counters decide the field family.
                    (0..n_real)
                        .map(|i| {
                            (
                                this_offset + i as u32,
                                spill_field_ty(suspend_parameter_ty(ir, fid, i, f.params[i])),
                            )
                        })
                        .collect()
                };
                {
                    let mut w = ScopeWalk {
                        ir,
                        suspend_set: &suspend_set,
                        params: &prefix,
                        scope: Vec::new(),
                        pending: Vec::new(),
                        levels: Vec::new(),
                        temps_only: false,
                        out: Default::default(),
                    };
                    // Walk the WHOLE body block — an expression-bodied fn (`= withLock { … }`)
                    // carries its suspensions in the block's VALUE, not its statements.
                    w.walk(b);
                    let out = w.out;
                    entry.insert(out);
                }
            }
        }
        let suspension_lines = if let (Some(b), None) = (body, forward) {
            capture_suspension_lines(ir, b, &suspend_set, ir.fn_close_lines.get(&fid).copied())
        } else {
            std::collections::HashMap::new()
        };
        // Normalize `return { stmts…; value }` into `stmts…; return value`. An elvis / safe-call subject
        // that suspends lowers to a value-position `Block` binding a temp (`{ val t = susp()…; when{…} }`);
        // hoisting can't see into a value block, so the suspension would hide there and the flattener bail.
        // Splicing lifts the block's statements to the top level where the hoister/flattener handle them.
        if let (Some(b), None) = (body, forward) {
            splice_return_blocks(ir, b);
            separate_catches_from_finally(ir, b);
        }
        // Hoist a suspension nested at an unconditional position in an expression (`foo() + 2`) into a
        // preceding `val tmp = foo()` temp, so the flattener only meets suspensions at handled positions.
        if let (Some(b), None) = (body, forward) {
            let mut value_types = function_value_types(ir, fid, b);
            hoist_suspensions(ir, b, &suspend_set, &orig_rets, &mut value_types);
        }
        // Desugar `return <suspend call>` (incl. an `= <suspend call>` expression body) into
        // `val tmp = <suspend call>; return tmp` so a tail-position suspension becomes a uniform
        // bound-local point. Uses the function's (pre-CPS) declared return type for `tmp`.
        let ret_ty = ir.functions[fid as usize].ret.clone();
        if let (Some(b), None) = (body, forward) {
            // An expression-bodied suspend function can carry a suspending `when`/`try` as the block's
            // trailing VALUE rather than as an explicit `return`. Materialize that tail first so the
            // same value-control-flow desugars below handle expression and block bodies identically.
            // Do not do this for a direct tail suspension: `desugar_tail_suspend` owns that shape and
            // preserves tail-call/CPS result typing.
            // Tail forwarding was decided from the untouched checked IR above. If a remaining
            // trailing value still suspends, materialize it as an explicit return/effect now so
            // the generic statement normalizers can expose its suspension. This covers not only
            // value `when`/`try`, but a source grouping block retained below a checked coercion.
            let suspending_tail = matches!(&ir.exprs[b as usize],
                IrExpr::Block { value: Some(value), .. }
                    if expr_calls_suspend(ir, *value, &suspend_set));
            if suspending_tail {
                ensure_tail_return(ir, b, ret_ty == Ty::Unit);
                // Materializing the tail can expose an inline non-local return that was previously
                // the block's value (`run { return await() }`). Re-run the same structural splicer so
                // the inner return becomes the function statement and the now-unreachable synthetic
                // outer return is removed before suspension hoisting/state flattening.
                splice_return_blocks(ir, b);
            }
            desugar_value_try(ir, b, &suspend_set, &ret_ty);
            desugar_value_when(ir, b, &suspend_set, &ret_ty);
            normalize_statement_try_results(ir, b, true);
            // A leaf suspend function is emitted as an ordinary JVM method with an added continuation
            // parameter; its structured try/finally already implements returns correctly. Pending
            // completion is required only when this body will actually be split into machine states.
            if expr_calls_suspend(ir, b, &suspend_set) {
                linearize_finally_returns(ir, b, &ret_ty);
            }
            linearize_suspending_finally(ir, b, &suspend_set);
            normalize_block_inits(ir, b);
            // The value-`try`/`when` desugars bind each branch's value to a local, which can leave a
            // suspension NESTED in that bound value (`v = Sub(mk().tag)`) — a position the FIRST
            // hoist pass (which ran before the desugars) never saw. Hoist again with a FRESH
            // value-type map (the desugars declared new locals); already-normalized statements pass
            // through unchanged.
            let mut value_types = function_value_types(ir, fid, b);
            hoist_suspensions(ir, b, &suspend_set, &orig_rets, &mut value_types);
            promote_diverging_tail_to_statement(ir, b);
            desugar_tail_suspend(ir, b, &suspend_set, &ret_ty);
        }
        let has_susp =
            forward.is_none() && body.is_some_and(|b| expr_calls_suspend(ir, b, &suspend_set));
        // The IR machine sees no suspension here, but one lives inside a lambda whose body will be
        // spliced into this very frame. Its machine is built during emission, where the spliced
        // body's own locals exist. The conjunction is the whole gate: this claims only functions
        // that would otherwise be emitted with no continuation to pass.
        // A member's continuation re-enters the method on the receiver it kept, so the call it makes
        // has to reach exactly this body: an OPEN method would re-dispatch to an override, and a
        // private one is not callable from the continuation class. Those keep the diagnostic they
        // have today.
        //
        // A function that ALSO has suspensions of its own is taken whole: one method has one
        // dispatch, so the two kinds cannot be split between the IR machine and this one. The IR
        // machine never saw the spliced kind, so such a function does not compile today at all.
        let spliced_suspensions: Vec<ExprId> = match (forward, body) {
            (None, Some(b)) if machine_eligible(ir, fid) => {
                match cps::spliced_inline_suspensions(ir, b, &suspend_set).is_empty() {
                    true => Vec::new(),
                    false => cps::frame_suspensions(ir, b, &suspend_set),
                }
            }
            _ => Vec::new(),
        };
        #[cfg(feature = "trace")]
        if spliced_suspensions.is_empty() && forward.is_none() {
            if let Some(b) = body {
                let ignored = cps::spliced_inline_suspensions(ir, b, &suspend_set);
                if !ignored.is_empty() {
                    crate::trace_compiler!(
                        "suspend",
                        "machine NOT eligible fid={fid} name={} static={} open={} private={} n={}",
                        ir.functions[fid as usize].name,
                        ir.functions[fid as usize].is_static,
                        ir.open_methods.contains(&fid),
                        ir.private_methods.contains(&fid),
                        ignored.len()
                    );
                }
            }
        }
        // Those bodies have never been through the value-`try` desugar or suspension hoisting: the
        // passes above stop at a lambda. Normalize them now, then re-read the suspensions — hoisting
        // rewrites the very expressions just collected. Each body is typed in its own lambda's value
        // numbering, not this function's. A value-`try` the desugar could not reach (one nested
        // inside an expression) still keeps the call's raw `Object` in a scalar arm; such a body is
        // declined after normalization, exactly as before the machine existed.
        //
        // A direct non-local `return` out of such a body is NOT a reason to decline: `box_returns`
        // walks a lambda's retained `inline_body`, so the return already yields the CPS `Object`
        // result. A return that crosses `finally` remains declined until that control transfer can
        // be normalized inside a retained inline body.
        let spliced_suspensions = match (spliced_suspensions.is_empty(), body) {
            (false, Some(b)) => {
                hoist_spliced_inline_bodies(ir, b, &suspend_set, &orig_rets, &ret_ty);
                // Do not let an explicitly unsupported control transfer fall through to emission
                // and masquerade as an unrelated continuation-arity error. This backend pass owns
                // the limitation and declines the file at the exact boundary that detects it.
                if cps::spliced_return_crosses_finally(ir, b) {
                    return false;
                }
                let declined = cps::suspends_in_a_value_try(ir, b, &suspend_set);
                match declined {
                    true => Vec::new(),
                    false => cps::frame_suspensions(ir, b, &suspend_set),
                }
            }
            _ => spliced_suspensions,
        };
        let emit_time_machine = !spliced_suspensions.is_empty();
        // A `suspendCoroutineUninterceptedOrReturn` block that reads its continuation is a
        // first-class suspension point (common lowering records it separately from callable nodes): the
        // machine passes ITSELF as the continuation, so `it.resume(v)` re-enters this machine at
        // the resume label — kotlinc's protocol (coroutines/tailCallToNothing).
        crate::trace_compiler!(
            "suspend",
            "fn fid={fid} name={} has_susp={has_susp} spliced_suspensions={}",
            ir.functions[fid as usize].name,
            spliced_suspensions.len()
        );
        let is_static = ir.functions[fid as usize].is_static;
        // Keep the declared signature before it is consumed — the class's `@Metadata` and the method's
        // generic `Signature` both describe the function as Kotlin declared it.
        {
            let f = &ir.functions[fid as usize];
            let declared = (f.params.clone(), f.ret);
            ir.suspend_declared_sigs.insert(fid, declared);
        }
        // CPS signature: append the continuation parameter, erase the return to Object.
        let f = &mut ir.functions[fid as usize];
        f.params.push(continuation_ty());
        f.param_checks.push(None);
        f.ret = object_ty();
        // Preserve the synthesized continuation's ROLE in the physical parameter contract. The JVM
        // debug boundary formats it `$completion`; Kotlin metadata describes the declared function
        // and therefore reads only the leading declaration identities.
        if let Some(info) = ir.fn_params.get_mut(&fid) {
            info.identities
                .push(crate::ir::IrParameterIdentity::generated(
                    crate::ir::IrGeneratedParameterRole::Continuation,
                    None,
                ));
        }
        // kotlinc emits NO `checkNotNullParameter` on a suspend fn: the state-machine RE-ENTRY call
        // (`foo(null, continuation)`) passes null for every value parameter (the real values live in
        // the continuation's spill fields), so an entry null-check would throw on resume.
        for chk in f.param_checks.iter_mut() {
            *chk = None;
        }

        // The continuation parameter's value-index is `params + (this ? 1 : 0)`; common lowering numbered body
        // locals from that same index, so shift every body local up by one to make room for it.
        let p_old =
            ir.functions[fid as usize].params.len() as u32 - 1 + if is_static { 0 } else { 1 };
        if let Some(b) = body {
            shift_locals(ir, b, p_old);
            // A `suspendCoroutineUninterceptedOrReturn { c -> … }` in a body that gets NO machine
            // (a leaf or a tail-forward) keeps the placeholder semantics: `c` is the trailing
            // `Continuation` parameter itself. A body that DOES get a machine resolves the
            // placeholder to the machine WRAPPER inside `build_state_machine` (cont_v), so
            // `c.resume(v)` re-enters this machine.
            if !has_susp && !emit_time_machine {
                rewrite_current_continuation(ir, b, p_old);
            }
            // The pre-splice scope lists (captured above) hold PRE-shift local indices — shift them
            // identically so they match the machine's value numbering.
            if let Some(scopes) = pre_splice_scopes.get_mut(&fid) {
                for scope in scopes.values_mut() {
                    for e in &mut scope.values {
                        if e.0 >= p_old {
                            e.0 += 1;
                        }
                    }
                    scope.names = scope
                        .names
                        .drain()
                        .map(|(slot, name)| (if slot >= p_old { slot + 1 } else { slot }, name))
                        .collect();
                }
            }
        }

        if let (Some(call), Some(b)) = (forward, body) {
            // Tail-call forward: thread the function's own `$completion` (value-index `p_old`) into the
            // callee and return its `Object` result directly. No state machine, no continuation class —
            // exactly kotlinc's tail-call optimization.
            let cont = ir.add_expr(IrExpr::GetValue(p_old));
            if !append_continuation(ir, call, cont, default_call_operands) {
                return false;
            }
            // Checked expression bodies carry one source-oriented grouping block around the actual
            // statements. Normalize that transparent wrapper now that the forward decision has been
            // made; the call id remains stable and `make_forward_body` can rewrite the function's
            // physical tail in one place.
            splice_return_blocks(ir, b);
            make_forward_body(ir, b, call);
            // The body may hold EARLY returns besides the forwarded tail (`if (n == 0) return true;
            // return odd(n - 1)`) — the CPS method returns `Object`, so a primitive early return must
            // box exactly as in a leaf body (kotlinc boxes it and keeps the tail-call shape). The tail
            // return's coercion is an identity on the callee's already-`Object` result.
            if !box_returns(ir, b) {
                return false;
            }
        } else if emit_time_machine {
            // Emission owns this machine. Give every spliced suspension its continuation operand as
            // an `IrExpr::CurrentContinuation`: one `aload` either way, so the discovery pass (which
            // resolves it to `$completion`) allocates exactly the slots the emitting pass will.
            let b = body.expect("a spliced-inline suspension implies a body");
            let mut recorded = Vec::new();
            for &call in &spliced_suspensions {
                let cont = ir.add_expr(IrExpr::CurrentContinuation);
                if !append_continuation(ir, call, cont, default_call_operands) {
                    return false;
                }
                recorded.push(cps::SplicedSuspension { call });
            }
            if !box_returns(ir, b) {
                return false;
            }
            ensure_tail_return(ir, b, orig_rets[fid as usize] == Ty::Unit);
            // The standalone `invoke` of each such lambda is not emitted: it has no continuation of
            // its own to pass, and every call to it is spliced.
            for implementation in cps::spliced_suspension_lambda_impls(ir, b, &suspend_set) {
                ir.inline_only_fns.insert(implementation);
            }
            emit_time_machines.record(fid, recorded);
        } else if !has_susp {
            // Leaf: box the returns (no state machine). The CPS method returns `Object`, so an expression
            // / statement body that falls through (no `return`) must get a terminal return — a value body
            // returns its boxed value, a `Unit` body runs for effect then returns `Unit.INSTANCE`.
            if let Some(b) = body {
                if !box_returns(ir, b) {
                    return false;
                }
                let unit_ret = orig_rets[fid as usize] == Ty::Unit;
                ensure_tail_return(ir, b, unit_ret);
            }
        } else {
            let unit_ret = orig_rets[fid as usize] == Ty::Unit;
            if !build_state_machine(
                ir,
                facade,
                fid,
                body.unwrap(),
                unit_ret,
                &orig_rets,
                pre_splice_scopes.remove(&fid),
                &suspension_lines,
                continuation_metadata,
                default_call_operands,
            ) {
                return false;
            }
        }
    }
    // Suspend LAMBDAS with multiple suspensions / control flow: their `invokeSuspend` is a state machine
    // whose continuation is the lambda instance itself.
    for (fid, class_id, field_base) in ir.suspend_lambda_sm.clone() {
        if !build_lambda_state_machine(
            ir,
            fid,
            class_id,
            field_base,
            &orig_rets,
            pre_splice_scopes.remove(&fid),
            default_call_operands,
        ) {
            return false;
        }
    }
    finalize_suspend_bridges(ir);
    default_call_operands.synchronize(ir)
}

/// Canonicalize `try { body } catch { arms } finally { cleanup }` as an inner `try/catch` wrapped by
/// an outer `try/finally`. This is Kotlin's exact control-flow composition: the cleanup observes normal
/// completion of either the body or a selected catch, and also every exception escaping the inner
/// region. Keeping one handler responsibility per `IrExpr::Try` lets the state-machine handler stack
/// compose nested regions instead of growing a second catch-plus-finally implementation.
fn separate_catches_from_finally(ir: &mut IrFile, expression: ExprId) {
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        separate_catches_from_finally(ir, child);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if catches.is_empty() {
        return;
    }
    let inner = ir.add_expr(IrExpr::Try {
        body,
        catches,
        finally: None,
        result,
    });
    let outer_body = if result == Ty::Unit {
        ir.add_expr(IrExpr::Block {
            stmts: vec![inner],
            value: None,
        })
    } else {
        ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: Some(inner),
        })
    };
    ir.exprs[expression as usize] = IrExpr::Try {
        body: outer_body,
        catches: Vec::new(),
        finally: Some(finally),
        result,
    };
}

/// Make a suspending `finally` an ordinary state-machine sequence while preserving the exceptional
/// path. Value-producing tries have already been bound to storage before this runs, and combined
/// catch/finally regions have already been split, so the remaining node is statement-shaped:
///
/// ```text
/// var pending: Throwable? = null
/// try { body } catch (e: Throwable) { pending = e }
/// finallyBody
/// if (pending != null) throw pending
/// ```
///
/// `pending` is an ordinary compiler temp. The normal liveness pass sees its read after the finally
/// suspension and allocates the continuation spill; no backend-only field identity is invented here.
fn linearize_suspending_finally(ir: &mut IrFile, expression: ExprId, suspend_set: &HashSet<u32>) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        linearize_suspending_finally(ir, child, suspend_set);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if !catches.is_empty()
        || result != Ty::Unit
        || !expr_calls_suspend(ir, finally, suspend_set)
        || expr_has_return(ir, body)
        || expr_contains_owned_loop_jump(ir, body)
    {
        return;
    }

    let pending = max_value_index(ir) + 1;
    let pending_ty = Ty::nullable(Ty::obj("java/lang/Throwable"));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let declaration = ir.add_expr(IrExpr::Variable {
        index: pending,
        ty: pending_ty,
        init: Some(null),
        named: false,
    });
    let catch_var = pending + 1;
    let caught = ir.add_expr(IrExpr::GetValue(catch_var));
    let remember = ir.add_expr(IrExpr::SetValue {
        var: pending,
        value: caught,
    });
    let catch_body = ir.add_expr(IrExpr::Block {
        stmts: vec![remember],
        value: None,
    });
    let protected = ir.add_expr(IrExpr::Try {
        body,
        catches: vec![crate::ir::IrCatch {
            var: catch_var,
            binding: None,
            exc_internal: type_name("java/lang/Throwable"),
            body: catch_body,
        }],
        finally: None,
        result: Ty::Unit,
    });
    let pending_read = ir.add_expr(IrExpr::GetValue(pending));
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let has_exception = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Ne,
        lhs: pending_read,
        rhs: null,
    });
    let exception = ir.add_expr(IrExpr::GetValue(pending));
    let throw = ir.add_expr(IrExpr::Throw { operand: exception });
    let throw_block = ir.add_expr(IrExpr::Block {
        stmts: vec![throw],
        value: None,
    });
    let no_exception = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let rethrow = ir.add_expr(IrExpr::When {
        branches: vec![(Some(has_exception), throw_block), (None, no_exception)],
    });
    ir.exprs[expression as usize] = IrExpr::Block {
        stmts: vec![declaration, protected, finally, rethrow],
        value: None,
    };
}

/// Route function returns through each enclosing `finally`. The return expression is evaluated once
/// into a typed pending-value local; a labeled break leaves a one-shot loop around the protected body;
/// after cleanup, the completion-kind dispatch performs the actual return. An exception or a return
/// from the finally itself naturally bypasses/overrides that pending completion.
fn linearize_finally_returns(ir: &mut IrFile, expression: ExprId, return_ty: &Ty) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        linearize_finally_returns(ir, child, return_ty);
    }
    let IrExpr::Try {
        body,
        catches,
        finally: Some(finally),
        result,
    } = ir.exprs[expression as usize].clone()
    else {
        return;
    };
    if result != Ty::Unit || !expr_has_return(ir, body) {
        return;
    }

    let completion = max_value_index(ir) + 1;
    let pending_value = completion + 1;
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let completion_decl = ir.add_expr(IrExpr::Variable {
        index: completion,
        ty: Ty::Int,
        init: Some(zero),
        named: false,
    });
    let stored_return_ty = if *return_ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        *return_ty
    };
    let default_return = zero_value(ir, &stored_return_ty);
    let return_decl = ir.add_expr(IrExpr::Variable {
        index: pending_value,
        ty: stored_return_ty,
        init: Some(default_return),
        named: false,
    });
    let label = format!("$finally$return${expression}");
    rewrite_returns_to_pending(
        ir,
        body,
        &label,
        completion,
        pending_value,
        *return_ty == Ty::Unit,
    );
    let leave = ir.add_expr(IrExpr::Break {
        label: Some(label.clone()),
    });
    let loop_body = ir.add_expr(IrExpr::Block {
        stmts: vec![body, leave],
        value: None,
    });
    let always = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let protected_loop = ir.add_expr(IrExpr::While {
        cond: always,
        body: loop_body,
        update: None,
        post_test: false,
        label: Some(label),
    });
    let protected_body = ir.add_expr(IrExpr::Block {
        stmts: vec![protected_loop],
        value: None,
    });
    let protected = ir.add_expr(IrExpr::Try {
        body: protected_body,
        catches,
        finally: Some(finally),
        result: Ty::Unit,
    });
    let completion_read = ir.add_expr(IrExpr::GetValue(completion));
    let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
    let has_return = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Ne,
        lhs: completion_read,
        rhs: zero,
    });
    let return_value = ir.add_expr(IrExpr::GetValue(pending_value));
    let perform_return = ir.add_expr(IrExpr::Return(Some(return_value)));
    let return_block = ir.add_expr(IrExpr::Block {
        stmts: vec![perform_return],
        value: None,
    });
    let fallthrough = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let dispatch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(has_return), return_block), (None, fallthrough)],
    });
    ir.exprs[expression as usize] = IrExpr::Block {
        stmts: vec![completion_decl, return_decl, protected, dispatch],
        value: None,
    };
}

fn rewrite_returns_to_pending(
    ir: &mut IrFile,
    expression: ExprId,
    label: &str,
    completion: u32,
    pending_value: u32,
    unit_return: bool,
) {
    match ir.exprs[expression as usize].clone() {
        IrExpr::Lambda { .. } => return,
        IrExpr::Return(value) => {
            // A checked `Unit` expression may have a physical void coercion: it must execute for
            // effect, but it cannot be used as the operand of the pending-value store. Materialize
            // the semantic singleton after evaluation, exactly as an ordinary JVM `Unit` return.
            let effect = unit_return.then_some(value).flatten();
            let value = if unit_return {
                ir.add_expr(IrExpr::UnitInstance)
            } else {
                value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance))
            };
            let store_value = ir.add_expr(IrExpr::SetValue {
                var: pending_value,
                value,
            });
            let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            let mark_return = ir.add_expr(IrExpr::SetValue {
                var: completion,
                value: one,
            });
            let leave = ir.add_expr(IrExpr::Break {
                label: Some(label.to_string()),
            });
            let stmts = effect
                .into_iter()
                .chain([store_value, mark_return, leave])
                .collect();
            ir.exprs[expression as usize] = IrExpr::Block { stmts, value: None };
        }
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
            for child in children {
                rewrite_returns_to_pending(
                    ir,
                    child,
                    label,
                    completion,
                    pending_value,
                    unit_return,
                );
            }
        }
    }
}

/// Realize Kotlin's safe `suspendCoroutine` protocol around each already-inlined user block.
///
/// Common lowering preserves the selected primitive and its checked block as one semantic suspension
/// point. The JVM realization uses the stdlib's actual protocol: intercept the current machine
/// continuation, wrap it in `SafeContinuation`, invoke the block once with that wrapper, then read
/// `getOrThrow()`. An immediate resume therefore produces the value synchronously; an asynchronous
/// resume first returns `COROUTINE_SUSPENDED` and later re-enters the enclosing machine. No callable
/// lookup or inline-body recovery happens here—the frontend supplied both the exact intrinsic kind and
/// the already-spliced block.
fn realize_safe_coroutine_points(ir: &mut IrFile) {
    let points = ir
        .intrinsic_suspension_points
        .iter()
        .filter_map(|(&expression, point)| {
            (point.kind == crate::ir::IrIntrinsicSuspensionKind::Safe).then_some(expression)
        })
        .collect::<Vec<_>>();
    for expression in points {
        let safe_slot = max_value_index(ir).saturating_add(1);
        let block = ir.add_expr(ir.exprs[expression as usize].clone());
        rewrite_subtree(ir, block, &mut |node| {
            if matches!(node, IrExpr::CurrentContinuation) {
                *node = IrExpr::GetValue(safe_slot);
            }
        });

        let current = ir.add_expr(IrExpr::CurrentContinuation);
        let intercepted = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlin/coroutines/intrinsics/IntrinsicsKt"),
                name: "intercepted".to_string(),
                descriptor: "(Lkotlin/coroutines/Continuation;)Lkotlin/coroutines/Continuation;"
                    .to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![current],
        });
        let safe_ty = Ty::obj("kotlin/coroutines/SafeContinuation");
        let safe = ir.add_expr(IrExpr::New {
            internal: type_name("kotlin/coroutines/SafeContinuation"),
            args: vec![intercepted],
            ctor_params: None,
            ctor_desc: Some("(Lkotlin/coroutines/Continuation;)V".to_string()),
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let declare_safe = ir.add_expr(IrExpr::Variable {
            index: safe_slot,
            ty: safe_ty,
            init: Some(safe),
            named: false,
        });
        let safe_for_result = ir.add_expr(IrExpr::GetValue(safe_slot));
        let result = ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: type_name("kotlin/coroutines/SafeContinuation"),
                name: "getOrThrow".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                params: None,
                interface: false,
            },
            dispatch_receiver: Some(safe_for_result),
            args: Vec::new(),
        });
        ir.exprs[expression as usize] = IrExpr::Block {
            stmts: vec![declare_safe, block],
            value: Some(result),
        };
        crate::trace_compiler!(
            "suspend",
            "realize safe coroutine point expression={expression} safe_slot={safe_slot}"
        );
    }
}

/// Convert already-derived suspend override bridges from their declared Kotlin signature to the JVM
/// CPS signature. Bridge derivation intentionally runs before value-class realization, so that pass can
/// reuse its normal target mangling and argument/return adaptation. Once concrete suspend methods have
/// gained their trailing continuation, both sides of each matching bridge gain the same parameter and
/// return `Object`; any pre-CPS result boxing is removed because the concrete suspend method already
/// crosses the continuation boundary in boxed form.
fn finalize_suspend_bridges(ir: &mut IrFile) {
    let continuation = continuation_ty();
    let object = object_ty();
    let suspend_targets: HashSet<(TypeName, String, usize)> = ir
        .classes
        .iter()
        .flat_map(|class| {
            class.methods.iter().filter_map(|&fid| {
                ir.suspend_funs.contains(&fid).then(|| {
                    let function = &ir.functions[fid as usize];
                    (
                        class.fq_name,
                        function.name.clone(),
                        function.params.len().saturating_sub(1),
                    )
                })
            })
        })
        .collect();
    let boxed_suspend_targets: HashSet<(TypeName, String, usize)> = ir
        .classes
        .iter()
        .flat_map(|class| {
            class.methods.iter().filter_map(|&fid| {
                matches!(
                    ir.value_class_suspend_returns.get(&fid),
                    Some(crate::ir::IrValueClassSuspendResult::Boxed { .. })
                )
                .then(|| {
                    let function = &ir.functions[fid as usize];
                    (
                        class.fq_name,
                        function.name.clone(),
                        function.params.len().saturating_sub(1),
                    )
                })
            })
        })
        .collect();
    for class in &mut ir.classes {
        for bridge in &mut class.bridges {
            let target = bridge.target_name.as_deref().unwrap_or(&bridge.name);
            if !suspend_targets.contains(&(
                class.fq_name,
                target.to_string(),
                bridge.concrete_params.len(),
            )) {
                continue;
            }
            bridge.erased_params.push(continuation);
            bridge.concrete_params.push(continuation);
            bridge
                .parameter_identities
                .push(crate::fir::ResolvedParameterIdentity::SuspendCompletion);
            if !bridge.unbox_params.is_empty() {
                bridge.unbox_params.push(None);
            }
            bridge.erased_ret = object;
            let target_key = (
                class.fq_name,
                target.to_string(),
                bridge.concrete_params.len().saturating_sub(1),
            );
            let target_returns_boxed = boxed_suspend_targets.contains(&target_key);
            if bridge.box_ret.is_some()
                && bridge.concrete_ret.is_reference()
                && !target_returns_boxed
            {
                // A generic supertype boundary needs a BOX, while a reference-carrier suspend target
                // returns the raw carrier as Object. Preserve the pre-CPS carrier adaptation and record
                // only the target descriptor's erased return separately.
                bridge.target_ret = Some(object);
            } else {
                // A scalar-carrier suspend target already boxed its value class before `areturn`; a
                // non-value-class bridge needs no result adaptation either. Forward Object directly.
                bridge.concrete_ret = object;
                bridge.target_ret = None;
                bridge.box_ret = None;
            }
            crate::trace_compiler!(
                "bridges",
                "finalized suspend bridge {}::{} -> {} arity={} target_boxed={target_returns_boxed}",
                class.fq_name,
                bridge.name,
                target,
                bridge.erased_params.len()
            );
        }
    }
}

/// Lift a value-position `Block` out of a top-level statement's direct operand, so a suspension buried in
/// the block's statements surfaces at the top level where the hoister/flattener handle it. An elvis /
/// safe-call whose subject suspends lowers to `{ val t = susp()…; when{…} }` in `return`/`val =`/assign
/// position — a block the hoister can't see into, so the flattener would bail. This rewrites:
///   `return { s…; v }`      → `s…; return v`
///   `val x = { s…; v }`     → `s…; val x = v`
///   `x = { s…; v }`         → `s…; x = v`
///   `capture = { s…; v }`   → `s…; capture = v`
/// A value-bearing block is spliced into its consumer. A value-less block is spliced only when it
/// definitely diverges, in which case the consumer can never execute and is removed. Re-runs until
/// settled, so nested blocks (safe-call inside elvis) fully unfold; lifted statements are reprocessed.
fn splice_return_blocks(ir: &mut IrFile, b: ExprId) {
    // Normalize lexical child blocks before their parent. A return/value wrapper can sit inside an
    // `if`/`when` or catch body just as legitimately as at the function body's top level; making the
    // transform depend on that container was the source of a private conditional-tail regression.
    // Stop the discovery walk at each nested block and recurse from there, so every block is processed
    // exactly within its own lexical boundary: statements may move inside that block, never out through
    // a branch/try owner. `Lambda` and `While` are LOWERING boundaries, not merely nested lexical
    // regions: a lambda's `inline_body` is a substitution template and collection HOFs have already
    // expanded their template into a loop by this phase. Descending into either here can reshape the
    // loop body before the dedicated loop/state-machine flattening establishes its labels and spills;
    // that made otherwise-supported suspending `map`/`flatMap` bodies decline emission. Their own
    // lowering path presents the relevant blocks to this normalizer when it is safe to do so. The
    // visited set also makes shared arena nodes and malformed cycles safe.
    let mut nested_blocks = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::from([b]);
    for_each_child(&ir.exprs, b, &mut |child| pending.push(child));
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        if matches!(
            ir.exprs[node as usize],
            IrExpr::Lambda { .. } | IrExpr::While { .. }
        ) {
            continue;
        }
        if matches!(ir.exprs[node as usize], IrExpr::Block { .. }) {
            nested_blocks.push(node);
            continue;
        }
        for_each_child(&ir.exprs, node, &mut |child| pending.push(child));
    }
    for nested in nested_blocks {
        splice_return_blocks(ir, nested);
    }

    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len());
    let mut changed = false;
    let mut value = value;
    let n = stmts.len();
    for (i, s) in stmts.into_iter().enumerate() {
        // A bare `Block` STATEMENT is pure grouping (IR locals are flat-indexed) — lift its statements
        // into the parent so a labeled break/suspension buried in the nested block reaches the top-level
        // flattening stream rather than surviving as a structured node. Its trailing VALUE: when the block
        // is the parent's LAST statement and the parent has no value of its own (an `= withLock { … }`
        // expression body lowers to `{ <withLock block> }`), the value IS the body's result — promote it
        // to the parent value so `ensure_tail_return` returns it. Otherwise it sits in statement position
        // and is run for effect.
        if let IrExpr::Block {
            stmts: bs,
            value: bv,
        } = ir.exprs[s as usize].clone()
        {
            // A `suspendCoroutineUninterceptedOrReturn` block is a registered suspension point,
            // not grouping — splicing it would orphan the suspension-point id.
            if ir.intrinsic_suspension_points.contains_key(&s) {
                new_stmts.push(s);
                continue;
            }
            new_stmts.extend(bs);
            if let Some(v) = bv {
                if i + 1 == n && value.is_none() {
                    value = Some(v);
                } else {
                    new_stmts.push(v);
                }
            }
            changed = true;
            continue;
        }
        // A checked coercion around a value-less block is equally transparent when that block
        // cannot fall through (notably `Nothing?` around an inlined labelled return). There is no
        // value on which the coercion could execute, so expose the structural exit to state-machine
        // splitting just as `diverging_value_consumer_statements` does for a return/store operand.
        if let Some(statements) = diverging_block_statements(ir, s) {
            new_stmts.extend(statements);
            changed = true;
            continue;
        }
        let statement = ir.exprs[s as usize].clone();
        // A checked inline/labeled return can leave `return { effects; return value }`: the inner
        // value-less block never falls through, so evaluating the outer consumer is impossible.
        // Lift its statements and drop the unreachable consumer. Apply the same semantic rule to
        // local/captured writes whose stable holder has no prior effect; this is the divergent twin
        // of the value-block normalization immediately below.
        if let Some(statements) = diverging_value_consumer_statements(ir, &statement) {
            new_stmts.extend(statements);
            changed = true;
            continue;
        }
        let spliced = match statement {
            IrExpr::Return(Some(inner)) => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::Return(Some(bv)))
                }),
            IrExpr::Variable {
                index,
                ty,
                init: Some(inner),
                named,
            } => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::Variable {
                        index,
                        ty,
                        init: Some(bv),
                        named,
                    })
                }),
            IrExpr::SetValue { var, value: inner } => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::SetValue { var, value: bv })
                }),
            IrExpr::RefSet {
                holder,
                elem,
                value: inner,
            } if matches!(ir.exprs[holder as usize], IrExpr::GetValue(_)) => value_block(ir, inner)
                .filter(|_| !ir.intrinsic_suspension_points.contains_key(&inner))
                .map(|(bs, bv)| {
                    new_stmts.extend(bs);
                    ir.add_expr(IrExpr::RefSet {
                        holder,
                        elem,
                        value: bv,
                    })
                }),
            _ => None,
        };
        match spliced {
            Some(ns) => {
                new_stmts.push(ns);
                changed = true;
            }
            None => new_stmts.push(s),
        }
    }
    // The block's own trailing value may itself be a value-carrying block whose statements must surface.
    let value = match value {
        Some(v) => match value_block(ir, v) {
            Some((bs, bv)) => {
                new_stmts.extend(bs);
                changed = true;
                Some(bv)
            }
            None => Some(v),
        },
        None => None,
    };
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
        // A lifted statement may itself carry a value-block (safe-call nested in elvis) — repeat.
        splice_return_blocks(ir, b);
    }
}

/// Statements of a structural block that cannot produce a value or fall through. Checked
/// coercions around it are observationally irrelevant because control never reaches the coercion.
fn diverging_control_core(ir: &IrFile, expression: ExprId) -> Option<ExprId> {
    if !stmt_diverges(ir, expression) {
        return None;
    }
    match &ir.exprs[expression as usize] {
        IrExpr::TypeOp { arg, .. } => diverging_control_core(ir, *arg),
        _ => Some(expression),
    }
}

fn diverging_block_statements(ir: &IrFile, expression: ExprId) -> Option<Vec<ExprId>> {
    match &ir.exprs[diverging_control_core(ir, expression)? as usize] {
        IrExpr::Block { stmts, value: None } => Some(stmts.clone()),
        _ => None,
    }
}

/// Statements of a value operand that can never return to its surrounding consumer. Local/value
/// declarations and local assignments have no separately evaluated target; a captured-cell write is
/// equally safe only when its holder is already a stable local read. In those cases the operand's
/// statements can replace the unreachable consumer without changing evaluation order.
fn diverging_value_consumer_statements(ir: &IrFile, consumer: &IrExpr) -> Option<Vec<ExprId>> {
    let operand = match consumer {
        IrExpr::Return(Some(operand))
        | IrExpr::Variable {
            init: Some(operand),
            ..
        }
        | IrExpr::SetValue { value: operand, .. } => *operand,
        IrExpr::RefSet { holder, value, .. }
            if matches!(ir.exprs[*holder as usize], IrExpr::GetValue(_)) =>
        {
            *value
        }
        _ => return None,
    };
    (!ir.intrinsic_suspension_points.contains_key(&operand))
        .then(|| diverging_block_statements(ir, operand))
        .flatten()
}

/// If `e` is a value-bearing `Block`, return `(its statements, its value)`; else `None`.
fn value_block(ir: &mut IrFile, e: ExprId) -> Option<(Vec<ExprId>, ExprId)> {
    match ir.exprs[e as usize].clone() {
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => {
            // The block is only an evaluation-order wrapper. When it is removed, retain the exact
            // checked result type on the expression that replaces it; later coroutine
            // normalization uses that metadata to allocate branch-result storage without
            // re-inferring a type from backend shapes.
            preserve_replacement_logical_type(ir, e, value);
            Some((stmts, value))
        }
        // Checked result coercions do not make a source grouping block semantically observable.
        // Lift the block's statements and reapply the exact operation to its value; no type or
        // conversion decision is repeated here.
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let (statements, value) = value_block(ir, arg)?;
            let value = ir.add_expr(IrExpr::TypeOp {
                op,
                arg: value,
                type_operand,
            });
            Some((statements, value))
        }
        _ => None,
    }
}

fn preserve_replacement_logical_type(ir: &mut IrFile, original: ExprId, replacement: ExprId) {
    if let Some(ty) = ir.logical_types.get(&original).copied() {
        ir.logical_types.entry(replacement).or_insert(ty);
    }
}

/// Rewrite each `return <suspend call>` in `b` into `val tmp = <suspend call>; return tmp` (a fresh
/// local typed `ret_ty`), so a tail-position suspension is handled as an ordinary bound-local
/// suspension point. Runs before the CPS rewrite, so `ret_ty` is the function's declared return type.
///
/// Descends into STATEMENT-position `when` arms and nested blocks: `when (n) { 0 -> return a() }` puts
/// the suspending `return` one level down, where the flattener (which models a suspending `Variable`
/// init, not a suspending `Return`) would otherwise bail out of the whole file.
fn desugar_tail_suspend(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts = Vec::with_capacity(stmts.len() + 1);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if is_suspension_point(ir, e, suspend_set) {
                let tmp = max_value_index(ir) + 1;
                let var = ir.add_expr(IrExpr::Variable {
                    index: tmp,
                    ty: ret_ty.clone(),
                    init: Some(e),
                    named: false,
                });
                let get = ir.add_expr(IrExpr::GetValue(tmp));
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(var);
                new_stmts.push(ret);
                changed = true;
                continue;
            }
        }
        desugar_returns_under(ir, s, suspend_set, ret_ty);
        new_stmts.push(s);
    }
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
    }
}

/// A checked block can retain an exhaustive conditional as its syntactic value even when every arm
/// returns or throws. Such a value never reaches the operand stack; make it an ordinary terminal
/// statement so suspension normalization can descend into its arms and the state-machine builder
/// does not mistake it for an unsupported value-producing tail.
fn promote_diverging_tail_to_statement(ir: &mut IrFile, body: ExprId) {
    let IrExpr::Block {
        mut stmts,
        value: Some(value),
    } = ir.exprs[body as usize].clone()
    else {
        return;
    };
    if !stmt_diverges(ir, value) {
        return;
    }
    stmts.push(value);
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
}

/// Apply [`desugar_tail_suspend`]'s rewrite to the `return`s nested inside a statement — the arms of a
/// statement-position `when`, and any block reachable through them.
///
/// A `Lambda` body is a SEPARATE state machine with its own return semantics, so it is never descended
/// into (matching the flattener's own treatment).
fn desugar_returns_under(ir: &mut IrFile, s: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    match ir.exprs[s as usize].clone() {
        IrExpr::Block { .. } => desugar_tail_suspend(ir, s, suspend_set, ret_ty),
        IrExpr::When { branches } => {
            for (_, body) in branches {
                // A bare `0 -> return a()` arm: the arm body IS the `Return`, so it becomes the
                // two-statement block the flattener can walk.
                if let IrExpr::Return(Some(e)) = ir.exprs[body as usize] {
                    if is_suspension_point(ir, e, suspend_set) {
                        let tmp = max_value_index(ir) + 1;
                        let var = ir.add_expr(IrExpr::Variable {
                            index: tmp,
                            ty: *ret_ty,
                            init: Some(e),
                            named: false,
                        });
                        let get = ir.add_expr(IrExpr::GetValue(tmp));
                        let ret = ir.add_expr(IrExpr::Return(Some(get)));
                        ir.exprs[body as usize] = IrExpr::Block {
                            stmts: vec![var, ret],
                            value: None,
                        };
                        continue;
                    }
                }
                desugar_returns_under(ir, body, suspend_set, ret_ty);
            }
        }
        _ => {}
    }
}

/// Desugar a VALUE-position `try` whose body suspends into a STATEMENT-position one binding a temp, so the
/// flattener (which models a `try` STATEMENT) can handle it: `return try { … } catch { … }` becomes
/// `var tmp = <default>; try { … tmp = <body value> } catch { … tmp = <catch value> }; return tmp`. A
/// suspending branch value is bound to a fresh `Variable` first (the flattener's `stmt_suspension` handles
/// a suspend `Variable` init, not a `SetValue`), then copied to `tmp`. Both `return <try>` and a bound
/// `val v = <try>` are rewritten (the latter targets the bound local directly), and a result COERCION
/// wrapping the `try` (`suspend fun f(): Base = try { sub() } …`) moves onto each selected branch, like
/// `desugar_value_when`'s branch wrap. A `SetValue` of a suspending `try` is left to skip the file.
fn desugar_value_try(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    for nested in owned_nested_blocks(ir, b) {
        desugar_value_try(ir, nested, suspend_set, ret_ty);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if let Some((decl, new_try, get)) =
                bind_value_try_to_fresh_local(ir, e, ret_ty, suspend_set)
            {
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(decl);
                new_stmts.push(new_try);
                new_stmts.push(ret);
                changed = true;
                continue;
            }
        }
        // The same value-position `try`, already bound to a local (`val v = try { … } …` — spelled
        // in source, or produced by an earlier pass binding an expression body's result): rewrite in
        // place, using the BOUND local as the branch-assignment target (`var v = <default>; try { …
        // v = <body value> } catch { … v = <catch value> }`).
        if let IrExpr::Variable {
            index,
            ty,
            init: Some(init),
            named,
        } = ir.exprs[s as usize].clone()
        {
            if let Some(parts) = suspending_value_try(ir, init, suspend_set) {
                let dflt = zero_value(ir, &ty);
                let decl = ir.add_expr(IrExpr::Variable {
                    index,
                    ty,
                    init: Some(dflt),
                    named,
                });
                let new_try = bind_value_try_to_local(ir, parts, index, &ty, suspend_set);
                new_stmts.push(decl);
                new_stmts.push(new_try);
                changed = true;
                continue;
            }
        }
        // Storage writes do not acquire their new value until the complete `try` expression returns.
        // Bind the selected body/catch result to a fresh local first, then perform the original write.
        // Local/static targets and a captured-cell holder read have no receiver effect to reorder.
        match ir.exprs[s as usize].clone() {
            IrExpr::SetValue { var, value } => {
                if let Some(ty) = ir.logical_types.get(&value).copied() {
                    if let Some((decl, new_try, get)) =
                        bind_value_try_to_fresh_local(ir, value, &ty, suspend_set)
                    {
                        new_stmts.push(decl);
                        new_stmts.push(new_try);
                        new_stmts.push(ir.add_expr(IrExpr::SetValue { var, value: get }));
                        changed = true;
                        continue;
                    }
                }
            }
            IrExpr::SetStatic { index, value } => {
                let ty = ir.statics.get(index as usize).map(|field| field.ty);
                if let Some(ty) = ty {
                    if let Some((decl, new_try, get)) =
                        bind_value_try_to_fresh_local(ir, value, &ty, suspend_set)
                    {
                        new_stmts.push(decl);
                        new_stmts.push(new_try);
                        new_stmts.push(ir.add_expr(IrExpr::SetStatic { index, value: get }));
                        changed = true;
                        continue;
                    }
                }
            }
            IrExpr::RefSet {
                holder,
                elem,
                value,
            } if matches!(ir.exprs[holder as usize], IrExpr::GetValue(_)) => {
                if let Some((decl, new_try, get)) =
                    bind_value_try_to_fresh_local(ir, value, &elem, suspend_set)
                {
                    new_stmts.push(decl);
                    new_stmts.push(new_try);
                    new_stmts.push(ir.add_expr(IrExpr::RefSet {
                        holder,
                        elem,
                        value: get,
                    }));
                    changed = true;
                    continue;
                }
            }
            _ => {}
        }
        new_stmts.push(s);
    }
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
    }
}

fn bind_value_try_to_fresh_local(
    ir: &mut IrFile,
    expression: ExprId,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> Option<(ExprId, ExprId, ExprId)> {
    let parts = suspending_value_try(ir, expression, suspend_set)?;
    let target = max_value_index(ir) + 1;
    let dflt = zero_value(ir, ty);
    let declaration = ir.add_expr(IrExpr::Variable {
        index: target,
        ty: *ty,
        init: Some(dflt),
        named: false,
    });
    // Publish the declaration before rewriting branches: a suspending branch may allocate another
    // temporary via `max_value_index`, which must not reuse the result slot.
    let value_try = bind_value_try_to_local(ir, parts, target, ty, suspend_set);
    let value = ir.add_expr(IrExpr::GetValue(target));
    Some((declaration, value_try, value))
}

/// Recognize the one semantic value-`try` shape supported by the state-machine desugar. The source
/// context (`return` versus a local initializer) is intentionally absent: both consumers must peel
/// the same optional result coercion and apply the same suspension predicate.
fn suspending_value_try(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<ValueTryParts> {
    // An expression body whose value coerces to its declared return type wraps the `Try` in a
    // `TypeOp`. Move that operation onto each selected branch, like `desugar_value_when`.
    let (try_expr, branch_wrap) = match ir.exprs[expression as usize].clone() {
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } if matches!(ir.exprs[arg as usize], IrExpr::Try { .. }) => {
            (arg, Some(ValueBranchWrap::TypeOp(op, type_operand)))
        }
        IrExpr::Call {
            callee,
            dispatch_receiver: None,
            args,
        } if matches!(&callee, Callee::Static { name, .. } if name == "box-impl")
            && matches!(args.as_slice(), [arg] if matches!(ir.exprs[*arg as usize], IrExpr::Try { .. })) =>
        {
            (args[0], Some(ValueBranchWrap::ValueClassBox(callee)))
        }
        _ => (expression, None),
    };
    if !expr_calls_suspend(ir, try_expr, suspend_set) {
        return None;
    }
    let IrExpr::Try {
        body,
        catches,
        finally,
        ..
    } = ir.exprs[try_expr as usize].clone()
    else {
        return None;
    };
    Some(ValueTryParts {
        body,
        catches,
        finally,
        branch_wrap,
    })
}

/// Turn the recognized value-`try` into a statement-position `try` assigning every value-producing
/// branch to `target`. This single reconstruction path prevents return-bound and local-bound forms
/// from drifting in catch/finally handling or coercion placement.
fn bind_value_try_to_local(
    ir: &mut IrFile,
    parts: ValueTryParts,
    target: u32,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> ExprId {
    let new_body = assign_branch_to_tmp(
        ir,
        parts.body,
        target,
        ty,
        suspend_set,
        parts.branch_wrap.clone(),
    );
    let new_catches: Vec<crate::ir::IrCatch> = parts
        .catches
        .into_iter()
        .map(|catch| crate::ir::IrCatch {
            var: catch.var,
            binding: catch.binding,
            exc_internal: catch.exc_internal,
            body: assign_branch_to_tmp(
                ir,
                catch.body,
                target,
                ty,
                suspend_set,
                parts.branch_wrap.clone(),
            ),
        })
        .collect();
    ir.add_expr(IrExpr::Try {
        body: new_body,
        catches: new_catches,
        finally: parts.finally,
        result: Ty::Unit,
    })
}

/// Desugar a VALUE-position `when`/`if` in `return` position whose BRANCH VALUES suspend (but whose
/// CONDITIONS do not — a suspending condition is hoisted earlier) into a STATEMENT-position `when` binding
/// a temp: `return when (x) { a -> v0; else -> v1 }` becomes `var tmp = <default>; when (x) { a -> { …
/// tmp = v0 }; else -> { … tmp = v1 } }; return tmp`. The flattener models a `when` STATEMENT with
/// suspending branch bodies (`emit_when_stmt`), so each branch's suspension surfaces there.
fn desugar_value_when(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    for nested in owned_nested_blocks(ir, b) {
        desugar_value_when(ir, nested, suspend_set, ret_ty);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if let Some((decl, new_when, get)) =
                bind_value_when_to_fresh_local(ir, e, ret_ty, suspend_set)
            {
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(decl);
                new_stmts.push(new_when);
                new_stmts.push(ret);
                changed = true;
                continue;
            }
        }
        new_stmts.push(s);
    }
    if changed {
        ir.exprs[b as usize] = IrExpr::Block {
            stmts: new_stmts,
            value,
        };
    }
}

/// Immediate nested block regions owned by `root`, excluding lambda bodies (which have an independent
/// value namespace and coroutine machine). Each returned block owns its deeper recursion, so shared
/// arena nodes are visited once per normalization entry without moving statements across a branch,
/// loop, or protected-region boundary.
fn owned_nested_blocks(ir: &IrFile, root: ExprId) -> Vec<ExprId> {
    let mut blocks = Vec::new();
    let mut pending = Vec::new();
    let mut seen = HashSet::from([root]);
    for_each_child(&ir.exprs, root, &mut |child| pending.push(child));
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.exprs[expression as usize] {
            IrExpr::Lambda { .. } => {}
            IrExpr::Block { .. } => blocks.push(expression),
            _ => for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    blocks
}

/// Bind a suspending value-`when` to one fresh typed local and return the declaration, the
/// statement-position conditional, and the final read. This is the one normalization shared by
/// returns and storage writes; callers decide only where the final read is consumed.
fn bind_value_when_to_fresh_local(
    ir: &mut IrFile,
    expression: ExprId,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> Option<(ExprId, ExprId, ExprId)> {
    // A result coercion belongs on each selected branch, after its suspension resumes.
    let (when_expr, branch_wrap) = match ir.exprs[expression as usize].clone() {
        IrExpr::When { .. } => (expression, None),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } if matches!(ir.exprs[arg as usize], IrExpr::When { .. }) => {
            (arg, Some(ValueBranchWrap::TypeOp(op, type_operand)))
        }
        _ => return None,
    };
    let suspends = expr_calls_suspend(ir, when_expr, suspend_set);
    let exits_loop = expr_contains_owned_loop_jump(ir, when_expr);
    if !suspends && !exits_loop {
        return None;
    }
    let IrExpr::When { branches } = ir.exprs[when_expr as usize].clone() else {
        return None;
    };
    let tmp = max_value_index(ir) + 1;
    let dflt = zero_value(ir, ty);
    let declaration = ir.add_expr(IrExpr::Variable {
        index: tmp,
        ty: *ty,
        init: Some(dflt),
        named: false,
    });
    let branches = branches
        .into_iter()
        .map(|(condition, body)| {
            (
                condition,
                assign_branch_to_tmp(ir, body, tmp, ty, suspend_set, branch_wrap.clone()),
            )
        })
        .collect();
    let conditional = ir.add_expr(IrExpr::When { branches });
    let value = ir.add_expr(IrExpr::GetValue(tmp));
    Some((declaration, conditional, value))
}

/// Rewrite a `try`/`catch` branch into a value-LESS block that runs its statements and assigns its VALUE
/// to `tmp`. A suspending value is bound to a fresh `Variable` (so the flattener handles the suspension),
/// then copied to `tmp`; a non-suspending value is assigned directly. A branch with no value (a divergent
/// `return`/`throw`) is left unchanged.
fn assign_branch_to_tmp(
    ir: &mut IrFile,
    branch: ExprId,
    tmp: u32,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
    wrap: Option<ValueBranchWrap>,
) -> ExprId {
    let (mut stmts, value) = match ir.exprs[branch as usize].clone() {
        IrExpr::Block { stmts, value } => (stmts, value),
        _ => (Vec::new(), Some(branch)),
    };
    if let Some(mut v) = value {
        if stmt_diverges(ir, v) {
            // A divergent branch VALUE (`else -> throw …`, `-> return …`, or a nested all-arms-divergent
            // `if`/`when`) produces no value to bind: emit it as a plain statement. Assigning it to `tmp`
            // would leave a dead `goto` after the `athrow`/`return` (a frameless VerifyError);
            // `stmt_diverges` on this same value suppresses that trailing goto.
            stmts.push(v);
        } else {
            if let Some(wrap) = wrap {
                v = match wrap {
                    ValueBranchWrap::TypeOp(op, type_operand) => ir.add_expr(IrExpr::TypeOp {
                        op,
                        arg: v,
                        type_operand,
                    }),
                    ValueBranchWrap::ValueClassBox(callee) => ir.add_expr(IrExpr::Call {
                        callee,
                        dispatch_receiver: None,
                        args: vec![v],
                    }),
                };
            }
            if let Some((declaration, value_try, value)) =
                bind_value_try_to_fresh_local(ir, v, ty, suspend_set)
            {
                stmts.push(declaration);
                stmts.push(value_try);
                stmts.push(ir.add_expr(IrExpr::SetValue { var: tmp, value }));
            } else if expr_calls_suspend(ir, v, suspend_set) {
                let fresh = max_value_index(ir) + 1;
                let var = ir.add_expr(IrExpr::Variable {
                    index: fresh,
                    ty: *ty,
                    init: Some(v),
                    named: false,
                });
                let get = ir.add_expr(IrExpr::GetValue(fresh));
                let set = ir.add_expr(IrExpr::SetValue {
                    var: tmp,
                    value: get,
                });
                stmts.push(var);
                stmts.push(set);
            } else {
                let set = ir.add_expr(IrExpr::SetValue { var: tmp, value: v });
                stmts.push(set);
            }
        }
    }
    ir.add_expr(IrExpr::Block { stmts, value: None })
}

/// Hoist each suspension call that sits at an *unconditional* position inside a top-level statement's
/// expression (e.g. `val a = foo() + 2`, `sum = sum + foo()`) into a preceding `val tmp = foo()`, so the
/// flattener only meets a suspension as a bound-local / bare statement (the positions it models). A
/// suspension inside a conditional sub-expression (an `if`/`when`/elvis/loop) is left in place — those
/// are handled structurally by the flattener (or skip the file if not yet modeled). Order of hoisted
/// temps follows left-to-right evaluation.
/// Whether `e` is an `if`/`when` EXPRESSION at least one of whose CONDITIONS calls a suspension — the
/// pure guard for the arms that route to [`hoist_when_cond_suspensions`].
fn when_cond_suspends(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    matches!(&ir.exprs[e as usize], IrExpr::When { branches }
        if branches.iter().any(|(c, _)| c.is_some_and(|c| expr_calls_suspend(ir, c, suspend_set))))
}

fn value_when(ir: &IrFile, expression: ExprId) -> Option<ExprId> {
    match ir.exprs[expression as usize] {
        IrExpr::When { .. } => Some(expression),
        IrExpr::TypeOp { arg, .. } if matches!(ir.exprs[arg as usize], IrExpr::When { .. }) => {
            Some(arg)
        }
        _ => None,
    }
}

/// Expose a checked conversion wrapped around a value conditional by applying that same operation
/// to each selected branch value. This is a structural equality:
/// `convert(if (c) a else b)` becomes `if (c) convert(a) else convert(b)`; branch statements and
/// divergent branches remain inside their original control-flow region.
fn normalize_value_when(ir: &mut IrFile, expression: ExprId) -> Option<ExprId> {
    match ir.exprs[expression as usize].clone() {
        IrExpr::When { .. } => Some(expression),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let IrExpr::When { branches } = ir.exprs[arg as usize].clone() else {
                return None;
            };
            let branches = branches
                .into_iter()
                .map(|(condition, body)| {
                    let body = if let Some((stmts, value)) = value_block(ir, body) {
                        let value = ir.add_expr(IrExpr::TypeOp {
                            op,
                            arg: value,
                            type_operand,
                        });
                        ir.add_expr(IrExpr::Block {
                            stmts,
                            value: Some(value),
                        })
                    } else if matches!(ir.exprs[body as usize], IrExpr::Block { value: None, .. }) {
                        body
                    } else {
                        ir.add_expr(IrExpr::TypeOp {
                            op,
                            arg: body,
                            type_operand,
                        })
                    };
                    (condition, body)
                })
                .collect();
            Some(ir.add_expr(IrExpr::When { branches }))
        }
        _ => None,
    }
}

/// Snapshot the semantic types in one function's value-index namespace before suspend hoisting.
/// `IrFile::exprs` is a module-wide arena while `GetValue(n)` is function-local, so any type query
/// based on a global scan is inherently ambiguous. Nested lambda bodies own another namespace and are
/// deliberately skipped; only their capture expressions still belong to the enclosing function.
/// Whether emission may own this function's coroutine machine.
///
/// A static function always may. An instance method may when the continuation can call it back and
/// be sure of reaching this very body: `invokevirtual` on an OPEN method would land in an override,
/// and a private method is not accessible from the continuation class at all.
fn machine_eligible(ir: &IrFile, fid: u32) -> bool {
    let function = &ir.functions[fid as usize];
    if function.is_static {
        return true;
    }
    let owner_is_interface = function.dispatch_receiver.as_ref().is_some_and(|receiver| {
        ir.classes
            .iter()
            .any(|class| class.fq_name_matches(&receiver.render()) && class.is_interface)
    });
    function.dispatch_receiver.is_some() && !owner_is_interface && !ir.open_methods.contains(&fid)
}

fn function_value_types(ir: &IrFile, fid: u32, body: ExprId) -> HashMap<u32, Ty> {
    function_value_types_with(ir, fid, &ir.functions[fid as usize].params, body)
}

/// The value-type table of a lambda's `inline_body`, numbered as the impl method is: captures, then
/// the lambda's own parameters, then the locals the body declares. A `suspend`-typed lambda may
/// already have been through this pass — it precedes the frame it is spliced into in `suspend_funs`
/// — and then carries a trailing `Continuation` at the index its body's first local uses. The
/// declared signature is the one the body was numbered against.
pub(super) fn spliced_body_value_types(
    ir: &IrFile,
    impl_fn: u32,
    body: ExprId,
) -> HashMap<u32, Ty> {
    let params = ir
        .suspend_declared_sigs
        .get(&impl_fn)
        .map(|(params, _)| params.as_slice())
        .unwrap_or(&ir.functions[impl_fn as usize].params);
    function_value_types_with(ir, impl_fn, params, body)
}

fn function_value_types_with(
    ir: &IrFile,
    fid: u32,
    params: &[Ty],
    body: ExprId,
) -> HashMap<u32, Ty> {
    fn collect(ir: &IrFile, expression: ExprId, out: &mut HashMap<u32, Ty>) {
        match &ir.exprs[expression as usize] {
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                out.entry(*index).or_insert(*ty);
                if let Some(init) = init {
                    collect(ir, *init, out);
                }
            }
            IrExpr::Lambda { captures, .. } => {
                for &capture in captures {
                    collect(ir, capture, out);
                }
            }
            _ => for_each_child(&ir.exprs, expression, &mut |child| collect(ir, child, out)),
        }
    }

    let function = &ir.functions[fid as usize];
    let physical_receiver = function.dispatch_receiver.filter(|_| !function.is_static);
    let receiver_offset = u32::from(physical_receiver.is_some());
    let mut out = HashMap::new();
    if let Some(receiver) = physical_receiver {
        out.insert(0, Ty::obj_name(receiver));
    }
    for (index, ty) in params.iter().copied().enumerate() {
        out.insert(receiver_offset + index as u32, ty);
    }
    // A generated `SuspendLambda.invokeSuspend` reloads captures and own lambda parameters from the
    // lambda object's field prefix when entering each state. Before state-machine construction those
    // values are represented by semantic locals starting at index 2 (`this`, `result`, then fields),
    // without synthetic `Variable` nodes. Seed their exact types from the recorded lambda/class shape
    // so operand-order hoisting can snapshot reads such as the receiver of `this.result = susp()`.
    if let Some((_, class, field_base)) = ir
        .suspend_lambda_sm
        .iter()
        .find(|(lambda_fid, _, _)| *lambda_fid == fid)
    {
        if let Some(class) = ir.classes.get(*class as usize) {
            for (field, ty) in class
                .fields
                .iter()
                .take(*field_base as usize)
                .map(|field| field.ty)
                .enumerate()
            {
                out.insert(2 + field as u32, ty);
            }
        }
    }
    collect(ir, body, &mut out);
    out
}

/// For a same-file suspend call, the callee `FunId` — used to recover the callee's LOGICAL return type
/// (its index into `orig_rets`). Handles a static call (`Call{Local}`) and a same-file member call
/// (`MethodCall`, whose `FunId` is the class's method at `index`). Returns `None` for a cross-unit
/// suspend call (a `Callee::Static` to another file / the classpath) — that call has no local `FunId`;
/// its logical type comes from `ir.suspend_calls` instead.
fn suspend_call_fid(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> Option<u32> {
    match &ir.exprs[e as usize] {
        IrExpr::Call {
            callee: Callee::Local(fid),
            ..
        } if suspend_set.contains(fid) => Some(*fid),
        IrExpr::Call {
            callee: Callee::ClassStatic { function, .. },
            ..
        } if suspend_set.contains(function) => Some(*function),
        IrExpr::MethodCall { class, index, .. } => {
            let fid = *ir.classes[*class as usize].methods.get(*index as usize)?;
            suspend_set.contains(&fid).then_some(fid)
        }
        _ => None,
    }
}

/// Whether evaluating `e` is one atomic SUSPENSION POINT: either a direct suspend-function call
/// (same-file via [`suspend_call_fid`], or cross-unit via `ir.suspend_calls`) or an inlined intrinsic
/// expression recorded in `ir.intrinsic_suspension_points`. The latter already contains its
/// continuation behavior, so [`append_continuation`] deliberately leaves its non-call node unchanged.
fn is_suspension_point(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    suspend_call_fid(ir, e, suspend_set).is_some()
        || ir.suspend_calls.contains_key(&e)
        || ir.intrinsic_suspension_points.contains_key(&e)
}

/// The logical source result of a non-local call or intrinsic suspension point. Same-file calls use
/// `orig_rets` through [`suspend_call_fid`]; the two side maps cover only nodes whose result cannot be
/// recovered from a local `FunId`.
fn recorded_suspension_result(ir: &IrFile, e: ExprId) -> Option<Ty> {
    ir.suspend_calls.get(&e).copied().or_else(|| {
        ir.intrinsic_suspension_points
            .get(&e)
            .map(|point| point.result)
    })
}

fn value_class_suspension_result(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<crate::ir::IrValueClassSuspendResult> {
    suspend_call_fid(ir, e, suspend_set)
        .and_then(|fid| ir.value_class_suspend_returns.get(&fid).copied())
        .or_else(|| ir.value_class_suspend_calls.get(&e).copied())
}

fn when_has_non_direct_suspending_branch(
    ir: &IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
) -> bool {
    let Some(when) = value_when(ir, expression) else {
        return false;
    };
    let IrExpr::When { branches } = &ir.exprs[when as usize] else {
        return false;
    };
    branches.iter().any(|(_, branch)| {
        expr_calls_suspend(ir, *branch, suspend_set)
            && !is_suspension_point(
                ir,
                unwrap_suspend_cast(ir, *branch, suspend_set, false).point,
                suspend_set,
            )
    })
}

/// Whether EXACTLY ONE suspension point is reachable from `e`. Iterative with a visited set (the expr arena
/// can share/deeply-nest nodes — a recursive walk overflows the stack) and early-exits once a second is
/// seen (the caller only needs the "== 1" answer).
fn exactly_one_suspension_point(ir: &IrFile, e: ExprId, set: &HashSet<u32>) -> bool {
    let mut seen: HashSet<ExprId> = HashSet::new();
    let mut stack = vec![e];
    let mut count = 0usize;
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if is_suspension_point(ir, cur, set) {
            count += 1;
            if count > 1 {
                return false;
            }
        }
        for_each_child(&ir.exprs, cur, &mut |c| stack.push(c));
    }
    count == 1
}

/// If the suspend body is a pure TAIL suspension — its result is one direct point in tail position and
/// NOTHING else in the body suspends — return that point's `ExprId`. A callable point forwards its own
/// `$completion` to the callee; an inlined intrinsic point already uses that completion internally. Both
/// return the resulting `Object` directly without a continuation class. Detected before
/// `desugar_tail_suspend`, which would otherwise bind the tail into a resume point and force a machine.
/// Conservative: only a plain `return <point>` / trailing-value shape, never `if`/`when`/multi-return.
fn tail_forward_call(
    ir: &IrFile,
    b: ExprId,
    set: &HashSet<u32>,
    unit_ret: bool,
    orig_rets: &[Ty],
) -> Option<ExprId> {
    if !exactly_one_suspension_point(ir, b, set) {
        return None;
    }
    fn tail_expression(
        ir: &IrFile,
        expression: ExprId,
        set: &HashSet<u32>,
        unit_ret: bool,
        orig_rets: &[Ty],
    ) -> Option<ExprId> {
        let tail = match &ir.exprs[expression as usize] {
            IrExpr::Return(Some(e)) => *e,
            IrExpr::Block { value: Some(v), .. }
                if matches!(ir.exprs[*v as usize], IrExpr::Block { .. }) =>
            {
                return tail_expression(ir, *v, set, unit_ret, orig_rets);
            }
            IrExpr::Block { value: Some(v), .. } => *v,
            IrExpr::Block {
                value: None, stmts, ..
            } => match stmts.last() {
                Some(&last) => match ir.exprs[last as usize] {
                    IrExpr::Return(Some(e)) => e,
                    // A `Unit` fn whose LAST statement is a BARE `Unit` suspend call
                    // (`suspend fun delete(id) { repository.delete(id) }`) — kotlinc forwards it identically
                    // (`areturn` the callee's Object result: COROUTINE_SUSPENDED or the boxed `Unit`). Gated
                    // on the CALLEE returning `Unit` too, so the forwarded value is what the caller expects.
                    _ if unit_ret
                        && is_suspension_point(ir, last, set)
                        && suspension_ret_unit(ir, last, set, orig_rets) =>
                    {
                        last
                    }
                    // FIR preserves source grouping as nested statement-only blocks. They do not alter
                    // control flow or evaluation order, so tail position passes through them exactly as
                    // it does through the equivalent flattened block.
                    _ if matches!(ir.exprs[last as usize], IrExpr::Block { .. }) => {
                        return tail_expression(ir, last, set, unit_ret, orig_rets);
                    }
                    _ => return None,
                },
                None => return None,
            },
            _ => return None,
        };
        Some(tail)
    }
    let tail = tail_expression(ir, b, set, unit_ret, orig_rets)?;
    // A generic suspend call's erased result is cast to the declared type at the call site; a
    // tail-forward returns the callee's Object result verbatim (no checkcast), so peel the wrapper.
    let tail = unwrap_suspend_cast(ir, tail, set, /* ref_only */ true).point;
    is_suspension_point(ir, tail, set).then_some(tail)
}

/// Whether suspension point `e`'s LOGICAL result is `Unit` — a same-file callee via its declared return,
/// or a recorded cross-unit/intrinsic point via its semantic side map.
fn suspension_ret_unit(ir: &IrFile, e: ExprId, set: &HashSet<u32>, orig_rets: &[Ty]) -> bool {
    if let Some(fid) = suspend_call_fid(ir, e, set) {
        return orig_rets.get(fid as usize) == Some(&Ty::Unit);
    }
    recorded_suspension_result(ir, e).as_ref() == Some(&Ty::Unit)
}

/// Rewrite the body's tail so it `return`s the forwarded suspend call directly — the CPS `Object` result,
/// unboxed and unwrapped (no state machine). A trailing VALUE is promoted to a `Return`; a BARE trailing
/// call STATEMENT (the `Unit` forward) is replaced with `return <call>`; an existing return has its
/// operand replaced too, because checked bottom completion may wrap the physical call.
fn make_forward_body(ir: &mut IrFile, b: ExprId, call: ExprId) {
    match ir.exprs[b as usize].clone() {
        // Before checked bottom completion was explicit, an existing tail return already held
        // `call`. It may now hold `BottomValue(call)`: replace that semantic boundary just like a
        // block value, because this frame forwards the physical CPS Object and its caller owns the
        // resumed completion.
        IrExpr::Return(Some(_)) => {
            ir.exprs[b as usize] = IrExpr::Return(Some(call));
        }
        IrExpr::Block {
            stmts,
            value: Some(_),
        } => {
            // Return the PEELED `call`, not the block's value expr — the value is the callee call still
            // wrapped in the redundant reference `Cast` that `tail_forward_call` stripped for detection.
            // A tail-forward `areturn`s the callee's `Object` result verbatim (no `checkcast`); returning
            // the wrapper would re-emit the cast that kotlinc omits.
            let mut stmts = stmts;
            stmts.push(ir.add_expr(IrExpr::Return(Some(call))));
            ir.exprs[b as usize] = IrExpr::Block { stmts, value: None };
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
            ir.exprs[b as usize] = IrExpr::Block { stmts, value: None };
        }
        _ => {}
    }
}

/// The CPS form of a logical method descriptor: append the trailing `Continuation` parameter and erase
/// the return to `Object` — `()I` → `(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`. A
/// cross-unit suspend callee is *resolved* by its logical signature (no continuation, real return), but
/// the emitted `invokestatic` must name the callee's physical CPS descriptor.
fn cps_descriptor(logical: &str) -> String {
    let close = logical
        .rfind(')')
        .unwrap_or(logical.len().saturating_sub(1));
    format!(
        "{}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        &logical[..close]
    )
}

/// Locate the continuation slot in a suspend `$default` descriptor. Its ABI suffix is
/// `Continuation, int mask..., Object marker`; scanning the typed signature keeps both continuation
/// insertion and operand spilling independent of source arity and of the number of mask words.
fn default_suspend_continuation_index(params: &[Ty]) -> Option<usize> {
    let mut index = params.len().checked_sub(2)?;
    let mut masks = 0;
    while params.get(index).copied() == Some(Ty::Int) {
        masks += 1;
        index = index.checked_sub(1)?;
    }
    (masks > 0
        && params
            .get(index)
            .and_then(|ty| ty.obj_internal())
            .is_some_and(|name| name.matches("kotlin/coroutines/Continuation")))
    .then_some(index)
}

/// Append the continuation `cont` as the trailing argument of suspend call `call_e` (a `Call` or
/// `MethodCall`) — the CPS parameter the callee now expects. For a cross-unit `Callee::Static` (resolved
/// by its logical signature), also rewrite the descriptor to the physical CPS form so the emitted
/// `invokestatic` matches the callee. Returns the (unchanged) `ExprId`.
fn append_continuation(
    ir: &mut IrFile,
    call_e: ExprId,
    cont: ExprId,
    default_call_operands: &mut crate::jvm::default_call_operands::DefaultCallOperands,
) -> bool {
    crate::trace_compiler!(
        "suspend",
        "append continuation call={call_e} continuation={cont} node={:?}",
        ir.exprs.get(call_e as usize),
    );
    let planned_index = match default_call_operands.insert_continuation(call_e, cont) {
        Ok(index) => index,
        Err(()) => {
            crate::trace_compiler!(
                "suspend",
                "append continuation BAIL: default operand plan has no ABI suffix call={call_e}"
            );
            return false;
        }
    };
    match &mut ir.exprs[call_e as usize] {
        IrExpr::Call {
            args,
            callee: Callee::Static { descriptor, .. },
            ..
        } => {
            // A selected default bridge already spells the `Continuation` in its descriptor — BEFORE
            // the trailing mask words and marker. The recorded operand plan identifies that semantic
            // realization without recovering it from the emitted method name.
            if let Some(planned_index) = planned_index {
                let Some((params, _)) = crate::jvm::ir_emit::parse_physical_method_desc(descriptor)
                else {
                    crate::trace_compiler!(
                        "suspend",
                        "append continuation BAIL: invalid default descriptor call={call_e} descriptor={descriptor}"
                    );
                    return false;
                };
                let Some(index) = default_suspend_continuation_index(&params) else {
                    crate::trace_compiler!(
                        "suspend",
                        "default suspend call={call_e} has no continuation slot descriptor={descriptor} params={params:?}"
                    );
                    return false;
                };
                if planned_index != index || index > args.len() {
                    crate::trace_compiler!(
                        "suspend",
                        "append continuation BAIL: operand-plan boundary mismatch call={call_e} planned={planned_index} descriptor={index} args={} descriptor_text={descriptor}",
                        args.len()
                    );
                    return false;
                }
                crate::trace_compiler!(
                    "suspend",
                    "insert default continuation call={call_e} index={index} descriptor={descriptor} args_before={args:?}"
                );
                args.insert(index, cont);
            } else {
                *descriptor = cps_descriptor(descriptor);
                args.push(cont);
            }
        }
        // A sibling-file suspend callee: its CPS signature appends a `Continuation` parameter and erases
        // the return to `Object` (the JVM backend builds the descriptor from these `Ty`s).
        IrExpr::Call {
            args,
            callee:
                Callee::CrossFile {
                    params,
                    ret,
                    module_default_call,
                    ..
                },
            ..
        } => {
            if *module_default_call {
                let Some(index) = planned_index else {
                    return false;
                };
                if index > args.len() || index > params.len() {
                    return false;
                }
                params.insert(index, continuation_ty());
                args.insert(index, cont);
            } else {
                if planned_index.is_some() {
                    return false;
                }
                params.push(continuation_ty());
                args.push(cont);
            }
            *ret = object_ty();
        }
        IrExpr::Call {
            args,
            callee: Callee::Virtual {
                descriptor, params, ..
            },
            ..
        } => {
            if planned_index.is_some() {
                return false;
            }
            if let Some((params, ret)) = params {
                params.push(continuation_ty());
                *ret = object_ty();
            } else {
                *descriptor = cps_descriptor(descriptor);
            }
            args.push(cont);
        }
        IrExpr::Call {
            args,
            callee: Callee::LocalDefault(_) | Callee::ClassStaticDefault { .. },
            ..
        } => {
            let Some(index) = planned_index else {
                return false;
            };
            if index > args.len() {
                return false;
            }
            args.insert(index, cont);
        }
        IrExpr::Call { args, .. } => {
            if planned_index.is_some() {
                return false;
            }
            args.push(cont);
        }
        IrExpr::MethodCall { args, .. } => {
            if planned_index.is_some() {
                return false;
            }
            args.push(Some(cont));
        }
        // A suspend function VALUE call (`block(a)`): the value implements `Function{N+1}`, so append the
        // continuation — the emitter picks `Function{N+1}.invoke` from the arg count. The CPS result is
        // the raw erased `Object` (COROUTINE_SUSPENDED or the boxed value): erase `ret` so the emitter
        // does NOT unbox it — a tail-forward `areturn`s it verbatim, and the flattener re-applies the
        // logical coercion from `ir.suspend_calls` when it binds the resume value.
        IrExpr::InvokeFunction {
            args, params, ret, ..
        } => {
            if planned_index.is_some() {
                return false;
            }
            *ret = object_ty();
            args.push(cont);
            params.push(continuation_ty());
        }
        _ => {
            if planned_index.is_some() {
                return false;
            }
        }
    }
    true
}

/// Whether `e`'s subtree contains any call to a suspend function (used to reject shapes this pass can't
/// restructure — a suspend call nested in an expression, a branch, a loop, etc.).
fn expr_calls_suspend(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if is_suspension_point(ir, e, suspend_set) {
        return true;
    }
    // Constructing a lambda evaluates its captures, but its body belongs to the generated invoke
    // method and cannot suspend the enclosing body. `for_each_child` deliberately exposes the
    // `inline_body` for generic IR transforms, so suspension analysis must enforce this semantic
    // ownership boundary itself.
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        return captures
            .iter()
            .any(|&capture| expr_calls_suspend(ir, capture, suspend_set));
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if expr_calls_suspend(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

/// How many SUSPEND functions sharing this one's continuation NAME the file declares before it.
///
/// A continuation class is named after the method it re-enters, so two overloads would share one —
/// and they do not share a spill layout, so whichever class loses the name resumes against fields it
/// does not have (`NoSuchFieldError`). Both machines number the later one.
///
/// This answers for a suspend function that has NO source declaration behind it — a lowering-made
/// one, which no frontend pass could have reserved a position for. A declared function reads its
/// position from [`continuation_ordinal`] instead of counting anything here.
fn same_name_ordinal(ir: &IrFile, fid: u32) -> usize {
    let function = &ir.functions[fid as usize];
    let bare = |name: &str| name.split('-').next().unwrap_or(name).to_string();
    let name = bare(&function.name);
    ir.functions
        .iter()
        .enumerate()
        .take(fid as usize)
        .filter(|(other_fid, other)| {
            bare(&other.name) == name
                && other.dispatch_receiver == function.dispatch_receiver
                && ir.suspend_funs.contains(&(*other_fid as u32))
        })
        .count()
}

/// The 1-based `$N` the continuation class of `fid` takes in its `<owner>$<function>` sequence.
///
/// The sequence is shared with the anonymous objects those bodies declare, and the pass that names
/// those objects is the one that leaves a position free for each suspend function, in declaration
/// order. It publishes which position it left — `IrFile::fn_continuation_ordinal` — so there is one
/// numbering, computed once. Nothing here re-derives it from a class name.
pub(crate) fn continuation_ordinal(ir: &IrFile, fid: u32) -> usize {
    match ir.fn_continuation_ordinal.get(&fid) {
        Some(&ordinal) => ordinal as usize,
        None => {
            assert!(
                !ir.fn_source_order.contains_key(&fid),
                "source suspend function {fid} has no published continuation ordinal"
            );
            // A lowering-made function has no source declaration, so no anonymous source object
            // can consume its generated sequence. Its target-private overloads number themselves.
            same_name_ordinal(ir, fid) + 1
        }
    }
}

/// The continuation class for an ordinal from [`continuation_ordinal`]. The one place a generated
/// continuation class is spelled.
pub(crate) fn continuation_class_name(owner: &str, function: &str, ordinal: usize) -> String {
    format!("{owner}${function}${ordinal}")
}

/// Build the coroutine state machine for `fid` (whose body `b` is a top-level block). The body is
/// flattened into a state graph: each suspension point (including one inside an `if`/`when` branch value)
/// ends a state and starts a resume state, and control flow becomes `label = next` transitions through a
/// `while(true){ r = cont.result; <restore spilled>; when(label){ states } else throw }` dispatch loop. A
/// local live across any suspension point is spilled to a continuation field (restored at the loop top so
/// its slot is frame-consistent on every dispatch path). Returns `false` (skip, never miscompile) for a
/// shape the flattener doesn't handle yet (a suspension nested deeper than a branch value, in a loop, …).
#[allow(clippy::too_many_arguments)]
fn build_state_machine(
    ir: &mut IrFile,
    facade: &str,
    fid: u32,
    b: ExprId,
    unit_ret: bool,
    orig_rets: &[Ty],
    captured_scopes: Option<SuspensionScopes>,
    suspension_lines: &std::collections::HashMap<ExprId, (u32, u32)>,
    continuation_metadata: &mut ContinuationMetadataMap,
    default_call_operands: &mut crate::jvm::default_call_operands::DefaultCallOperands,
) -> bool {
    crate::trace_compiler!(
        "suspend",
        "build_state_machine fid={fid} input={:?}",
        ir.exprs[b as usize]
    );
    // Normalize a block-valued initializer (`val a = (x ?: foo())`, `a?.b ?: foo()` — elvis / safe-call
    // lower to `Variable{ init: Block{ prelude…, value: When } }`) into `prelude…; Variable{ init: When }`,
    // so the conditional suspension surfaces as a `Variable{init: When}` the flattener handles.
    normalize_block_inits(ir, b);
    split_unit_conditional_returns(ir, b, unit_ret);
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    // The finally/non-local-return normalizers run after the initial hoist and can expose new
    // statement consumers around already-checked inline bodies. Normalize once more at the actual
    // state-machine boundary, where the body has its final control-flow shape but suspend callees
    // still retain their snapshotted declared result types in `orig_rets`.
    let return_ty = orig_rets.get(fid as usize).copied().unwrap_or(Ty::Unit);
    desugar_value_try(ir, b, &suspend_set, &return_ty);
    desugar_value_when(ir, b, &suspend_set, &return_ty);
    normalize_statement_try_results(ir, b, true);
    normalize_block_inits(ir, b);
    let mut value_types = function_value_types(ir, fid, b);
    hoist_suspensions(ir, b, &suspend_set, orig_rets, &mut value_types);
    // Give the body a terminal `return`. A value-less body that FALLS THROUGH (a `Unit` fn whose last
    // statement is a suspension / loop, with no explicit `return`) needs `return Unit.INSTANCE` —
    // otherwise its final resume state runs off the end of the `when(label)` dispatch, falls back to the
    // `while(true)` top, and re-dispatches the same label forever (a coroutine that never completes). A
    // trailing-VALUE body (an `= withLock { … }` expression body whose result survived as the block
    // value) needs `return <value>`. `ensure_tail_return` handles both. EXCEPT a trailing value that
    // itself SUSPENDS: it isn't desugared into a bound-local suspension point, so converting it would
    // emit an unmodeled `return <suspend call>` — leave that to the `value.is_some()` bail below.
    let convert_tail = match &ir.exprs[b as usize] {
        IrExpr::Block { value: None, .. } => true,
        IrExpr::Block { value: Some(v), .. } => !expr_calls_suspend(ir, *v, &suspend_set),
        _ => false,
    };
    if convert_tail {
        ensure_tail_return(ir, b, unit_ret);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: body not a Block"
        );
        return false;
    };
    if value.is_some() {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: block has a trailing value (suspend body must use `return`)"
        );
        trace_residual_suspension(ir, b, 0);
        return false; // a suspending trailing-value body isn't modeled (desugar to a `return` first)
    }
    // A SUSPENSION reached through `invokespecial` (`super.suspendHere(x)`): the machine would have to
    // thread the continuation through a non-virtual dispatch AND resume back into it, which the
    // resume path does not model — the resumed frame read back `null`
    // (`coroutines/suspendFunctionAsCoroutine/superCall*`). Skip the file, never miscompile.
    if suspends_through_super(ir, b, &suspend_set) {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: super suspension"
        );
        return false;
    }
    // Spilled locals: any local read at or after the first statement that contains a suspension — a
    // sound over-approximation of "live across a suspension point". Each maps to its declared type.
    let Some(first) = stmts
        .iter()
        .position(|&s| expr_calls_suspend(ir, s, &suspend_set))
    else {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: no suspension found"
        );
        return false; // caller guarantees a suspension exists
    };
    let mut reads: Vec<u32> = Vec::new();
    for &s in &stmts[first..] {
        collect_reads(ir, s, &mut reads);
    }
    reads.sort_unstable();
    reads.dedup();

    // Keep semantic ownership separate from physical JVM layout. A value-class member retains its
    // owner in `dispatch_receiver` so it remains a class member and names its continuation correctly,
    // but value-class lowering has already made it static and inserted the carrier as parameter zero.
    // Such a method has no JVM `this` slot and its continuation must not capture one.
    let semantic_owner: Option<TypeName> = ir.functions[fid as usize].dispatch_receiver;
    let receiver: Option<TypeName> =
        semantic_owner.filter(|_| !ir.functions[fid as usize].is_static);
    let this_offset = u32::from(receiver.is_some());
    // Real value parameters (excluding the appended CPS `Continuation`), at value-indices
    // `this_offset .. this_offset + real_params.len()`.
    let real_params: Vec<Ty> = {
        let p = &ir.functions[fid as usize].params;
        p[..p.len().saturating_sub(1)]
            .iter()
            .enumerate()
            .map(|(parameter, ty)| suspend_parameter_ty(ir, fid, parameter, *ty))
            .collect()
    };
    let completion_idx = real_params.len() as u32 + this_offset;
    // Type of a value PARAMETER at value-index `idx` (not `this`, not the continuation). A param read
    // across a suspension is spilled like a local, but — being live on ENTRY — the loop-top restore on
    // the first iteration would clobber it with the (still-unset) field; so the continuation also
    // CAPTURES it at construction (see `build_get_or_create` / `build_continuation_class`).
    let param_ty = |idx: u32| -> Option<Ty> {
        let hi = this_offset + real_params.len() as u32;
        (idx >= this_offset && idx < hi).then(|| real_params[(idx - this_offset) as usize].clone())
    };
    // A local whose EVERY reference lies strictly AFTER the last top-level suspending statement is not
    // live across any suspension — e.g. the iterator/counter of a STRUCTURAL (non-suspending) loop that
    // runs entirely in the final resume state. Spilling it is unsound: the spill layout gives it a
    // continuation-field restore slot, but the tail's own local allocator numbers the same value a
    // different slot, so the structural loop's back-edge stackmap frame disagrees (`locals[N]=top` vs
    // `Iterator`) → VerifyError. Retain only a value-index that is a PARAMETER or is WRITTEN somewhere up
    // to and including the last suspending statement (so it genuinely predates a suspension a later read
    // crosses). A loop-body local of the last suspending statement (itself a suspending loop) is written
    // inside that statement's subtree, so it is correctly kept.
    let last_susp = stmts
        .iter()
        .rposition(|&s| expr_calls_suspend(ir, s, &suspend_set))
        .unwrap_or(first);
    let mut head_writes: Vec<u32> = Vec::new();
    for &s in &stmts[..=last_susp] {
        collect_live_writes(ir, s, &suspend_set, &mut head_writes);
    }
    head_writes.sort_unstable();
    head_writes.dedup();
    reads.retain(|idx| param_ty(*idx).is_some() || head_writes.binary_search(idx).is_ok());
    // Named source variables declared anywhere in the body — the kotlinc scope-spill rule (below)
    // applies only to these; a compiler temp follows pure liveness.
    let mut named_vars: HashSet<u32> = HashSet::new();
    collect_named_vars(ir, b, &mut named_vars);
    // The coarse rule above keeps any local read at-or-after the FIRST suspending statement, but a
    // COMPILER TEMP (elvis/safe-call materialization, a suspension hoist) is a value kotlinc holds
    // on the operand stack — no `L$N` slot — unless a suspension genuinely sits between its write
    // and a read. Statement-level crossing check: a suspension inside the temp's OWN initializer
    // runs BEFORE the store (no read crosses it); a suspending statement strictly between the write
    // statement and a read statement DOES cross; a read inside a suspending statement other than
    // that proven-safe binding shape has unknowable intra-statement order — keep conservatively.
    // (Named source variables keep the coarse treatment; kotlinc spills them by SCOPE anyway.)
    let susp_outside_init = |wst: ExprId, idx: u32| -> bool {
        let in_stmt = count_suspensions(ir, wst, &suspend_set);
        let in_init =
            find_var_init(ir, wst, idx).map_or(0, |init| count_suspensions(ir, init, &suspend_set));
        in_stmt != in_init
    };
    let before_crossing = reads.clone();
    reads.retain(|&idx| {
        if param_ty(idx).is_some() || named_vars.contains(&idx) {
            return true;
        }
        let Some(wi) = stmts.iter().position(|&st| stmt_writes(ir, st, idx)) else {
            return true;
        };
        for (ri, &st) in stmts.iter().enumerate() {
            if ri < wi || !expr_reads(ir, st, idx) {
                continue;
            }
            if ri == wi {
                // Read and write share a statement: safe only when every suspension in it is the
                // temp's own initializer (evaluated before the store).
                if susp_outside_init(st, idx) {
                    return true;
                }
                continue;
            }
            // A statement between the write and this read that OVERWRITES `idx` on EVERY path
            // before reading it kills the earlier value: a `when` assigning the temp in each of its
            // branches leaves nothing of the declaration's value to carry, and the suspensions
            // inside it all run BEFORE their branch's store. The crossing test restarts from that
            // statement — only a suspension AFTER it can make the surviving value cross.
            let (wi, rebased) = stmts[wi + 1..ri]
                .iter()
                .rposition(|&s2| kills_value(ir, s2, idx, &suspend_set))
                .map_or((wi, false), |k| (wi + 1 + k, true));
            // A suspending statement strictly between the write and this read → the value crosses.
            if stmts[wi + 1..ri]
                .iter()
                .any(|&s2| expr_calls_suspend(ir, s2, &suspend_set))
            {
                return true;
            }
            // The write statement suspends outside the initializer → crossing, keep. A REBASED write
            // statement suspends only before its own stores, by `kills_value`'s own definition.
            if !rebased && susp_outside_init(stmts[wi], idx) {
                return true;
            }
            // The read statement itself suspends: a read INSIDE a suspend call's own subtree
            // (receiver/arguments) evaluates BEFORE the suspension — kotlinc consumes it from the
            // operand stack, no slot. Keep only when some read escapes every suspend-call subtree
            // (it could then run AFTER the resume).
            if expr_calls_suspend(ir, st, &suspend_set)
                && !reads_only_in_suspension_args(ir, st, idx, &suspend_set)
            {
                return true;
            }
        }
        false
    });
    // Locals the crossing filter dropped only because a later statement OVERWRITES them on every
    // path. They carry nothing across a suspension — no spill field — but the flattener still splits
    // their binding and their reads across states, so each keeps a method-scope slot.
    let kill_dropped: Vec<u32> = before_crossing
        .into_iter()
        .filter(|idx| !reads.contains(idx))
        .filter(|&idx| stmts.iter().any(|&s| kills_value(ir, s, idx, &suspend_set)))
        .collect();
    // LOOP-CARRIED values: a local written+read inside a SUSPENDING loop statement is re-read on the
    // back-edge AFTER a resume (the induction `i` of `for (i in 5..6) { susp(i.toString()) }`), so
    // "consumed before the suspension" never holds — spill every local of such a loop.
    for &s in &stmts {
        if expr_calls_suspend(ir, s, &suspend_set) && stmt_contains_loop(ir, s) {
            collect_live_writes(ir, s, &suspend_set, &mut reads);
        }
    }
    reads.sort_unstable();
    reads.dedup();
    // kotlinc spills every named REFERENCE param/local IN SCOPE at a suspension point — regardless of
    // liveness (an unused param still gets an `L$N` slot) — excluding the binding of the LAST suspension
    // itself (not yet in scope there). Union that scope set with the liveness set above (which keeps
    // primitives and the locals internal to a suspending-loop last statement) so the `L$N` field set
    // matches kotlinc's.
    let mut spill_idx: Vec<u32> = reads;
    for (i, p) in real_params.iter().enumerate() {
        if p.is_reference() {
            spill_idx.push(this_offset + i as u32);
        }
    }
    // The scope rule covers NAMED source variables only: a compiler temp (elvis/safe-call
    // materialization) is a value kotlinc holds on the operand stack — empty across a suspension
    // unless still needed — so temps follow pure liveness (the `reads` set above).
    let mut scope_writes: Vec<u32> = Vec::new();
    for &s in &stmts[..last_susp] {
        collect_live_writes(ir, s, &suspend_set, &mut scope_writes);
    }
    for w in scope_writes {
        if named_vars.contains(&w) && find_local_ty(ir, b, w).is_some_and(|t| t.is_reference()) {
            spill_idx.push(w);
        }
    }
    // The LAST suspension's own DIRECT binding (`val rules = <suspend call>` with nothing suspending
    // after) is NOT in scope at any suspension point — kotlinc gives it no slot (its value arrives as
    // the resume `result`, bound as a fresh local). Only the direct-call shape: a CONDITIONAL init
    // (`val a = if (…) susp() else 7`) assigns the local from several branch states, which need the
    // spilled slot's loop-top declaration. Drop it unless an EARLIER statement also writes it.
    if let IrExpr::Variable {
        index,
        init: Some(init),
        ..
    } = &ir.exprs[stmts[last_susp] as usize]
    {
        if is_suspension_point(ir, *init, &suspend_set) {
            let idx = *index;
            let mut earlier: Vec<u32> = Vec::new();
            for &s in &stmts[..last_susp] {
                collect_live_writes(ir, s, &suspend_set, &mut earlier);
            }
            if !earlier.contains(&idx) {
                spill_idx.retain(|&i| i != idx);
            }
        }
    }
    spill_idx.sort_unstable();
    spill_idx.dedup();
    // A COMPILER TEMP bound to a `When` with a suspending branch VALUE (`val t = b?.f() ?: -1`
    // where `f` is a suspend value: `when { b != null -> f.invoke(b), else -> null }`): the
    // flattener state-splits the binding (`emit_cond` binds `t` in each branch's own resume
    // state, the reads live in the merge state), so the value MUST cross states through a spill
    // field — the liveness rule above ("a suspension inside the temp's own initializer runs
    // before the store") only holds for the straight-line binding shape.
    collect_cond_susp_temp_bindings(ir, b, &suspend_set, &mut spill_idx);
    spill_idx.sort_unstable();
    spill_idx.dedup();
    let mut spilled: Vec<(u32, Ty)> = Vec::new();
    for idx in spill_idx {
        if let Some(ty) = param_ty(idx).or_else(|| find_local_ty(ir, b, idx)) {
            spilled.push((idx, spill_field_ty(ty)));
        }
    }
    // A dropped temp still needs its slot declared once per invocation, exactly like a spilled one.
    let machine_locals: Vec<(u32, Ty)> = kill_dropped
        .iter()
        .filter(|idx| !spilled.iter().any(|(local, _)| local == *idx))
        .filter_map(|&idx| find_local_ty(ir, b, idx).map(|ty| (idx, spill_field_ty(ty))))
        .collect();
    // The spilled value parameters — captured at continuation construction (in spilled order).
    let param_caps: Vec<(u32, Ty)> = spilled
        .iter()
        .filter(|(idx, _)| param_ty(*idx).is_some())
        .cloned()
        .collect();
    let fname = ir.functions[fid as usize].name.clone();
    // kotlinc nests a suspend method's continuation class under its ENCLOSING class
    // (`Svc$work$1`), and a top-level suspend fun's under the file facade (`FooKt$foo$1`). The
    // dispatch receiver is the enclosing class internal name; a top-level/extension fun has none.
    let cont_owner = semantic_owner
        .map(TypeName::render)
        .unwrap_or_else(|| facade.to_string());
    // The continuation class uses the SOURCE method name, never the value-class-mangled JVM name:
    // kotlinc names `create-SCm-oBs`'s continuation `<Owner>$create$1`. `-` can't occur in a Kotlin
    // identifier, so it only ever separates the mangle hash — strip from the first `-`.
    let cont_fname = fname.split('-').next().unwrap_or(&fname);
    let cont_internal =
        continuation_class_name(&cont_owner, cont_fname, continuation_ordinal(ir, fid));
    let cont_ty = Ty::obj(&cont_internal);

    let base = max_value_index(ir) + 1;
    let cont_v = base;
    let r_v = base + 1;
    let suspended_v = base + 2;
    // The dispatch's own transient exception var is `base + 3`; the flattener's fresh locals start at
    // `base + 4`. A `try/catch` whose CATCH body suspends needs the caught exception to outlive that
    // suspension: allocate a fresh, collision-free value-index per such catch (above `base + 3`), rewrite
    // the catch body's reads of the user variable to it, and add it to the spill set BEFORE the
    // continuation class is built so it gets an `L$i` field. The handler binds it from `r_v` on entry.
    let mut catch_spills: std::collections::HashMap<ExprId, u32> = std::collections::HashMap::new();
    let mut catch_spill_sources: Vec<(u32, ExprId, u32)> = Vec::new();
    let mut next_ev = base + 4;
    {
        let mut tries: Vec<(u32, ExprId, crate::types::TypeName)> = Vec::new();
        find_suspending_catch_tries(ir, b, &suspend_set, &mut tries);
        for (cvar, cbody, exc_internal) in tries {
            let ev = next_ev;
            next_ev += 1;
            let mut reads: Vec<ExprId> = Vec::new();
            collect_getvalue(ir, cbody, cvar, &mut reads);
            for n in reads {
                ir.exprs[n as usize] = IrExpr::GetValue(ev);
            }
            spilled.push((ev, spill_field_ty(Ty::obj(&exc_internal.render()))));
            catch_spills.insert(cbody, ev);
            catch_spill_sources.push((cvar, cbody, ev));
        }
    }
    // Derive the flattener's first fresh local from the actual number of exception spills allocated
    // (`next_ev`), NOT `catch_spills.len()` — so even if two catches ever shared a value-index (making
    // the map shorter than the allocations) no `fresh()` local could alias an `ev`.
    let flat_next_local = next_ev;

    // The PRE-SPLICE per-suspension scope lists (captured in `lower_suspend` before
    // `splice_return_blocks` could leak block-scoped locals); catch variables remap to their
    // exception-spill locals allocated above.
    let mut susp_scopes = captured_scopes.unwrap_or_else(|| {
        let mut w = ScopeWalk {
            ir,
            suspend_set: &suspend_set,
            params: &param_caps,
            scope: Vec::new(),
            pending: Vec::new(),
            levels: Vec::new(),
            temps_only: false,
            out: Default::default(),
        };
        w.walk_stmts(&stmts);
        w.out
    });
    // The hoisted temps of a multi-suspension expression exist only in the FINAL body, so they are
    // collected here and merged into the pre-splice named-by-scope lists.
    merge_live_temps(&mut susp_scopes, live_temp_scopes(ir, b, &suspend_set));
    for &(cvar, cbody, ev) in &catch_spill_sources {
        let mut catch_points = HashSet::new();
        collect_suspension_points(ir, cbody, &suspend_set, &mut catch_points);
        for (point, scope) in &mut susp_scopes {
            if !catch_points.contains(point) {
                continue;
            }
            for e in &mut scope.values {
                if e.0 == cvar {
                    e.0 = ev;
                }
            }
            if let Some(name) = scope.names.remove(&cvar) {
                scope.names.insert(ev, name);
            }
        }
    }
    // Fold the field layout only over suspensions still PRESENT in the final (post-transform)
    // body — a desugared-away call's captured list must not size the class (kotlinc's field count
    // reflects the suspensions its machine actually has).
    let mut live_calls: HashSet<ExprId> = HashSet::new();
    collect_suspension_points(ir, b, &suspend_set, &mut live_calls);
    // A positional scope list is the authoritative set the flattener stores and restores. Reconcile
    // local allocation with it after both pre-splice named scopes and post-hoist live temps are merged:
    // otherwise a named local that becomes dead after operand snapshotting can still be restored into
    // an undeclared JVM slot. The same helper is used by lambda machines below, so allocation cannot
    // drift by machine or declaration-provider shape.
    reconcile_positional_spill_locals(ir, b, &susp_scopes, &live_calls, &mut spilled);
    // NOTE: `spill_shape_unmodeled` deliberately applies only to the LAMBDA machine — the named
    // machine's restore handles sub-int spills (`Boolean` params/temps, e2e-verified). Validate after
    // positional reconciliation because scope-only locals are real spills too.
    if spills_bottom_typed_local(&spilled) {
        return false;
    }
    crate::trace_compiler!(
        "suspend",
        "build_state_machine fid={fid} this_offset={this_offset} completion_idx={completion_idx} real_params={real_params:?} shared={:?} spilled={spilled:?} param_caps={param_caps:?}",
        ir.shared_capture_parameters
            .iter()
            .filter(|((function, _), _)| *function == fid)
            .collect::<Vec<_>>()
    );
    let mut layout = SpillLayout::default();
    for call in suspension_points_in_order(ir, b, &suspend_set) {
        if let Some(scope) = susp_scopes.get(&call) {
            layout.add_list(&scope.values);
        }
    }

    let cont_id = build_continuation_class(
        ir,
        &cont_internal,
        fid,
        &layout,
        &param_caps,
        receiver,
        &real_params,
    );

    // A `suspendCoroutineUninterceptedOrReturn { c -> … }` block inside this body reads its
    // continuation parameter: bind it to the machine WRAPPER (`cont_v`) so `c.resume(v)`
    // re-enters THIS machine at the resume label (kotlinc's protocol; the raw incoming
    // `$completion` would resume the CALLER instead).
    rewrite_current_continuation(ir, b, cont_v);

    // Flatten the body into a state graph.
    let mut flat = Flat {
        ir,
        default_call_operands,
        suspend: &suspend_set,
        cont_v,
        r_v,
        suspended_v,
        cont_id,
        field_base: 0, // dedicated continuation class: result/label/spilled at field 0..
        spilled: spilled.clone(),
        scopes: susp_scopes,
        layout: layout.clone(),
        states: vec![Vec::new()],
        resume_points: Vec::new(),
        state_handlers: vec![None],
        state_scope: vec![None],
        cur_handler: None,
        catch_var: base + 3,
        catch_spills,
        // Value parameters are assigned on entry (captured at construction, restored at the loop top).
        assigned: param_caps.iter().map(|(l, _)| *l).collect(),
        next_local: flat_next_local,
        loop_targets: Vec::new(),
        jump_finalizers: Vec::new(),
        failed: false,
    };
    flat.flatten(&stmts, 0, None);
    if flat.failed {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: flattener failed"
        );
        return false;
    }
    let resume_points = std::mem::take(&mut flat.resume_points);
    {
        let ir = &*flat.ir;
        let mut slot_name: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        if let Some(identities) = ir.function_parameter_identities(fid) {
            let function = &ir.functions[fid as usize];
            let this_off = u32::from(function.dispatch_receiver.is_some() && !function.is_static);
            for (i, identity) in identities.iter().enumerate() {
                if let Some(name) =
                    crate::jvm::parameter_names::debug_metadata(identity, &function.name)
                {
                    slot_name.insert(this_off + i as u32, name);
                }
            }
        }
        collect_slot_names(ir, b, &mut slot_name);
        let mut s = Vec::new();
        let mut n = Vec::new();
        let mut state_indices = Vec::new();
        for (state_idx, call) in resume_points.iter().enumerate() {
            let scope = &flat.scopes[call];
            let mut positions = kind_positions(&scope.values);
            // `@DebugMetadata`'s `n`/`s` lists hoist the REFERENCE spills ahead of the rest and
            // otherwise keep the order the locals were spilled in. They are not grouped by kind:
            // kotlinc lists `J$0` between `I$0` and `I$1` when the `long` was declared between the
            // two `int`s. Nor do they follow the class's field layout, which groups by kind — the
            // two orders are independent and only look alike when they agree.
            positions.sort_by_key(|&(_, _, kind, _)| u8::from(kind != REFERENCE_SPILL_KIND));
            for (slot, _ty, kind, pos) in positions {
                let name = scope
                    .names
                    .get(&slot)
                    .or_else(|| slot_name.get(&slot))
                    .cloned()
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                s.push(format!("{kind}${pos}"));
                n.push(name);
                state_indices.push(state_idx as i32);
            }
        }
        let line_of = |call: ExprId| -> (i32, i32) {
            if let Some(&(line, resume_line)) = suspension_lines.get(&call) {
                return (line as i32, resume_line as i32);
            }
            let line = ir
                .expr_lines
                .get(&call)
                .or_else(|| ir.fn_decl_lines.get(&fid))
                .copied()
                .unwrap_or_default() as i32;
            (line, line)
        };
        let l: Vec<i32> = resume_points.iter().map(|&call| line_of(call).0).collect();
        let nl: Vec<i32> = resume_points.iter().map(|&call| line_of(call).1).collect();
        let metadata = ContinuationMetadata {
            l,
            nl,
            i: state_indices,
            s,
            n,
            m: fname.clone(),
            c: cont_owner.replace('/', "."),
            v: 2,
            enclosing_class: cont_owner.clone(),
            enclosing_method: fname.clone(),
            enclosing_descriptor: crate::jvm::names::method_descriptor(
                &ir.functions[fid as usize].params,
                ir.functions[fid as usize].ret,
            ),
        };
        continuation_metadata.insert(cont_internal.clone(), metadata);
    }
    let states = std::mem::take(&mut flat.states);
    let state_handlers = std::mem::take(&mut flat.state_handlers);
    let catch_var = flat.catch_var;

    // --- assemble: prologue + while(true){ r=cont.result; restore spilled; when(label){states} } ---
    let k = |ir: &mut IrFile, e: IrExpr| ir.add_expr(e);
    let cint = |ir: &mut IrFile, n: i32| ir.add_expr(IrExpr::Const(IrConst::Int(n)));
    let getf = |ir: &mut IrFile, recv: ExprId, idx: u32| {
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: cont_id,
            index: idx,
        })
    };
    let state_scopes = std::mem::take(&mut flat.state_scope);
    // Value parameters are the stable PREFIX of every scope list — their per-kind positions (and
    // so their fields) are identical across states; the constructor captures them there.
    let param_positions: Vec<(u32, Ty, char, u32)> = kind_positions(&param_caps);

    // For an instance method, `new C$fn$1(this, completion)` also captures the receiver (value-index 0);
    // live value parameters are stored into their `L$N` fields right after a FRESH construction.
    let receiver_this = receiver.as_ref().map(|_| 0u32);
    let cap_pairs: Vec<(u32, u32)> = param_positions
        .iter()
        .map(|&(i, _, kind, pos)| (i, 2 + layout.slot(kind, pos)))
        .collect();
    let get_or_create = build_get_or_create(
        ir,
        completion_idx,
        &cont_ty,
        cont_id,
        receiver_this,
        &cap_pairs,
    );
    let var_cont = k(
        ir,
        IrExpr::Variable {
            index: cont_v,
            ty: cont_ty.clone(),
            init: Some(get_or_create),
            named: false,
        },
    );
    let suspended_call = coroutine_suspended(ir);
    let var_suspended = k(
        ir,
        IrExpr::Variable {
            index: suspended_v,
            ty: object_ty(),
            init: Some(suspended_call),
            named: false,
        },
    );

    // Prologue slot declarations: every spilled LOCAL gets a zero-initialized slot ONCE per
    // invocation (before the dispatch loop), so a resume arm's restores and same-invocation
    // cross-state reads all target one method-scope slot. Value parameters already own theirs.
    let is_param = |local: u32| param_caps.iter().any(|(p, _)| *p == local);
    let mut prologue_decls: Vec<ExprId> = Vec::new();
    for (local, ty) in spilled.iter().chain(machine_locals.iter()).copied() {
        if is_param(local) {
            continue;
        }
        let z = zero_value(ir, &ty);
        prologue_decls.push(k(
            ir,
            IrExpr::Variable {
                index: local,
                ty,
                init: Some(z),
                named: false,
            },
        ));
    }

    let mut loop_stmts: Vec<ExprId> = Vec::new();
    let cont_for_r = k(ir, IrExpr::GetValue(cont_v));
    let r_init = getf(ir, cont_for_r, 0);
    loop_stmts.push(k(
        ir,
        IrExpr::Variable {
            index: r_v,
            ty: object_ty(),
            init: Some(r_init),
            named: false,
        },
    ));

    let mut branches: Branches = Vec::new();
    for (i, st) in states.iter().enumerate() {
        let failure = throw_on_failure(ir, r_v);
        let mut ss = Vec::new();
        // A RESUME arm restores exactly ITS suspension's scope list (kotlinc: per-arm restores; a
        // non-resume state restores nothing — locals persist within one invocation). Restore before
        // delivering a failed resume: a catch state executes in this invocation and must observe the
        // captured locals rather than the null/zero placeholders passed by `invokeSuspend`.
        if let Some(Some(list)) = state_scopes.get(i) {
            for (local, ty, kind, pos) in kind_positions(list) {
                let cont_for_f = k(ir, IrExpr::GetValue(cont_v));
                let fld = 2 + layout.slot(kind, pos);
                let mut init = getf(ir, cont_for_f, fld);
                // A reference spill lives in an `Object`-typed field — `checkcast` back on restore.
                if ty.is_reference() && ty != object_ty() {
                    init = k(
                        ir,
                        IrExpr::TypeOp {
                            op: IrTypeOp::Cast,
                            arg: init,
                            type_operand: ty,
                        },
                    );
                }
                let restore = IrExpr::SetValue {
                    var: local,
                    value: init,
                };
                ss.push(k(ir, restore));
            }
            for local in rematerialized_nulls(list) {
                let n = k(ir, IrExpr::Const(IrConst::Null));
                ss.push(k(
                    ir,
                    IrExpr::SetValue {
                        var: local,
                        value: n,
                    },
                ));
            }
        }
        ss.push(failure);
        ss.extend(st.iter().copied());
        let recv = k(ir, IrExpr::GetValue(cont_v));
        let lbl = getf(ir, recv, 1);
        let sc = cint(ir, i as i32);
        let cond = k(
            ir,
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: lbl,
                rhs: sc,
            },
        );
        let block = k(
            ir,
            IrExpr::Block {
                stmts: ss,
                value: None,
            },
        );
        branches.push((Some(cond), block));
    }
    // default: `throw IllegalStateException(...)` (an unreachable resume label) — matches kotlinc.
    let msg = k(
        ir,
        IrExpr::Const(IrConst::String(KtString::from(
            "call to 'resume' before 'invoke' with coroutine",
        ))),
    );
    let exc = ir.new_external(
        "java/lang/IllegalStateException",
        "(Ljava/lang/String;)V",
        vec![msg],
    );
    let throw = k(ir, IrExpr::Throw { operand: exc });
    let else_block = k(
        ir,
        IrExpr::Block {
            stmts: vec![throw],
            value: None,
        },
    );
    branches.push((None, else_block));

    let dispatch = k(ir, IrExpr::When { branches });
    let dispatch =
        wrap_dispatch_for_handlers(ir, dispatch, &state_handlers, catch_var, cont_v, cont_id, 0);
    loop_stmts.push(dispatch);
    let loop_body = k(
        ir,
        IrExpr::Block {
            stmts: loop_stmts,
            value: None,
        },
    );
    let cond_true = k(ir, IrExpr::Const(IrConst::Boolean(true)));
    let while_loop = k(
        ir,
        IrExpr::While {
            cond: cond_true,
            body: loop_body,
            update: None,
            post_test: false,
            label: None,
        },
    );
    let mut body_stmts = vec![var_cont, var_suspended];
    body_stmts.extend(prologue_decls);
    body_stmts.push(while_loop);
    let new_body = k(
        ir,
        IrExpr::Block {
            stmts: body_stmts,
            value: None,
        },
    );
    ir.functions[fid as usize].body = Some(new_body);
    box_returns(ir, new_body)
}

/// Build the coroutine state machine for a suspend LAMBDA's `invokeSuspend` (`fid`) whose continuation
/// is the lambda instance (`class_id`) itself. The lambda class already holds its captures/parameters
/// at fields `0..field_base`; this appends `result`/`label`/spilled fields after them and rewrites the
/// body to `this.result = result; while(true){ r = this.result; <restore spilled>; when(this.label){
/// states } }`, threading `this` into each suspend call. Returns `false` (skip) for an unmodeled shape.
fn build_lambda_state_machine(
    ir: &mut IrFile,
    fid: u32,
    class_id: ClassId,
    field_base: u32,
    orig_rets: &[Ty],
    captured_scopes: Option<SuspensionScopes>,
    default_call_operands: &mut crate::jvm::default_call_operands::DefaultCallOperands,
) -> bool {
    let Some(b) = ir.functions[fid as usize].body else {
        return false;
    };
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    // Flatten a block-valued statement (`{ val g = …; res = g() }` whose tail assignment is wrapped as a
    // `Unit`-valued `Variable { init: Block { … } }`) into the top-level statement list FIRST, so the
    // hoist below sees the in-block declarations and the suspension in their real order — then lift a
    // suspension nested in an expression (`res = foo().a`) into a preceding `val tmp = foo()`, so the
    // flattener meets it as a bound-local suspension typed by the callee's logical return.
    normalize_block_inits(ir, b);
    split_unit_conditional_returns(ir, b, orig_rets.get(fid as usize) == Some(&Ty::Unit));
    let mut value_types = function_value_types(ir, fid, b);
    hoist_suspensions(ir, b, &suspend_set, orig_rets, &mut value_types);
    // A `suspendCoroutineUninterceptedOrReturn { c -> … }` block reads its continuation parameter:
    // bind it to the lambda machine itself (`this` = value-index 0) so `c.resume(v)` re-enters
    // THIS machine at the resume label.
    rewrite_current_continuation(ir, b, 0);
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        crate::trace_compiler!(
            "suspend",
            "build_lambda_sm fid={fid} BAIL: body not a Block"
        );
        return false;
    };
    if value.is_some() {
        crate::trace_compiler!(
            "suspend",
            "build_lambda_sm fid={fid} BAIL: block has a trailing value"
        );
        return false;
    }
    crate::trace_compiler!(
        "suspend",
        "build_lambda_sm fid={fid} ({} stmts)",
        stmts.len()
    );
    let Some(first) = stmts
        .iter()
        .position(|&s| expr_calls_suspend(ir, s, &suspend_set))
    else {
        crate::trace_compiler!(
            "suspend",
            "build_lambda_sm fid={fid} BAIL: no suspend call in any stmt"
        );
        return false;
    };
    let mut reads: Vec<u32> = Vec::new();
    for &s in &stmts[first..] {
        collect_reads(ir, s, &mut reads);
    }
    reads.sort_unstable();
    reads.dedup();
    // Drop tail-confined locals (every reference after the last top-level suspending statement — e.g. a
    // structural loop's iterator that runs entirely in the final resume state). See the twin comment in
    // `build_state_machine`: spilling them mis-frames the loop back-edge. A capture/param (`2..2+field_base`)
    // is retained (reloaded in the prologue); otherwise keep only a value WRITTEN up to & including the
    // last suspending statement.
    let last_susp = stmts
        .iter()
        .rposition(|&s| expr_calls_suspend(ir, s, &suspend_set))
        .unwrap_or(first);
    let mut head_writes: Vec<u32> = Vec::new();
    for &s in &stmts[..=last_susp] {
        collect_live_writes(ir, s, &suspend_set, &mut head_writes);
    }
    head_writes.sort_unstable();
    head_writes.dedup();
    reads.retain(|idx| (2..2 + field_base).contains(idx) || head_writes.binary_search(idx).is_ok());
    // LOOP-CARRIED values (see the named machine): every local of a suspending loop statement is
    // re-read on the back-edge after a resume — spill them regardless of read position.
    for &s in &stmts {
        if expr_calls_suspend(ir, s, &suspend_set) && stmt_contains_loop(ir, s) {
            collect_live_writes(ir, s, &suspend_set, &mut reads);
        }
    }
    reads.sort_unstable();
    reads.dedup();
    let mut spilled: Vec<(u32, Ty)> = Vec::new();
    for idx in reads {
        // Capture/parameter locals (value-indices `2..2+field_base`) are reloaded from their fields in
        // the prologue at every entry, so they survive re-entry without being spilled — exclude them.
        if (2..2 + field_base).contains(&idx) {
            continue;
        }
        if let Some(ty) = find_local_ty(ir, b, idx) {
            spilled.push((idx, spill_field_ty(ty)));
        }
    }
    // A RECEIVER lambda's restore mis-slots a NARROW int-like or array spill (the corpus
    // `intLikeVarSpilling` and `suspendFunctionAsCoroutine/superCall*` shapes), so those still bail.
    // `Boolean` is NOT among them: `runBlocking { … }` binds `CoroutineScope` as its receiver, so a
    // `Boolean` live across a suspension inside one reaches this path routinely and the restore
    // handles it (`suspend_try_finally_body_e2e`, `suspend_receiver_lambda_unit_try_tail`).
    let receiver_lambda = ir.classes[class_id as usize]
        .fields
        .first()
        .is_some_and(|f| f.name == "this");
    // The PRE-SPLICE per-suspension scope lists (captured in `lower_suspend`); a lambda has no
    // value-param prefix (captures/params live in leading fields, reloaded each entry). A lambda
    // fid the prelude never visited (not in `suspend_funs`) falls back to a post-transform walk.
    let mut susp_scopes = captured_scopes.unwrap_or_else(|| {
        let mut w = ScopeWalk {
            ir,
            suspend_set: &suspend_set,
            params: &[],
            scope: Vec::new(),
            pending: Vec::new(),
            levels: Vec::new(),
            temps_only: false,
            out: Default::default(),
        };
        w.walk_stmts(&stmts);
        w.out
    });
    // See the twin in `build_state_machine`: the hoisted temps of a multi-suspension expression exist
    // only in the FINAL body, so they are collected here and merged into the captured lists.
    merge_live_temps(&mut susp_scopes, live_temp_scopes(ir, b, &suspend_set));
    let mut live_calls: HashSet<ExprId> = HashSet::new();
    collect_suspension_points(ir, b, &suspend_set, &mut live_calls);
    reconcile_positional_spill_locals(ir, b, &susp_scopes, &live_calls, &mut spilled);
    if (receiver_lambda && spill_shape_unmodeled(&spilled))
        || spills_bottom_typed_local(&spilled)
        || tail_suspending_loop(ir, &stmts, &suspend_set)
    {
        return false;
    }
    let mut layout = SpillLayout::default();
    for call in suspension_points_in_order(ir, b, &suspend_set) {
        if let Some(scope) = susp_scopes.get(&call) {
            layout.add_list(&scope.values);
        }
    }

    // Append `result`, `label`, then the positional spill slots — after the captures/parameters.
    {
        let cls = &mut ir.classes[class_id as usize];
        let mut push = |name: &str, ty: Ty| {
            // State-machine fields are mutable and non-private (read/written cross-class).
            cls.fields
                .push(crate::ir::IrField::new(name.to_string(), ty).with_is_private(false));
        };
        push("result", object_ty());
        push("label", int_ty());
        for (name, ty) in layout.fields() {
            push(&name, ty);
        }
    }

    let base = max_value_index(ir) + 1;
    let r_v = base;
    let suspended_v = base + 1;

    let mut flat = Flat {
        ir,
        default_call_operands,
        suspend: &suspend_set,
        cont_v: 0, // `this`
        r_v,
        suspended_v,
        cont_id: class_id,
        field_base,
        spilled: spilled.clone(),
        scopes: susp_scopes,
        layout: layout.clone(),
        states: vec![Vec::new()],
        resume_points: Vec::new(),
        state_handlers: vec![None],
        state_scope: vec![None],
        cur_handler: None,
        catch_var: base + 2,
        // A suspend LAMBDA's `invokeSuspend` doesn't yet model a suspending catch (the shape bails in
        // `flatten` as before), so no exception spills are pre-allocated here.
        catch_spills: std::collections::HashMap::new(),
        // Captures/parameters live in leading fields (excluded from `spilled`), so no spilled local is
        // assigned on entry.
        assigned: std::collections::HashSet::new(),
        next_local: base + 3,
        loop_targets: Vec::new(),
        jump_finalizers: Vec::new(),
        failed: false,
    };
    for (n, &s) in stmts.iter().enumerate() {
        crate::trace_compiler!(
            "suspend",
            "lambda stmt[{n}] = {:?}",
            flat.ir.exprs[s as usize]
        );
        if expr_calls_suspend(flat.ir, s, &suspend_set) {
            trace_residual_suspension(flat.ir, s, 0);
        }
    }
    flat.flatten(&stmts, 0, None);
    if flat.failed {
        crate::trace_compiler!(
            "suspend",
            "build_lambda_sm fid={fid} BAIL: flattener failed"
        );
        return false;
    }
    crate::trace_compiler!(
        "suspend",
        "build_lambda_sm fid={fid} spilled={:?}",
        flat.spilled
    );
    let states = std::mem::take(&mut flat.states);
    let state_handlers = std::mem::take(&mut flat.state_handlers);
    let state_scopes = std::mem::take(&mut flat.state_scope);
    let catch_var = flat.catch_var;

    let k = |ir: &mut IrFile, e: IrExpr| ir.add_expr(e);
    let cint = |ir: &mut IrFile, n: i32| ir.add_expr(IrExpr::Const(IrConst::Int(n)));
    let getf = |ir: &mut IrFile, recv: ExprId, idx: u32| {
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: class_id,
            index: field_base + idx,
        })
    };

    // Prologue: `this.result = result` (the invokeSuspend parameter is value-index 1).
    let this_p = k(ir, IrExpr::GetValue(0));
    let result_param = k(ir, IrExpr::GetValue(1));
    let store_result = k(
        ir,
        IrExpr::SetField {
            receiver: this_p,
            class: class_id,
            index: field_base,
            value: result_param,
        },
    );
    let suspended_call = coroutine_suspended(ir);
    let var_suspended = k(
        ir,
        IrExpr::Variable {
            index: suspended_v,
            ty: object_ty(),
            init: Some(suspended_call),
            named: false,
        },
    );

    let mut loop_stmts: Vec<ExprId> = Vec::new();
    let this_r = k(ir, IrExpr::GetValue(0));
    let r_init = getf(ir, this_r, 0);
    loop_stmts.push(k(
        ir,
        IrExpr::Variable {
            index: r_v,
            ty: object_ty(),
            init: Some(r_init),
            named: false,
        },
    ));
    // Prologue slot declarations (see the named-machine twin): every spilled local gets one
    // zero-initialized method-scope slot per invocation.
    let mut prologue_decls: Vec<ExprId> = Vec::new();
    for (local, ty) in spilled.iter().copied() {
        let z = zero_value(ir, &ty);
        prologue_decls.push(k(
            ir,
            IrExpr::Variable {
                index: local,
                ty,
                init: Some(z),
                named: false,
            },
        ));
    }
    let mut branches: Branches = Vec::new();
    for (i, st) in states.iter().enumerate() {
        let failure = throw_on_failure(ir, r_v);
        let mut ss = Vec::new();
        // A RESUME arm restores exactly ITS suspension's scope list (kotlinc: per-arm restores).
        if let Some(Some(list)) = state_scopes.get(i) {
            for (local, ty, kind, pos) in kind_positions(list) {
                let this_f = k(ir, IrExpr::GetValue(0));
                let fld = 2 + layout.slot(kind, pos);
                let mut init = getf(ir, this_f, fld);
                if ty.is_reference() && ty != object_ty() {
                    init = k(
                        ir,
                        IrExpr::TypeOp {
                            op: IrTypeOp::Cast,
                            arg: init,
                            type_operand: ty,
                        },
                    );
                }
                ss.push(k(
                    ir,
                    IrExpr::SetValue {
                        var: local,
                        value: init,
                    },
                ));
            }
            for local in rematerialized_nulls(list) {
                let n = k(ir, IrExpr::Const(IrConst::Null));
                ss.push(k(
                    ir,
                    IrExpr::SetValue {
                        var: local,
                        value: n,
                    },
                ));
            }
        }
        ss.push(failure);
        ss.extend(st.iter().copied());
        let recv = k(ir, IrExpr::GetValue(0));
        let lbl = getf(ir, recv, 1);
        let sc = cint(ir, i as i32);
        let cond = k(
            ir,
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: lbl,
                rhs: sc,
            },
        );
        let block = k(
            ir,
            IrExpr::Block {
                stmts: ss,
                value: None,
            },
        );
        branches.push((Some(cond), block));
    }
    let msg = k(
        ir,
        IrExpr::Const(IrConst::String(KtString::from(
            "call to 'resume' before 'invoke' with coroutine",
        ))),
    );
    let exc = ir.new_external(
        "java/lang/IllegalStateException",
        "(Ljava/lang/String;)V",
        vec![msg],
    );
    let throw = k(ir, IrExpr::Throw { operand: exc });
    let else_block = k(
        ir,
        IrExpr::Block {
            stmts: vec![throw],
            value: None,
        },
    );
    branches.push((None, else_block));
    let dispatch = k(ir, IrExpr::When { branches });
    let dispatch = wrap_dispatch_for_handlers(
        ir,
        dispatch,
        &state_handlers,
        catch_var,
        0,
        class_id,
        field_base,
    );
    loop_stmts.push(dispatch);
    let loop_body = k(
        ir,
        IrExpr::Block {
            stmts: loop_stmts,
            value: None,
        },
    );
    let cond_true = k(ir, IrExpr::Const(IrConst::Boolean(true)));
    let while_loop = k(
        ir,
        IrExpr::While {
            cond: cond_true,
            body: loop_body,
            update: None,
            post_test: false,
            label: None,
        },
    );
    // Reload each captured variable / parameter from its field into its local (value-index `2+i`) —
    // runs at every entry (including a resume), so a value read across a suspension is always available.
    let mut prologue: Vec<ExprId> = Vec::new();
    for i in 0..field_base {
        let cap_ty = ir.classes[class_id as usize].fields[i as usize].ty.clone();
        let this_c = k(ir, IrExpr::GetValue(0));
        let getf_c = k(
            ir,
            IrExpr::GetField {
                receiver: this_c,
                class: class_id,
                index: i,
            },
        );
        prologue.push(k(
            ir,
            IrExpr::Variable {
                index: 2 + i,
                ty: cap_ty,
                init: Some(getf_c),
                named: false,
            },
        ));
    }
    prologue.extend([store_result, var_suspended]);
    prologue.extend(prologue_decls);
    prologue.push(while_loop);
    let new_body = k(
        ir,
        IrExpr::Block {
            stmts: prologue,
            value: None,
        },
    );
    ir.functions[fid as usize].body = Some(new_body);
    box_returns(ir, new_body)
}

/// Flattener: turns the structured suspend-function body into a flat list of states connected by
/// `label = next` transitions (see [`build_state_machine`]).
struct Flat<'a> {
    ir: &'a mut IrFile,
    default_call_operands: &'a mut crate::jvm::default_call_operands::DefaultCallOperands,
    suspend: &'a HashSet<u32>,
    cont_v: u32,
    r_v: u32,
    suspended_v: u32,
    cont_id: ClassId,
    /// Base field index of the state-machine fields (`result`, `label`, spilled `L$…`) on `cont_id`. A
    /// function's dedicated continuation class puts them at `0..` (`field_base = 0`); a suspend LAMBDA
    /// reuses its own class, whose captures/parameters occupy the leading fields, so they start after.
    field_base: u32,
    spilled: Vec<(u32, Ty)>,
    states: Vec<Vec<ExprId>>,
    resume_points: Vec<ExprId>,
    /// Parallel to `states`: the handler state (a `catch` body's entry) whose `try` region covers this
    /// state, if any. A suspension inside a `try { … } catch { … }` marks the try-body states with their
    /// handler; the assembly then routes an exception thrown while `this.label` is such a state to the
    /// handler (via a `label`-based check in the dispatch's `catch`), leaving one thrown elsewhere to
    /// re-propagate. No per-state flag local/field is needed — `this.label` already identifies the state.
    state_handlers: Vec<Option<usize>>,
    /// The handler state currently in effect while flattening a `try` body (set/restored around it).
    cur_handler: Option<usize>,
    /// Value-index for the `catch`'s exception variable (a transient local; only used to stash the
    /// exception into the `result` field for the handler state to read back through `r_v`).
    catch_var: u32,
    /// For a `try { … } catch (e) { …; suspend(); … }` whose CATCH body ITSELF suspends: maps the
    /// catch body's stable IR region to a fresh, spilled value-index holding the caught exception.
    /// The catch body's reads of `e` are pre-rewritten to this index; the handler state binds it from
    /// `r_v` (`e = (E) r_v`) once on entry, and it is spilled/restored like any local so it survives the
    /// catch's own suspension (after which `r_v` holds the resume value, no longer the exception).
    catch_spills: std::collections::HashMap<ExprId, u32>,
    /// Per-suspension lexical scope lists (kotlinc's positional-spill model, see
    /// docs/POSITIONAL_SPILLS.md): suspend-call expr id → `params ++ in-scope locals`.
    scopes: SuspensionScopes,
    /// The positional field layout (per-kind maxima over every scope list).
    layout: SpillLayout,
    /// Per RESUME state: the scope list its arm restores (`None` for a non-resume state).
    state_scope: Vec<Option<Vec<(u32, Ty)>>>,
    /// Spilled locals definitely assigned on the current flatten path. `spill_all` skips a spilled var
    /// not in this set: on an exceptional edge (a `catch` body reached without the `try` body's writes)
    /// a body-only local is dead, and spilling its (coalesced, possibly wrong-typed) slot would emit a
    /// verify-invalid store. A skipped field keeps a same-typed prior value or its default, and the
    /// loop-top restore is always type-correct — so gating on definite assignment is sound.
    assigned: std::collections::HashSet<u32>,
    next_local: u32,
    /// Loop-target stack for a suspending loop whose body is flattened across states: each entry is
    /// `(label, continue_state, break_state)`. A `Continue`/`Break` statement inside the body resolves to
    /// the innermost frame (or the labeled one) and emits a `goto` to that state — the structured
    /// `Continue`/`Break` node can't survive flattening (at emit it would target the dispatch `while(true)`
    /// loop, not the user's logical loop). Pushed around the body in the `While`-suspending-body handler.
    loop_targets: Vec<(Option<String>, usize, usize)>,
    /// Active non-suspending `finally` regions which an outward loop jump must execute. The boundary
    /// is the number of loop frames active on entry to the protected region; a jump to an older frame
    /// exits that region. The saved handler is the enclosing handler under which cleanup executes.
    jump_finalizers: Vec<(usize, ExprId, Option<usize>)>,
    failed: bool,
}

impl Flat<'_> {
    fn add(&mut self, e: IrExpr) -> ExprId {
        self.ir.add_expr(e)
    }
    fn gv(&mut self, i: u32) -> ExprId {
        self.add(IrExpr::GetValue(i))
    }
    fn fresh(&mut self) -> u32 {
        let v = self.next_local;
        self.next_local += 1;
        v
    }
    fn new_state(&mut self) -> usize {
        self.states.push(Vec::new());
        self.state_handlers.push(self.cur_handler);
        self.state_scope.push(None);
        self.states.len() - 1
    }
    fn is_spilled(&self, l: u32) -> bool {
        self.spilled.iter().any(|(x, _)| *x == l)
    }
    fn mark_assigned(&mut self, l: u32) {
        if self.is_spilled(l) {
            self.assigned.insert(l);
        }
    }
    fn setfield(&mut self, out: &mut Vec<ExprId>, idx: u32, val: ExprId) {
        let recv = self.gv(self.cont_v);
        let e = self.add(IrExpr::SetField {
            receiver: recv,
            class: self.cont_id,
            index: self.field_base + idx,
            value: val,
        });
        out.push(e);
    }
    fn set_label(&mut self, out: &mut Vec<ExprId>, target: usize) {
        let v = self.add(IrExpr::Const(IrConst::Int(target as i32)));
        self.setfield(out, 1, v);
    }
    /// The scope list for `call` — the snapshot from the pre-flatten walk. `None` (a shape the
    /// walk didn't reach) fails the machine: the layout wouldn't cover it (skip, never miscompile).
    fn scope_list_for(&self, call: ExprId) -> Option<Vec<(u32, Ty)>> {
        self.scopes.get(&call).map(|scope| scope.values.clone())
    }
    /// Store `list` POSITIONALLY into the spill fields (kotlinc: each suspension stores its
    /// in-scope vars at per-kind positions; different states reuse the same fields).
    fn spill_scope(&mut self, out: &mut Vec<ExprId>, list: &[(u32, Ty)]) {
        for (l, ty, kind, pos) in kind_positions(list) {
            let f = 2 + self.layout.slot(kind, pos);
            // A `Unit`-typed local has no on-stack value (`gv` would underflow) — its live value across
            // the suspension is always the `Unit` singleton, so store that directly.
            let v = if ty == Ty::obj("kotlin/Unit") {
                self.add(IrExpr::UnitInstance)
            } else {
                self.gv(l)
            };
            self.setfield(out, f, v);
        }
    }
    fn goto(&mut self, out: &mut Vec<ExprId>, target: usize) {
        // No spilling: every transfer dispatches through `when(label)` WITHIN one invocation, and
        // locals persist; only RESUME arms restore fields (stored by their suspension's spill).
        debug_assert!(
            self.state_scope.get(target).is_none_or(Option::is_none),
            "goto into a resume arm would restore stale fields"
        );
        self.set_label(out, target);
    }
    /// Materialize the suspension point's receiver/arguments into fresh temps emitted into `out` AHEAD
    /// of the spill, left to right so source evaluation order is preserved, and rewrite the call to read
    /// the temps. Without this the spill `putfield`s run first and a side effect an operand has on a
    /// spilled local (`foo(i++)`) lands in the local but never in the field, so the resume arm restores
    /// the PRE-evaluation value (`bars(foo(i++), foo(i++))` yielded `"1;1;"` instead of `"1;2;"`).
    /// kotlinc has its arguments on the operand stack before its `putfield`s, so its spill always
    /// observes the post-evaluation state; the temps reproduce that ordering in IR. They never cross the
    /// suspension — the call IS the suspension and consumes them before it — so they need no spill slots.
    ///
    /// Only fires when an operand actually writes a local this point spills: binding every suspension's
    /// operands would add store/load pairs kotlinc does not emit. Returns `false` when the shape can't be
    /// re-bound (an intrinsic/inline-splice callee, a lambda or vararg operand, a conditional suspension
    /// buried in an operand) — the caller then fails the machine (skip, never miscompile).
    fn bind_operand_temps(
        &mut self,
        out: &mut Vec<ExprId>,
        point: ExprId,
        list: &[(u32, Ty)],
    ) -> bool {
        let operands = suspension_operand_ids(self.ir, point);
        if operands.is_empty() {
            // An INTRINSIC suspension point (`suspendCoroutineUninterceptedOrReturn { c -> … }`) is an
            // inlined block, not a call: it has no operands to move ahead of the spill, and the block
            // body runs after the spill by construction. That is not a hazard, because a mutable local
            // the block writes is captured BY REFERENCE (the front end `RefNew`-boxes it the moment a
            // lambda writes it), so the write lands in the heap cell whose reference the spill stored —
            // the restore cannot undo it. Refuse should that ever stop holding, rather than lose a write.
            return !writes_local_in(self.ir, point, list);
        }
        if !operands.iter().any(|&o| writes_local_in(self.ir, o, list)) {
            return true;
        }
        let Some(operands) = typed_suspension_operands(self.ir, point) else {
            return false;
        };
        // A conditional suspension nested in an operand (`foo(if (c) susp() else 1)`) is left in place by
        // the hoister for the flattener; hoisting it into a pre-spill temp would put a suspension ahead of
        // this one's own spill. Refuse instead.
        if operands
            .iter()
            .any(|&(o, _)| expr_calls_suspend(self.ir, o, self.suspend))
        {
            return false;
        }
        let reads: Vec<ExprId> = operands
            .iter()
            .map(|&(o, ty)| {
                let idx = self.fresh();
                let v = self.add(IrExpr::Variable {
                    index: idx,
                    ty,
                    init: Some(o),
                    named: false,
                });
                out.push(v);
                self.gv(idx)
            })
            .collect();
        let rebound_arguments = match &mut self.ir.exprs[point as usize] {
            IrExpr::Call {
                dispatch_receiver,
                args,
                ..
            } => {
                let mut it = reads.iter().copied();
                if let Some(r) = dispatch_receiver.as_mut() {
                    *r = it.next().expect("receiver operand was typed first");
                }
                *args = it.collect();
                Some(args.clone())
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                let mut it = reads.iter().copied();
                *receiver = it.next().expect("receiver operand was typed first");
                *args = it.map(Some).collect();
                None
            }
            _ => return false,
        };
        if let Some(arguments) = rebound_arguments {
            if !self
                .default_call_operands
                .replace_operands(point, &arguments)
            {
                return false;
            }
        }
        true
    }
    /// Emit the suspend-call sequence into `out`, transferring to state `resume` (the loop re-dispatches
    /// `resume` on synchronous completion; on `COROUTINE_SUSPENDED` the function returns and a later
    /// resume re-enters at `resume`).
    fn emit_suspension(
        &mut self,
        out: &mut Vec<ExprId>,
        point: ExprId,
        resume: usize,
        completion: SuspensionCompletion,
    ) {
        crate::trace_compiler!(
            "suspend",
            "emit_suspension {point}:{:?}",
            &self.ir.exprs[point as usize]
        );
        let Some(list) = self.scope_list_for(point) else {
            crate::trace_compiler!(
                "suspend",
                "emit_suspension BAIL: no scope snapshot for suspension point {point}"
            );
            self.failed = true;
            return;
        };
        if !self.bind_operand_temps(out, point, &list) {
            crate::trace_compiler!(
                "suspend",
                "emit_suspension BAIL: unre-bindable operands at suspension point {point}"
            );
            self.failed = true;
            return;
        }
        // A suspend call whose source result is `Nothing` can still return
        // `COROUTINE_SUSPENDED`. The ordinary emitter-level bottom guard must therefore not run on
        // the physical call itself (it would throw before the marker comparison). The caller has
        // already selected the resume completion from the checked BottomValue wrapper in its exact
        // use context; clear the raw call's logical bottom fact independently of that selection.
        if completion.semantic_bottom {
            self.ir.logical_types.remove(&point);
        }
        self.spill_scope(out, &list);
        self.resume_points.push(point);
        if let Some(sc) = self.state_scope.get_mut(resume) {
            *sc = Some(list);
        }
        self.set_label(out, resume);
        let cont_arg = {
            let c = self.gv(self.cont_v);
            self.add(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: c,
                type_operand: continuation_ty(),
            })
        };
        // Callable points receive the CPS continuation argument. An intrinsic block already embeds
        // its continuation placeholder and is intentionally unchanged by `append_continuation`.
        if !append_continuation(self.ir, point, cont_arg, self.default_call_operands) {
            self.failed = true;
            return;
        }
        let vv = self.fresh();
        let var = self.add(IrExpr::Variable {
            index: vv,
            ty: object_ty(),
            init: Some(point),
            named: false,
        });
        out.push(var);
        let vr = self.gv(vv);
        let sr = self.gv(self.suspended_v);
        let is = self.add(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: vr,
            rhs: sr,
        });
        let sv = self.gv(self.suspended_v);
        let ret = self.add(IrExpr::Return(Some(sv)));
        // The branch body must be a `Block` (as in `emit_cond`/`emit_when_stmt`): the When-statement
        // emitter drops a bare non-`Block` branch body, so a raw `Return` here emits no bytecode —
        // letting `COROUTINE_SUSPENDED` fall through to the unbox (a `ClassCastException` on suspend).
        let ret_block = self.add(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let empty = self.add(IrExpr::Block {
            stmts: vec![],
            value: None,
        });
        let when = self.add(IrExpr::When {
            branches: vec![(Some(is), ret_block), (None, empty)],
        });
        out.push(when);
        let vg = self.gv(vv);
        self.setfield(out, 0, vg); // cont.result = v (so the resume reads the synchronous value)
    }

    fn terminate_bottom_resume(&mut self, out: &mut Vec<ExprId>) {
        let exception =
            self.ir
                .new_external("kotlin/KotlinNothingValueException", "()V", Vec::new());
        out.push(self.add(IrExpr::Throw { operand: exception }));
    }
    /// Bind a suspension result from `cont.result` (loaded into `r`) at a resume state's entry.
    /// A spilled local's slot is pre-declared in the machine PROLOGUE (zero-initialized), so assign
    /// it; a non-spilled local is declared here.
    fn bind_from_r(
        &mut self,
        out: &mut Vec<ExprId>,
        local: u32,
        ty: &Ty,
        _resume: usize,
        call: ExprId,
    ) {
        let rg = self.gv(self.r_v);
        let realization = value_class_suspension_result(self.ir, call, self.suspend);
        crate::trace_compiler!(
            "suspend",
            "bind_from_r local={local} ty={ty:?} realization={realization:?} spilled={}",
            self.is_spilled(local)
        );
        // A CPS result always arrives as a VALUE. In particular semantic `Unit` is the
        // `kotlin.Unit` singleton here, never a JVM `void` result. Record the stored physical type
        // on the resume local so later checked coercions consume the existing operand instead of
        // materializing a second singleton and leaving the first one on the stack.
        let physical_ty = realization.map_or_else(
            || crate::types::stored_value_ty(*ty),
            crate::ir::IrValueClassSuspendResult::boundary_ty,
        );
        let unb = match realization {
            Some(crate::ir::IrValueClassSuspendResult::Boxed { classifier, .. }) => {
                unbox(self.ir, rg, &Ty::obj_name(classifier))
            }
            Some(crate::ir::IrValueClassSuspendResult::Carrier(carrier)) => {
                unbox(self.ir, rg, &carrier)
            }
            None => unbox(self.ir, rg, ty),
        };
        self.mark_assigned(local);
        if self.is_spilled(local) {
            out.push(self.add(IrExpr::SetValue {
                var: local,
                value: unb,
            }));
        } else {
            out.push(self.add(IrExpr::Variable {
                index: local,
                ty: physical_ty,
                init: Some(unb),
                named: false,
            }));
        }
    }
    /// If `stmt` is a (possibly result-discarding) direct suspension, return `(bound local, call ExprId)`.
    /// Whether `e`'s subtree contains a `continue`/`break` for a loop currently being flattened — an
    /// UNLABELED jump (targets the innermost loop, i.e. the one whose body is flattening), or a LABELED
    /// jump matching an active `loop_targets` frame. Stops at a nested `While`/`Lambda`: an unlabeled jump
    /// there belongs to that inner loop / closure, not this one. Drives the `When`-statement state-split so
    /// a branch carrying such a jump gets its own state (where the jump becomes a tail `goto`).
    fn expr_has_loop_jump(&self, e: ExprId) -> bool {
        match &self.ir.exprs[e as usize] {
            IrExpr::Break { label } | IrExpr::Continue { label } => match label {
                None => true,
                Some(l) => self
                    .loop_targets
                    .iter()
                    .any(|(fl, _, _)| fl.as_deref() == Some(l.as_str())),
            },
            IrExpr::While { .. } | IrExpr::Lambda { .. } => false,
            _ => {
                let mut found = false;
                crate::ir::for_each_child(&self.ir.exprs, e, &mut |c| {
                    found = found || self.expr_has_loop_jump(c);
                });
                found
            }
        }
    }
    /// Whether `e` contains a LABELED `break`/`continue` targeting a loop frame currently being
    /// flattened (an active `loop_targets` entry) — a jump that must pierce OUT of `e` to a state.
    /// Unlike `expr_has_loop_jump`, this recurses THROUGH a nested `While` (a labeled jump can cross an
    /// inner structural loop to an outer flattened one — e.g. a `return@withLock`/labeled break buried in
    /// a `?.let { … }` whose inline expansion is a `while(true){ … }` wrapper). Unlabeled jumps bind to
    /// the innermost structural loop, not an outer frame, so they don't count; a `Lambda` is a closure
    /// boundary and stops the descent. Drives the state-split of an otherwise-structural `When`/`While`
    /// so the buried jump reaches its `goto` instead of dangling at a dissolved loop label.
    fn expr_jumps_to_active_frame(&self, e: ExprId) -> bool {
        match &self.ir.exprs[e as usize] {
            IrExpr::Break { label: Some(l) } | IrExpr::Continue { label: Some(l) } => self
                .loop_targets
                .iter()
                .any(|(fl, _, _)| fl.as_deref() == Some(l.as_str())),
            IrExpr::Break { label: None } | IrExpr::Continue { label: None } => false,
            IrExpr::Lambda { .. } => false,
            _ => {
                let mut found = false;
                crate::ir::for_each_child(&self.ir.exprs, e, &mut |c| {
                    found = found || self.expr_jumps_to_active_frame(c);
                });
                found
            }
        }
    }
    /// The state a `continue`/`break` transfers to: the `cont`/`exit` of the innermost active loop frame,
    /// or the frame whose label matches. `None` when no such loop is being flattened (a jump the caller
    /// leaves structural).
    fn loop_jump_target_with_frame(
        &self,
        label: Option<&str>,
        is_break: bool,
    ) -> Option<(usize, usize)> {
        let (frame_index, (_, cont, exit)) = match label {
            Some(label) => self
                .loop_targets
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (frame_label, _, _))| frame_label.as_deref() == Some(label))?,
            None => self.loop_targets.iter().enumerate().next_back()?,
        };
        Some((frame_index, if is_break { *exit } else { *cont }))
    }
    /// Transfer a loop completion through every `finally` region it exits. Cleanup runs in a fresh
    /// state outside the protected region's own handler; the original jump is appended after cleanup
    /// and recursively routes through any next enclosing `finally`.
    fn emit_loop_jump(
        &mut self,
        out: &mut Vec<ExprId>,
        jump: ExprId,
        label: Option<&str>,
        is_break: bool,
    ) -> bool {
        let Some((frame_index, target)) = self.loop_jump_target_with_frame(label, is_break) else {
            return false;
        };
        let Some(finalizer_index) = self
            .jump_finalizers
            .iter()
            .rposition(|(boundary, _, _)| frame_index < *boundary)
        else {
            self.goto(out, target);
            return true;
        };
        let finalizer = self.jump_finalizers.remove(finalizer_index);
        let saved_handler = self.cur_handler;
        self.cur_handler = finalizer.2;
        let cleanup = self.new_state();
        self.goto(out, cleanup);
        let mut cleanup_stmts = self.block_stmts(finalizer.1);
        cleanup_stmts.push(jump);
        self.flatten(&cleanup_stmts, cleanup, None);
        self.cur_handler = saved_handler;
        self.jump_finalizers.insert(finalizer_index, finalizer);
        true
    }
    fn stmt_suspension(&self, stmt: ExprId) -> Option<Suspension> {
        match &self.ir.exprs[stmt as usize] {
            IrExpr::Variable {
                index,
                ty,
                init: Some(init),
                ..
            } => {
                // `val x: T = susp()` where the generic call result is wrapped in a `Cast`/coercion to
                // `T` — the binding's `bind_from_r` already casts the resumed result to the variable's
                // declared `ty`, so the wrapper is redundant; bind the raw suspend call underneath it.
                let suspension =
                    unwrap_suspend_cast(self.ir, *init, self.suspend, /* ref_only */ false);
                is_suspension_point(self.ir, suspension.point, self.suspend).then(|| {
                    (
                        Some((*index, ty.clone())),
                        suspension.point,
                        suspension_completion(suspension, false),
                    )
                })
            }
            _ => {
                // A provider boundary may wrap a physical suspend call in an implicit coercion to
                // its checked semantic result (notably a bare `Unit` call). Bind/forward the raw call
                // just as the variable-initializer path above does; appending a continuation to the
                // wrapper itself is a no-op and leaves a `$default` descriptor one operand short.
                let suspension =
                    unwrap_suspend_cast(self.ir, stmt, self.suspend, /* ref_only */ false);
                is_suspension_point(self.ir, suspension.point, self.suspend).then(|| {
                    (
                        None,
                        suspension.point,
                        suspension_completion(suspension, true),
                    )
                })
            }
        }
    }
    /// If `stmt` is `val L = when { … }` where a branch value is a direct suspension, return
    /// `(L, ty, branches)`. Sets `failed` if a branch hides a suspension the flattener can't lift.
    fn stmt_cond_suspension(&mut self, stmt: ExprId) -> Option<(u32, Ty, Branches)> {
        let IrExpr::Variable {
            index,
            ty,
            init: Some(init),
            ..
        } = &self.ir.exprs[stmt as usize]
        else {
            return None;
        };
        let (index, ty, init) = (*index, ty.clone(), *init);
        let IrExpr::When { branches } = &self.ir.exprs[init as usize] else {
            return None;
        };
        let branches = branches.clone();
        // A branch value may wrap the suspension in a redundant `Cast`/`ImplicitCoercion` (the
        // safe-call lowering boxes a primitive member result so both arms are references:
        // `b?.f()` → `when { b != null -> box(f.invoke(b)), else -> null }`). Post-CPS the call
        // returns `Object` and `bind_from_r` unboxes to the DECLARED ty, so the wrapper is
        // redundant — see through it here and in `emit_cond`.
        let any_susp = branches.iter().any(|(_, v)| {
            is_suspension_point(
                self.ir,
                unwrap_suspend_cast(self.ir, *v, self.suspend, /* ref_only */ false).point,
                self.suspend,
            )
        });
        // `val v = expr ?: continue` lowers to `val v = when { c -> expr; else -> continue }` — a branch
        // whose VALUE is a loop-jump binds nothing and diverges to the loop's cont/break state. Route the
        // whole binding through `emit_cond` (state-split) so the jump becomes a tail `goto`; otherwise the
        // structured `Continue`/`Break` sits in the merge's value slot → a stackmap/verify mismatch.
        let any_jump = branches
            .iter()
            .any(|(_, value)| self.expr_jumps_to_active_frame(*value));
        if !any_susp && !any_jump {
            // No DIRECT branch suspension/jump — but one hiding DEEPER in a branch value (`val q =
            // f() ?: if (c) break else error("")`) would be emitted structurally with a mis-framed
            // jump into the dispatch loop. Bail those; a jump-free/suspension-free `when` binding is
            // the ordinary case and stays structural.
            if self.expr_has_loop_jump(init) || expr_calls_suspend(self.ir, init, self.suspend) {
                crate::trace_compiler!(
                    "suspend",
                    "conditional suspension BAIL: hidden control in binding init={:?}",
                    self.ir.exprs[init as usize]
                );
                self.failed = true;
            }
            return None;
        }
        // A branch value must be a direct suspension/jump, a nested decision that can be split into
        // another state, or free of both. Arbitrary expressions with hidden control flow remain invalid:
        // the earlier hoist pass is responsible for exposing those as statements.
        for (_, v) in &branches {
            let direct_jump = matches!(
                self.ir.exprs[*v as usize],
                IrExpr::Break { .. } | IrExpr::Continue { .. }
            );
            let uv = unwrap_suspend_cast(self.ir, *v, self.suspend, /* ref_only */ false);
            let nested_control = matches!(
                self.ir.exprs[*v as usize],
                IrExpr::When { .. } | IrExpr::Block { .. }
            );
            if !is_suspension_point(self.ir, uv.point, self.suspend)
                && !direct_jump
                && !nested_control
                && (expr_calls_suspend(self.ir, *v, self.suspend)
                    || self.expr_jumps_to_active_frame(*v))
            {
                crate::trace_compiler!(
                    "suspend",
                    "conditional suspension BAIL: hidden control in branch value={:?}",
                    self.ir.exprs[*v as usize]
                );
                self.failed = true;
                return None;
            }
        }
        Some((index, ty, branches))
    }
    /// Emit the `when` for a conditional suspension binding `L`; every branch computes `L` and `goto`s
    /// `merge`. A suspending branch routes through its own resume state.
    fn emit_cond(
        &mut self,
        local: u32,
        ty: &Ty,
        branches: &[(Option<ExprId>, ExprId)],
        merge: usize,
    ) -> ExprId {
        let mut out_branches: Branches = Vec::new();
        for (cond, value) in branches {
            let mut bb: Vec<ExprId> = Vec::new();
            let jump = match &self.ir.exprs[*value as usize] {
                IrExpr::Break { label } => Some((label.clone(), true)),
                IrExpr::Continue { label } => Some((label.clone(), false)),
                _ => None,
            };
            // A cast/coercion-wrapped direct suspension binds the RAW call — `bind_from_r` already
            // unboxes the resumed `Object` to the declared ty (see `stmt_cond_suspension`).
            let suspension =
                unwrap_suspend_cast(self.ir, *value, self.suspend, /* ref_only */ false);
            if let Some((label, is_break)) = jump {
                // A loop-jump branch: transfer to the loop's cont/break state; bind nothing (it diverges).
                if !self.emit_loop_jump(&mut bb, *value, label.as_deref(), is_break) {
                    self.goto(&mut bb, merge);
                }
            } else if is_suspension_point(self.ir, suspension.point, self.suspend) {
                let br_resume = self.new_state();
                let completion = suspension_completion(suspension, false);
                self.emit_suspension(&mut bb, suspension.point, br_resume, completion);
                let mut rs: Vec<ExprId> = Vec::new();
                if completion.resume_diverges {
                    self.terminate_bottom_resume(&mut rs);
                } else {
                    self.bind_from_r(&mut rs, local, ty, br_resume, suspension.point);
                    self.goto(&mut rs, merge);
                }
                self.states[br_resume] = rs;
            } else if matches!(
                self.ir.exprs[*value as usize],
                IrExpr::When { .. } | IrExpr::Block { .. }
            ) && (expr_calls_suspend(self.ir, *value, self.suspend)
                || self.expr_jumps_to_active_frame(*value))
            {
                // Preserve source decision order while recursively exposing control flow. The outer
                // branch merely enters a state whose ordinary statement is the same binding; `flatten`
                // then applies this rule again to the nested `when`. This handles arbitrarily deep
                // Elvis/if/when trees without embedding a structured jump into a dispatch-loop value.
                let branch_entry = self.new_state();
                self.goto(&mut bb, branch_entry);
                let branch_stmts = match self.ir.exprs[*value as usize].clone() {
                    IrExpr::Block { mut stmts, value } => {
                        if let Some(value) = value {
                            stmts.push(self.add(IrExpr::Variable {
                                index: local,
                                ty: ty.clone(),
                                init: Some(value),
                                named: false,
                            }));
                        }
                        stmts
                    }
                    IrExpr::When { .. } => vec![self.add(IrExpr::Variable {
                        index: local,
                        ty: ty.clone(),
                        init: Some(*value),
                        named: false,
                    })],
                    _ => unreachable!("nested conditional branch shape was checked above"),
                };
                self.flatten(&branch_stmts, branch_entry, Some(merge));
            } else {
                let diverges = stmt_diverges(self.ir, *value);
                if diverges {
                    // A bottom-valued branch never produces the value to store. Keeping the binding
                    // wrapper would emit an unreachable `astore` immediately after `athrow`, for which
                    // the JVM verifier correctly requires a separate frame.
                    bb.push(*value);
                } else {
                    if self.is_spilled(local) {
                        bb.push(self.add(IrExpr::SetValue {
                            var: local,
                            value: *value,
                        }));
                    } else {
                        bb.push(self.add(IrExpr::Variable {
                            index: local,
                            ty: ty.clone(),
                            init: Some(*value),
                            named: false,
                        }));
                    }
                    self.mark_assigned(local);
                    self.goto(&mut bb, merge);
                }
            }
            let block = self.add(IrExpr::Block {
                stmts: bb,
                value: None,
            });
            out_branches.push((*cond, block));
        }
        self.add(IrExpr::When {
            branches: out_branches,
        })
    }
    /// Emit the `when` for an `if`/`when` STATEMENT whose branch body suspends: each branch `goto`s its
    /// own entry state (which flattens the branch body, converging at `merge`); a missing `else` falls
    /// through straight to `merge`.
    fn emit_when_stmt(&mut self, branches: Branches, merge: usize) -> ExprId {
        let mut out_branches: Branches = Vec::new();
        let mut has_else = false;
        for (cond, body) in &branches {
            has_else |= cond.is_none();
            let entry = self.new_state();
            let mut bb: Vec<ExprId> = Vec::new();
            self.goto(&mut bb, entry);
            let block = self.add(IrExpr::Block {
                stmts: bb,
                value: None,
            });
            out_branches.push((*cond, block));
            let body_stmts = self.block_stmts(*body);
            self.flatten(&body_stmts, entry, Some(merge));
        }
        if !has_else {
            let mut bb: Vec<ExprId> = Vec::new();
            self.goto(&mut bb, merge);
            let block = self.add(IrExpr::Block {
                stmts: bb,
                value: None,
            });
            out_branches.push((None, block));
        }
        self.add(IrExpr::When {
            branches: out_branches,
        })
    }

    fn split_when_statement(
        &mut self,
        branches: Branches,
        mut out: Vec<ExprId>,
        cur: usize,
        rest: &[ExprId],
        after: Option<usize>,
    ) {
        let merge = self.new_state();
        let when = self.emit_when_stmt(branches, merge);
        out.push(when);
        self.states[cur] = out;
        self.flatten(rest, merge, after);
    }
    /// The statement list of a branch body (a `Block`'s statements, or the single expression itself).
    fn block_stmts(&self, body: ExprId) -> Vec<ExprId> {
        match &self.ir.exprs[body as usize] {
            IrExpr::Block { stmts, value } => {
                let mut v = stmts.clone();
                v.extend(value.iter().copied());
                v
            }
            _ => vec![body],
        }
    }
    /// A plain (non-suspending) statement. A `Variable` declaration of a spilled local becomes a
    /// `SetValue` (the local is already declared at the loop top).
    fn rewrite_plain(&mut self, stmt: ExprId) -> ExprId {
        if let IrExpr::Variable {
            index,
            init: Some(init),
            ..
        } = self.ir.exprs[stmt as usize]
        {
            if self.is_spilled(index) {
                self.mark_assigned(index);
                return self.add(IrExpr::SetValue {
                    var: index,
                    value: init,
                });
            }
        }
        if let IrExpr::SetValue { var, .. } = self.ir.exprs[stmt as usize] {
            self.mark_assigned(var);
        }
        stmt
    }
    /// Flatten `stmts` into state `cur`, transferring to `after` (if any) when the sequence falls through.
    fn flatten(&mut self, stmts: &[ExprId], cur: usize, after: Option<usize>) {
        let mut out: Vec<ExprId> = std::mem::take(&mut self.states[cur]);
        for i in 0..stmts.len() {
            if self.failed {
                self.states[cur] = out;
                return;
            }
            let stmt = stmts[i];
            // A `continue`/`break` inside a suspending loop's body: emit a `goto` to the loop's
            // continue/break state (resolved from the loop-target stack — innermost, or the frame whose
            // label matches). The structured node can't survive flattening; anything after it in this
            // sequence is unreachable. Falls through to the plain path when no matching loop frame is in
            // scope (e.g. a loop whose own body doesn't suspend, handled structurally by the emitter).
            // A `Variable { init: Block { stmts, value } }` — an elvis / safe-call subject lowers its
            // subject into the block's statements and the result `when` into the block's value
            // (`val v = m[i] ?: continue` → `{ val t = m[i]; when { t != null -> t; else -> continue } }`).
            // `normalize_block_inits` unwraps these only at the function-body top level, not inside a loop.
            // When such an init carries a loop-jump (or suspension), splice it (`stmts…; val v = value;
            // rest`) so the inner `when` reaches `stmt_cond_suspension` / the jump reaches its handler.
            if let IrExpr::Variable {
                index,
                ty,
                init: Some(init),
                named,
            } = self.ir.exprs[stmt as usize].clone()
            {
                if let IrExpr::Block {
                    stmts: bs,
                    value: Some(bv),
                } = self.ir.exprs[init as usize].clone()
                {
                    if !self.ir.intrinsic_suspension_points.contains_key(&init)
                        && (self.expr_has_loop_jump(stmt)
                            || expr_calls_suspend(self.ir, stmt, self.suspend)
                            || self.expr_jumps_to_active_frame(stmt))
                    {
                        let rebind = self.add(IrExpr::Variable {
                            index,
                            ty,
                            init: Some(bv),
                            named,
                        });
                        let mut spliced = bs;
                        spliced.push(rebind);
                        spliced.extend_from_slice(&stmts[i + 1..]);
                        self.states[cur] = out;
                        self.flatten(&spliced, cur, after);
                        return;
                    }
                }
            }
            if let IrExpr::Break { label } | IrExpr::Continue { label } =
                self.ir.exprs[stmt as usize].clone()
            {
                let is_break = matches!(self.ir.exprs[stmt as usize], IrExpr::Break { .. });
                if self.emit_loop_jump(&mut out, stmt, label.as_deref(), is_break) {
                    self.states[cur] = out;
                    return;
                }
            }
            if let Some((bind, call, completion)) = self.stmt_suspension(stmt) {
                let resume = self.new_state();
                self.emit_suspension(&mut out, call, resume, completion);
                self.states[cur] = out;
                let mut rs: Vec<ExprId> = Vec::new();
                if completion.resume_diverges {
                    self.terminate_bottom_resume(&mut rs);
                } else if let Some((local, ty)) = bind {
                    self.bind_from_r(&mut rs, local, &ty, resume, call);
                }
                self.states[resume] = rs;
                if !completion.resume_diverges {
                    self.flatten(&stmts[i + 1..], resume, after);
                }
                return;
            }
            if let Some((local, ty, when_branches)) = self.stmt_cond_suspension(stmt) {
                let merge = self.new_state();
                let when = self.emit_cond(local, &ty, &when_branches, merge);
                out.push(when);
                self.states[cur] = out;
                self.flatten(&stmts[i + 1..], merge, after);
                return;
            }
            // A bare `Block` STATEMENT that suspends (e.g. a `for` loop desugars to
            // `{ val it = xs.iterator(); while (it.hasNext()) { … } }`, spliced into the body as one
            // block) or carries a jump to an outer flattened loop (a `?.let { return@withLock v }` whose
            // safe-call/let expansion is a `Block { …, When }` holding the labeled break). Inline its
            // statements into the flattening stream — IR locals are flat-indexed, so the block is pure
            // grouping and can be flattened away. A trailing VALUE in this statement position is discarded,
            // so re-emit it as a trailing statement (reached on the paths that don't take the jump).
            if let IrExpr::Block {
                stmts: inner,
                value,
            } = &self.ir.exprs[stmt as usize]
            {
                let (inner, value) = (inner.clone(), *value);
                // A value-carrying block only splices for the jump case (its discarded trailing value is
                // re-emitted below); a suspending block with a trailing value stays an expression position
                // handled elsewhere.
                if (value.is_none() && expr_calls_suspend(self.ir, stmt, self.suspend))
                    || self.expr_jumps_to_active_frame(stmt)
                {
                    crate::trace_compiler!(
                        "suspend",
                        "flatten: splicing suspending block stmt with {} inner stmts",
                        inner.len()
                    );
                    let mut spliced: Vec<ExprId> = inner;
                    if let Some(v) = value {
                        spliced.push(v);
                    }
                    spliced.extend_from_slice(&stmts[i + 1..]);
                    self.states[cur] = out;
                    self.flatten(&spliced, cur, after);
                    return;
                }
            }
            // The same structural block may be wrapped in a checked result coercion. If it cannot
            // fall through and exits an active flattened frame, the coercion is unreachable; feed
            // the block statements back through the ordinary flattener so the selected jump becomes
            // a state transition.
            if self.expr_jumps_to_active_frame(stmt) {
                if let Some(control) = diverging_control_core(self.ir, stmt) {
                    match self.ir.exprs[control as usize].clone() {
                        IrExpr::Block {
                            stmts: mut inner,
                            value: None,
                        } => {
                            inner.extend_from_slice(&stmts[i + 1..]);
                            self.states[cur] = out;
                            self.flatten(&inner, cur, after);
                            return;
                        }
                        IrExpr::When { branches } => {
                            self.split_when_statement(branches, out, cur, &stmts[i + 1..], after);
                            return;
                        }
                        IrExpr::Break { label } | IrExpr::Continue { label } => {
                            let is_break =
                                matches!(self.ir.exprs[control as usize], IrExpr::Break { .. });
                            if self.emit_loop_jump(&mut out, control, label.as_deref(), is_break) {
                                self.states[cur] = out;
                                return;
                            }
                        }
                        _ => {}
                    }
                }
            }
            // An `if`/`when` STATEMENT whose branch body suspends: route each branch through its own
            // entry state (which flattens the branch), all converging at `merge`.
            // Also fire when a branch carries a `continue`/`break` for the enclosing suspending loop
            // (`if (c) continue`): a loop-jump can only transfer control from a state via a tail `goto`, so
            // the branch must live in its own state (where its `Continue`/`Break` becomes a `goto` to the
            // loop's cont/break state) — exactly the state-split `emit_when_stmt` performs.
            if let IrExpr::When { branches } = &self.ir.exprs[stmt as usize] {
                if expr_calls_suspend(self.ir, stmt, self.suspend)
                    || self.expr_has_loop_jump(stmt)
                    || self.expr_jumps_to_active_frame(stmt)
                {
                    let branches = branches.clone();
                    self.split_when_statement(branches, out, cur, &stmts[i + 1..], after);
                    return;
                }
            }
            // A `while`/`do`-`while` loop whose condition, body, or update suspends: header (test) ↔
            // body ↔ exit. A pre-test loop enters at the header; a post-test (`do`-`while`) enters at
            // the body (runs once first).
            if let IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } = &self.ir.exprs[stmt as usize]
            {
                if expr_calls_suspend(self.ir, *cond, self.suspend)
                    || expr_calls_suspend(self.ir, *body, self.suspend)
                    || update.is_some_and(|u| expr_calls_suspend(self.ir, u, self.suspend))
                    || self.expr_jumps_to_active_frame(*body)
                {
                    let (cond, body, update, post_test, label) =
                        (*cond, *body, *update, *post_test, label.clone());
                    let header = self.new_state();
                    let body_entry = self.new_state();
                    let cont = self.new_state();
                    // When the loop has NO continuation (`stmts[i+1..]` is empty — e.g. the `while(true){
                    // …; break }` wrapper an inlined `withLock`/labeled-return uses, whose only exit is
                    // the break), route `break`/the loop exit straight to `after` rather than a separate
                    // empty exit state (a `goto`-only state whose label the emitter binds one past the
                    // code → the break jumps to a frameless out-of-range offset). Otherwise a real exit
                    // state carries the rest.
                    let rest_empty = stmts[i + 1..].is_empty();
                    let exit = match after {
                        Some(a) if rest_empty => a,
                        _ => self.new_state(),
                    };
                    // A suspending condition was normalized to `Block { prelude, value: bool }` by
                    // `hoist_stmt`. Its prelude belongs to the header state and therefore re-runs on
                    // every back-edge. Refuse any residual suspension in the Boolean value itself: it
                    // means the generic expression hoister could not preserve that shape safely.
                    let (mut condition_stmts, condition_value) =
                        match self.ir.exprs[cond as usize].clone() {
                            IrExpr::Block {
                                stmts,
                                value: Some(value),
                            } => (stmts, value),
                            _ => (Vec::new(), cond),
                        };
                    if expr_calls_suspend(self.ir, condition_value, self.suspend) {
                        crate::trace_compiler!(
                            "suspend",
                            "flatten BAIL: residual suspension in loop condition value {:?}",
                            self.ir.exprs[condition_value as usize]
                        );
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    // cur → header (pre-test) or → body (post-test runs the body once before testing)
                    self.goto(&mut out, if post_test { body_entry } else { header });
                    self.states[cur] = out;
                    // header: when(cond){ true → body_entry; else → exit }
                    let mut hs: Vec<ExprId> = Vec::new();
                    let t_block = {
                        let mut b = Vec::new();
                        self.goto(&mut b, body_entry);
                        self.add(IrExpr::Block {
                            stmts: b,
                            value: None,
                        })
                    };
                    let e_block = {
                        let mut b = Vec::new();
                        self.goto(&mut b, exit);
                        self.add(IrExpr::Block {
                            stmts: b,
                            value: None,
                        })
                    };
                    let hwhen = self.add(IrExpr::When {
                        branches: vec![(Some(condition_value), t_block), (None, e_block)],
                    });
                    hs.push(hwhen);
                    condition_stmts.extend(hs);
                    // body → cont (back to header after the update). A `continue` in the body targets
                    // `cont` (the update+re-test), a `break` targets `exit`; push the frame so a
                    // `Continue`/`Break` statement flattens to the right `goto` rather than surviving as a
                    // structured node aimed at the dispatch loop.
                    let body_stmts = self.block_stmts(body);
                    self.loop_targets.push((label, cont, exit));
                    self.flatten(&condition_stmts, header, None);
                    self.flatten(&body_stmts, body_entry, Some(cont));
                    // cont: run the loop update (a `for`-loop increment + the counted-loop bound-check
                    // `break`), then back to header. FLATTEN it (with the loop frame still active) rather
                    // than `rewrite_plain`, so a `break` in the update — the overflow-safe counted-loop
                    // bound check `if (i == last) break` — routes to `exit` instead of surviving as a
                    // structured node aimed at the dispatch loop.
                    let update_stmts: Vec<ExprId> =
                        update.map(|u| self.block_stmts(u)).unwrap_or_default();
                    self.flatten(&update_stmts, cont, Some(header));
                    self.loop_targets.pop();
                    // exit: the rest (skipped when the exit IS `after` — nothing follows the loop).
                    if !(rest_empty && Some(exit) == after) {
                        self.flatten(&stmts[i + 1..], exit, after);
                    }
                    return;
                }
            }
            // A `try { … } catch (e) { … }` STATEMENT whose body suspends or transfers to an active
            // flattened-loop frame. Modeled shapes: one or more NON-suspending catches, or a SINGLE
            // straight-line catch that MAY itself suspend — no `finally` either way. The try-body
            // states are marked with a handler; the
            // assembly's dispatch `catch` routes an exception thrown while `this.label` is one of
            // them to the handler state, leaving a suspension BEFORE/AFTER the try uncaught; the
            // handler re-checks the exception's type per arm (`instanceof`) and re-throws a
            // non-matching one. Richer shapes (finally combinations, a suspending catch among
            // several, a BRANCH in a suspending catch) skip the whole file.
            if let IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } = &self.ir.exprs[stmt as usize]
            {
                if expr_calls_suspend(self.ir, stmt, self.suspend)
                    || self.expr_jumps_to_active_frame(stmt)
                {
                    let (body, catches, finally) = (*body, catches.clone(), *finally);
                    // A try-FINALLY (no catch): the finally must run on BOTH exits of the suspending try
                    // body — normal completion (→ continue after the try) and an exception (→ run finally,
                    // then re-throw). Model it with a `fin_normal` state (normal path) and a `fin_handler`
                    // state (the try region's exception handler); the finally block is emitted in each.
                    // Scoped to a NON-suspending finally (a suspending one would itself span states) and a
                    // body with no bare `return` (a function return inside the try needs a
                    // finally-before-return transfer not yet modeled). Other finally shapes skip the file.
                    if let Some(fin) = finally {
                        if catches.is_empty()
                            && !expr_calls_suspend(self.ir, fin, self.suspend)
                            && !expr_has_return(self.ir, body)
                        {
                            let saved = self.cur_handler;
                            let try_after = self.new_state();
                            let fin_handler = self.new_state();
                            self.cur_handler = Some(fin_handler);
                            let try_entry = self.new_state();
                            let fin_normal = self.new_state();
                            self.goto(&mut out, try_entry);
                            self.states[cur] = out;
                            // Definite-assignment on entry to the try (before the body's own writes) —
                            // the handler is reached exceptionally WITHOUT those writes.
                            let a_entry = self.assigned.clone();
                            let body_stmts = self.block_stmts(body);
                            self.jump_finalizers
                                .push((self.loop_targets.len(), fin, saved));
                            self.flatten(&body_stmts, try_entry, Some(fin_normal));
                            self.jump_finalizers.pop();
                            let a_body = self.assigned.clone();
                            self.cur_handler = saved;
                            // Normal path: run the finally, then fall through to after the try. The body
                            // completed, so its writes are in scope here.
                            self.assigned = a_body;
                            let fin_stmts = self.block_stmts(fin);
                            self.flatten(&fin_stmts, fin_normal, Some(try_after));
                            let a_after = self.assigned.clone();
                            // Exceptional path: the stashed exception arrives in `r_v` (loaded at the loop
                            // top, like a resume value). Run the finally, then re-throw it. Reached without
                            // the body's writes → start from the pre-try assignment set.
                            self.assigned = a_entry;
                            let mut fh_stmts = self.block_stmts(fin);
                            let rv = self.gv(self.r_v);
                            // `r_v` is typed `Object` (the resume/exception slot); `athrow` needs a
                            // `Throwable`.
                            let exc = self.add(IrExpr::TypeOp {
                                op: IrTypeOp::Cast,
                                arg: rv,
                                type_operand: Ty::obj("java/lang/Throwable"),
                            });
                            fh_stmts.push(self.add(IrExpr::Throw { operand: exc }));
                            self.flatten(&fh_stmts, fin_handler, None);
                            // Continue after the try (normal path only).
                            self.assigned = a_after;
                            let rest: Vec<ExprId> = stmts[i + 1..].to_vec();
                            self.flatten(&rest, try_after, after);
                            return;
                        }
                        // A finally combined with a catch, a suspending finally, or a return in the try
                        // body is unmodeled — skip the file rather than miscompile.
                        crate::trace_compiler!(
                            "suspend",
                            "flatten BAIL: unnormalized finally catches={} fin_suspends={} body_returns={} try={:?}",
                            catches.len(),
                            expr_calls_suspend(self.ir, fin, self.suspend),
                            expr_has_return(self.ir, body),
                            self.ir.exprs[stmt as usize]
                        );
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    if catches.is_empty()
                        || catches.iter().any(|catch| {
                            expr_calls_suspend(self.ir, catch.body, self.suspend)
                                && !self.catch_spills.contains_key(&catch.body)
                        })
                    {
                        crate::trace_compiler!(
                            "suspend",
                            "flatten BAIL: catch spill invariant failed catches={catches:?} spills={:?}",
                            self.catch_spills
                        );
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    let saved = self.cur_handler;
                    // `try_after` and `handler` belong to the ENCLOSING handler region, not this try's.
                    let try_after = self.new_state();
                    let handler = self.new_state();
                    self.cur_handler = Some(handler);
                    let try_entry = self.new_state();
                    self.goto(&mut out, try_entry);
                    self.states[cur] = out;
                    // Definite-assignment on entry to the try (= before the body's own writes). The
                    // handler is reached via the exceptional edge WITHOUT the body's writes, so it must
                    // start from this set — not the body's accumulated one — else a body-only local
                    // would be spilled dead at handler→try_after.
                    let a_entry = self.assigned.clone();
                    let body_stmts = self.block_stmts(body);
                    self.flatten(&body_stmts, try_entry, Some(try_after));
                    let a_body = std::mem::replace(&mut self.assigned, a_entry);
                    self.cur_handler = saved;
                    // Handler state: the stashed exception arrives in `result` (loaded into `r_v` at
                    // the loop top, like a resume value). The dispatch's exception routing is a
                    // catch-ALL (`catch (Throwable)` around the state loop), so the handler must
                    // re-check the exception's type itself — each catch arm is guarded by
                    // `instanceof` and a non-matching exception RE-THROWS (kotlinc semantics: it
                    // propagates out of the coroutine; running the arm unguarded silently swallowed
                    // it). A `Throwable`-typed catch needs no guard and makes later arms dead.
                    let mut arms: Branches = Vec::new();
                    let mut full_cover = false;
                    for catch in catches {
                        let exc_internal = catch.exc_internal.render();
                        let exc_ty = Ty::obj(&exc_internal);
                        let arm_suspends = expr_calls_suspend(self.ir, catch.body, self.suspend);
                        let arm_stmts: Vec<ExprId> = if arm_suspends {
                            // The catch body itself suspends, so `r_v` is clobbered by its own
                            // resume. Bind the exception ONCE from `r_v` on arm entry into its
                            // spilled local `ev` (whose reads were pre-rewritten in
                            // `build_state_machine`); the spill machinery then carries it across
                            // the catch's suspension and restores it for the later reads
                            // (`throw e`).
                            let ev = self.catch_spills[&catch.body];
                            let rv = self.gv(self.r_v);
                            let cast = self.add(IrExpr::TypeOp {
                                op: IrTypeOp::Cast,
                                arg: rv,
                                type_operand: exc_ty,
                            });
                            let bind = self.add(IrExpr::SetValue {
                                var: ev,
                                value: cast,
                            });
                            let mut cs = vec![bind];
                            cs.extend(self.block_stmts(catch.body));
                            cs
                        } else {
                            // A NON-suspending catch body: `r_v` still holds the exception
                            // throughout, so read it there directly. Avoids a catch-variable LOCAL
                            // — which the IR's value-index reuse can alias with a body local of
                            // another type, and which the emitter can slot-coalesce with an `int`
                            // temp (a ref stored into an int slot → VerifyError).
                            let mut reads: Vec<ExprId> = Vec::new();
                            collect_getvalue(self.ir, catch.body, catch.var, &mut reads);
                            for n in reads {
                                let rv = self.gv(self.r_v);
                                self.ir.exprs[n as usize] = IrExpr::TypeOp {
                                    op: IrTypeOp::Cast,
                                    arg: rv,
                                    type_operand: exc_ty,
                                };
                            }
                            self.block_stmts(catch.body)
                        };
                        let blk = self.add(IrExpr::Block {
                            stmts: arm_stmts,
                            value: None,
                        });
                        full_cover = matches!(
                            exc_internal.as_str(),
                            "kotlin/Throwable" | "java/lang/Throwable"
                        );
                        if full_cover {
                            arms.push((None, blk));
                            break; // later arms are dead
                        }
                        let rv = self.gv(self.r_v);
                        let guard = self.add(IrExpr::TypeOp {
                            op: IrTypeOp::InstanceOf,
                            arg: rv,
                            type_operand: exc_ty,
                        });
                        arms.push((Some(guard), blk));
                    }
                    if !full_cover {
                        // else: no arm matches — re-throw the stashed exception.
                        let rv = self.gv(self.r_v);
                        let exc = self.add(IrExpr::TypeOp {
                            op: IrTypeOp::Cast,
                            arg: rv,
                            type_operand: Ty::obj("java/lang/Throwable"),
                        });
                        let thr = self.add(IrExpr::Throw { operand: exc });
                        arms.push((
                            None,
                            self.add(IrExpr::Block {
                                stmts: vec![thr],
                                value: None,
                            }),
                        ));
                    }
                    let catch_stmts: Vec<ExprId> = if arms.len() == 1 {
                        // A single unguarded arm (a full-coverage catch) keeps the direct shape.
                        self.block_stmts(arms[0].1)
                    } else {
                        vec![self.add(IrExpr::When { branches: arms })]
                    };
                    self.flatten(&catch_stmts, handler, Some(try_after));
                    // `try_after` joins the body and handler paths: a spilled local is definitely
                    // assigned there only if assigned on BOTH (intersection).
                    let a_handler = std::mem::take(&mut self.assigned);
                    self.assigned = a_body.intersection(&a_handler).copied().collect();
                    self.flatten(&stmts[i + 1..], try_after, after);
                    return;
                }
            }
            // A VALUE-bearing `Block` in STATEMENT position (`recv?.let { susp(…) }` inside an `if`
            // branch, result discarded): the value runs for effect only — splice its statements plus
            // the demoted value into this sequence and continue flattening.
            if expr_calls_suspend(self.ir, stmt, self.suspend) {
                if let IrExpr::Block {
                    stmts: bs,
                    value: Some(v),
                } = self.ir.exprs[stmt as usize].clone()
                {
                    let mut spliced = bs;
                    spliced.push(v);
                    spliced.extend_from_slice(&stmts[i + 1..]);
                    self.states[cur] = out;
                    self.flatten(&spliced, cur, after);
                    return;
                }
            }
            if expr_calls_suspend(self.ir, stmt, self.suspend) {
                trace_residual_suspension(self.ir, stmt, 0);
                trace_residual_parents(self.ir, stmt);
                if let IrExpr::Variable { init: Some(i), .. } = self.ir.exprs[stmt as usize] {
                    crate::trace_compiler!(
                        "suspend",
                        "flatten BAIL: Variable init node = {:?}",
                        self.ir.exprs[i as usize]
                    );
                    if let IrExpr::Block { stmts: bs, .. } = &self.ir.exprs[i as usize] {
                        for &bsi in bs {
                            crate::trace_compiler!(
                                "suspend",
                                "flatten BAIL: block stmt = {:?}",
                                self.ir.exprs[bsi as usize]
                            );
                        }
                    }
                }
                crate::trace_compiler!(
                    "suspend",
                    "flatten BAIL: unhandled suspending stmt {:?}",
                    self.ir.exprs[stmt as usize]
                );
                self.failed = true;
                self.states[cur] = out;
                return;
            }
            if self.expr_jumps_to_active_frame(stmt) {
                // A structured `break`/`continue` aimed at a FLATTENED (state-split) loop, buried in
                // a plain statement none of the modeled shapes claimed (`val q = f() ?: if (c) break
                // else …`): emitting it structurally would jump into the dispatch loop with a
                // mismatched frame (or loop forever). Bail — skip, never miscompile.
                crate::trace_compiler!(
                    "suspend",
                    "flatten BAIL: unmodeled loop-jump in plain stmt {:?}",
                    self.ir.exprs[stmt as usize]
                );
                trace_residual_suspension(self.ir, stmt, 0);
                trace_residual_parents(self.ir, stmt);
                self.failed = true;
                self.states[cur] = out;
                return;
            }
            let s2 = self.rewrite_plain(stmt);
            out.push(s2);
        }
        // Transfer to `after` on fall-through — but ONLY if the sequence can fall through. If the last
        // statement diverges (`return`/`throw`), the transition is unreachable dead code: emitting it
        // would leave a `goto` after a `return`/`athrow` with no stack-map frame → a load-time
        // VerifyError. A `return` STATEMENT inside a suspend `try` body, or a `throw` ending a catch body,
        // both hit this.
        let diverges = stmts.last().is_some_and(|&s| stmt_diverges(self.ir, s));
        crate::trace_compiler!(
            "suspend",
            "flatten exit state={cur} after={after:?} diverges={diverges} last={:?}",
            stmts.last().map(|&s| &self.ir.exprs[s as usize])
        );
        if !diverges {
            if let Some(a) = after {
                self.goto(&mut out, a);
            }
        }
        self.states[cur] = out;
    }
}

/// Whether `expression` contains a `break`/`continue` that must escape this value position. A nested
/// loop owns its own unlabeled exits, and a lambda is always a control-flow boundary. A labeled exit
/// is safe to expose without resolving it here: branch binding preserves its label and `Flat` uses
/// the active loop stack to route that exact target.
fn expr_contains_owned_loop_jump(ir: &IrFile, expression: ExprId) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::Break { .. } | IrExpr::Continue { .. } => true,
        IrExpr::While { .. } | IrExpr::Lambda { .. } => false,
        _ => {
            let mut found = false;
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                found = found || expr_contains_owned_loop_jump(ir, child);
            });
            found
        }
    }
}

/// Whether `e`'s subtree contains a bare `return` (a function return), NOT descending into a nested
/// lambda (whose `return` is its own). A `return` inside a suspending try body needs a
/// finally-before-return transfer the flattener does not yet model, so the try-finally path declines it.
fn expr_has_return(ir: &IrFile, e: ExprId) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::Return(_) => true,
        IrExpr::Lambda { .. } => false,
        _ => {
            let mut found = false;
            crate::ir::for_each_child(&ir.exprs, e, &mut |c| {
                found = found || expr_has_return(ir, c);
            });
            found
        }
    }
}

/// Whether statement `s` always transfers control away (never falls through): a `return`/`throw`, or a
/// block/`when` all of whose exits do. Used to suppress a dead fall-through transition after it.
fn stmt_diverges(ir: &IrFile, s: ExprId) -> bool {
    ir.expr_discarding_diverges_by(s, &|_, _| false)
}

/// Collect the value-indices read (`GetValue`) anywhere in `e`'s subtree.
fn collect_reads(ir: &IrFile, e: ExprId, out: &mut Vec<u32>) {
    visit_subtree(&ir.exprs, e, &mut |node| {
        if let IrExpr::GetValue(i) = node {
            out.push(*i);
        }
    });
}

fn stmt_writes(ir: &IrFile, e: ExprId, idx: u32) -> bool {
    match ir.exprs[e as usize] {
        IrExpr::Variable { index, .. } if index == idx => return true,
        IrExpr::SetValue { var, .. } if var == idx => return true,
        _ => {}
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if stmt_writes(ir, c, idx) {
            found = true;
        }
    });
    found
}

/// Whether EVERY read of `idx` under `stmt` sits inside a suspend-call subtree (the call's
/// receiver/arguments — all evaluated BEFORE the suspension), so no read can observe a resume.
fn reads_only_in_suspension_args(
    ir: &IrFile,
    stmt: ExprId,
    idx: u32,
    suspend_set: &HashSet<u32>,
) -> bool {
    fn walk(ir: &IrFile, e: ExprId, idx: u32, suspend_set: &HashSet<u32>, outside: &mut bool) {
        if is_suspension_point(ir, e, suspend_set) {
            // Everything below evaluates before the suspension — reads here are safe.
            return;
        }
        if matches!(ir.exprs[e as usize], IrExpr::GetValue(i) if i == idx) {
            *outside = true;
            return;
        }
        for_each_child(&ir.exprs, e, &mut |c| {
            walk(ir, c, idx, suspend_set, outside)
        });
    }
    let mut outside = false;
    walk(ir, stmt, idx, suspend_set, &mut outside);
    !outside
}

/// Whether the subtree under `e` reads local `idx` (a `GetValue(idx)` node).
fn expr_reads(ir: &IrFile, e: ExprId, idx: u32) -> bool {
    if matches!(ir.exprs[e as usize], IrExpr::GetValue(i) if i == idx) {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if expr_reads(ir, c, idx) {
            found = true;
        }
    });
    found
}

/// The initializer of the `IrExpr::Variable { index: idx, .. }` declaration under `e`, if any.
fn find_var_init(ir: &IrFile, e: ExprId, idx: u32) -> Option<ExprId> {
    if let IrExpr::Variable { index, init, .. } = ir.exprs[e as usize] {
        if index == idx {
            return init;
        }
    }
    let mut found = None;
    for_each_child(&ir.exprs, e, &mut |c| {
        if found.is_none() {
            found = find_var_init(ir, c, idx);
        }
    });
    found
}

/// The number of suspend-call nodes in the subtree under `e`.
fn count_suspensions(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> usize {
    let mut n = usize::from(is_suspension_point(ir, e, suspend_set));
    for_each_child(&ir.exprs, e, &mut |c| {
        n += count_suspensions(ir, c, suspend_set);
    });
    n
}

/// Collect every suspend-call expression id reachable under `e` (the final, post-transform body).
fn collect_suspension_points(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    out: &mut HashSet<ExprId>,
) {
    if is_suspension_point(ir, e, suspend_set) {
        out.insert(e);
    }
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        for &capture in captures {
            collect_suspension_points(ir, capture, suspend_set, out);
        }
        return;
    }
    for_each_child(&ir.exprs, e, &mut |c| {
        collect_suspension_points(ir, c, suspend_set, out)
    });
}

/// Every NAMED source variable (`IrExpr::Variable { named: true, .. }`) declared anywhere under `e`
/// — the set the suspend scope-spill rule applies to (compiler temps follow pure liveness).
/// Value-indices of `Variable` bindings whose init is a `When` with a suspension in a branch
/// VALUE — the flattener's `emit_cond` state-splits these (bind in a branch resume state, read in
/// the merge state), so they must be spilled regardless of the straight-line liveness rules.
fn collect_cond_susp_temp_bindings(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    out: &mut Vec<u32>,
) {
    let mut stack = vec![e];
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if let IrExpr::Variable {
            index,
            init: Some(init),
            ..
        } = ir.exprs[cur as usize]
        {
            if matches!(ir.exprs[init as usize], IrExpr::When { .. })
                && expr_calls_suspend(ir, init, suspend_set)
            {
                out.push(index);
            }
        }
        for_each_child(&ir.exprs, cur, &mut |c| stack.push(c));
    }
}

fn collect_slot_names(ir: &IrFile, e: ExprId, out: &mut std::collections::HashMap<u32, String>) {
    let mut stack = vec![e];
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if let IrExpr::Variable {
            index, named: true, ..
        } = ir.exprs[cur as usize]
        {
            if let Some(name) = super::debug_local_names::name(ir, cur) {
                out.entry(index).or_insert(name);
            }
        }
        for_each_child(&ir.exprs, cur, &mut |c| stack.push(c));
    }
}

fn collect_named_vars(ir: &IrFile, e: ExprId, out: &mut HashSet<u32>) {
    let mut stack = vec![e];
    let mut seen: HashSet<u32> = HashSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if let IrExpr::Variable {
            index, named: true, ..
        } = ir.exprs[cur as usize]
        {
            out.insert(index);
        }
        for_each_child(&ir.exprs, cur, &mut |c| stack.push(c));
    }
}

fn collect_live_writes(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>, out: &mut Vec<u32>) {
    match ir.exprs[e as usize].clone() {
        IrExpr::Variable { index, init, .. } => {
            out.push(index);
            if let Some(i) = init {
                collect_live_writes(ir, i, suspend_set, out);
            }
        }
        IrExpr::SetValue { var, value } => {
            out.push(var);
            collect_live_writes(ir, value, suspend_set, out);
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            collect_live_writes(ir, cond, suspend_set, out);
            if expr_calls_suspend(ir, body, suspend_set) {
                collect_live_writes(ir, body, suspend_set, out);
                if let Some(u) = update {
                    collect_live_writes(ir, u, suspend_set, out);
                }
            }
        }
        _ => crate::ir::for_each_child(&ir.exprs, e, &mut |c| {
            collect_live_writes(ir, c, suspend_set, out)
        }),
    }
}

/// The declared type of local `idx`, from its (first, pre-order) `Variable` declaration in `b`'s subtree.
fn find_local_ty(ir: &IrFile, b: ExprId, idx: u32) -> Option<Ty> {
    let mut found = None;
    visit_subtree(&ir.exprs, b, &mut |node| {
        if found.is_none() {
            if let IrExpr::Variable {
                index, ty, init, ..
            } = node
            {
                if *index == idx {
                    found = Some(local_storage_ty(ir, *ty, *init));
                }
            }
        }
    });
    found
}

/// Physical storage type of a common-IR local at the JVM suspend boundary. A shared mutable local
/// deliberately retains its Kotlin element type on `Variable`; the explicit `RefNew` initializer is
/// the semantic representation edge from which this backend derives its holder slot and spill field.
fn local_storage_ty(ir: &IrFile, semantic: Ty, init: Option<ExprId>) -> Ty {
    init.and_then(|initializer| match ir.exprs[initializer as usize] {
        IrExpr::RefNew { elem, .. } => Some(super::shared_captures::holder_ty(&elem)),
        _ => None,
    })
    .unwrap_or(semantic)
}

/// Synthesize the `Facade$fn$1 extends ContinuationImpl` continuation class: `result`/`label` fields, a
/// field per spilled local, a `<init>(Continuation)` delegating to super, and `invokeSuspend` (store the
/// resume value, set the `MIN_VALUE` label bit, re-enter the outer function).
/// A type-correct zero/`null` placeholder for `ty`, used as a value-parameter argument when
/// `invokeSuspend` re-enters the outer function — the real value is restored from the continuation
/// field at the loop top, so this placeholder is immediately overwritten (kotlinc passes `iconst_0`).
pub(crate) fn zero_value(ir: &mut IrFile, ty: &Ty) -> ExprId {
    let c = IrConst::zero_for_value_type(super::ir_emit::ir_ty_to_jvm(ty));
    ir.add_expr(IrExpr::Const(c))
}

fn add_static_call(
    ir: &mut IrFile,
    owner: &str,
    name: &str,
    descriptor: &str,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: type_name(owner),
            name: name.to_string(),
            descriptor: descriptor.to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args,
    })
}

fn coroutine_suspended(ir: &mut IrFile) -> ExprId {
    add_static_call(
        ir,
        "kotlin/coroutines/intrinsics/IntrinsicsKt",
        "getCOROUTINE_SUSPENDED",
        "()Ljava/lang/Object;",
        vec![],
    )
}

fn build_continuation_class(
    ir: &mut IrFile,
    internal: &str,
    outer_fid: u32,
    layout: &SpillLayout,
    _param_caps: &[(u32, Ty)],
    receiver: Option<TypeName>,
    params: &[Ty],
) -> ClassId {
    let class_id = ir.classes.len() as ClassId;
    let layout_fields = layout.fields();
    // result(0), label(1), spill slots(2..), and — for a member — the captured receiver `this$0` last.
    let recv_field_idx = 2 + layout_fields.len() as u32;

    // invokeSuspend(Object result): this.result = result; this.label |= MIN_VALUE; re-enter the outer
    // function. For a top-level fn that's `outer(this)`; for a member it's `this.this$0.m(this)`.
    let this0 = ir.add_expr(IrExpr::GetValue(0));
    let arg1 = ir.add_expr(IrExpr::GetValue(1));
    let set_result = ir.add_expr(IrExpr::SetField {
        receiver: this0,
        class: class_id,
        index: 0,
        value: arg1,
    });
    let this_lbl_recv = ir.add_expr(IrExpr::GetValue(0));
    let old_lbl = ir.add_expr(IrExpr::GetField {
        receiver: this_lbl_recv,
        class: class_id,
        index: 1,
    });
    let min = ir.add_expr(IrExpr::Const(IrConst::Int(I32_MIN)));
    let or_lbl = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::BitOr,
        lhs: old_lbl,
        rhs: min,
    });
    let this_set_lbl = ir.add_expr(IrExpr::GetValue(0));
    let set_label = ir.add_expr(IrExpr::SetField {
        receiver: this_set_lbl,
        class: class_id,
        index: 1,
        value: or_lbl,
    });
    let this_call = ir.add_expr(IrExpr::GetValue(0));
    let this_as_cont = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: this_call,
        type_operand: continuation_ty(),
    });
    // The outer fn now takes its real value parameters before the continuation. On re-entry the values
    // are irrelevant (the loop-top restore overwrites them from the captured fields), so pass type-
    // correct placeholders, exactly as kotlinc passes `iconst_0`/`aconst_null`.
    let mut reentry_args: Vec<ExprId> = params.iter().map(|t| zero_value(ir, t)).collect();
    reentry_args.push(this_as_cont);
    let call_outer = match receiver {
        None => ir.add_expr(IrExpr::Call {
            callee: Callee::Local(outer_fid),
            dispatch_receiver: None,
            args: reentry_args,
        }),
        Some(owner) => {
            let owner_internal = owner.render();
            // `((C)this.this$0).m(<params…>, (Continuation)this)` — invokevirtual the member on the receiver.
            let cont_this = ir.add_expr(IrExpr::GetValue(0));
            let recv = ir.add_expr(IrExpr::GetField {
                receiver: cont_this,
                class: class_id,
                index: recv_field_idx,
            });
            let name = ir.functions[outer_fid as usize].name.clone();
            // Build the member's CPS descriptor: its value params, then the trailing `Continuation`.
            let mut p_jvm: Vec<crate::types::Ty> =
                params.iter().map(super::ir_emit::ir_ty_to_jvm).collect();
            p_jvm.push(super::ir_emit::ir_ty_to_jvm(&continuation_ty()));
            let descriptor = crate::jvm::names::method_descriptor(
                &p_jvm,
                super::ir_emit::ir_ty_to_jvm(&object_ty()),
            );
            // A PRIVATE member can't be invoked from the continuation class (a separate class,
            // pre-nestmates). kotlinc emits a `PUBLIC|STATIC|FINAL|SYNTHETIC access$<name>` bridge on the
            // owner that `invokespecial`s the private member; the continuation calls the bridge.
            let owner_cid = ir.classes.iter().position(|c| c.fq_name == owner);
            let owner_midx = owner_cid
                .and_then(|cid| ir.classes[cid].methods.iter().position(|&m| m == outer_fid));
            if let (true, Some(cid), Some(midx)) = (
                ir.private_methods.contains(&outer_fid),
                owner_cid,
                owner_midx,
            ) {
                let access_name = format!("access${name}");
                // Bridge body (static frame): 0 = the owner receiver, 1..=n the value params,
                // n+1 the continuation — `return receiver.<private m>(args…, cont)` (the private
                // `MethodCall` emits as `invokespecial`).
                let recv0 = ir.add_expr(IrExpr::GetValue(0));
                let margs: Vec<Option<ExprId>> = (1..=params.len() + 1)
                    .map(|i| Some(ir.add_expr(IrExpr::GetValue(i as u32))))
                    .collect();
                let call = ir.add_expr(IrExpr::MethodCall {
                    class: cid as u32,
                    index: midx as u32,
                    receiver: recv0,
                    args: margs,
                });
                let aret = ir.add_expr(IrExpr::Return(Some(call)));
                let abody = ir.add_expr(IrExpr::Block {
                    stmts: vec![aret],
                    value: None,
                });
                let mut aparams = vec![Ty::obj_name(owner)];
                aparams.extend(params.iter().copied());
                aparams.push(continuation_ty());
                let afid = ir.add_fun(IrFunction {
                    name: access_name.clone(),
                    params: aparams.clone(),
                    ret: object_ty(),
                    body: Some(abody),
                    is_static: true,
                    dispatch_receiver: Some(owner),
                    param_checks: Vec::new(),
                });
                ir.classes[cid].methods.push(afid);
                ir.synthetic_methods.insert(afid); // kotlinc: 0x1019 PUBLIC|STATIC|FINAL|SYNTHETIC
                let a_jvm: Vec<crate::types::Ty> =
                    aparams.iter().map(super::ir_emit::ir_ty_to_jvm).collect();
                let adesc = crate::jvm::names::method_descriptor(
                    &a_jvm,
                    super::ir_emit::ir_ty_to_jvm(&object_ty()),
                );
                let mut aargs = vec![recv];
                aargs.extend(reentry_args);
                add_static_call(ir, &owner_internal, &access_name, &adesc, aargs)
            } else {
                // A suspend DEFAULT method lives on an interface: the re-entry call must be an
                // `invokeinterface` — an `invokevirtual` on an interface methodref fails linkage
                // with `IncompatibleClassChangeError` (coroutines/suspendDefaultImpl).
                let interface = owner_cid.is_some_and(|cid| ir.classes[cid].is_interface);
                ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner,
                        name,
                        descriptor,
                        params: None,
                        interface,
                    },
                    dispatch_receiver: Some(recv),
                    args: reentry_args,
                })
            }
        }
    };
    let ret = ir.add_expr(IrExpr::Return(Some(call_outer)));
    let inv_body = ir.add_expr(IrExpr::Block {
        stmts: vec![set_result, set_label, ret],
        value: None,
    });
    let inv_fid = ir.add_fun(IrFunction {
        name: "invokeSuspend".to_string(),
        params: vec![Ty::obj("kotlin/Any")],
        ret: object_ty(),
        body: Some(inv_body),
        is_static: false,
        dispatch_receiver: Some(type_name(internal)),
        param_checks: vec![None],
    });

    // State-machine fields: `result`/`label`/`L$i` are mutable and non-private (read/written
    // cross-class by the resume machinery).
    let mut fields = vec![
        crate::ir::IrField::new("result".to_string(), object_ty()).with_is_private(false),
        crate::ir::IrField::new("label".to_string(), int_ty()).with_is_private(false),
    ];
    for (name, field_ty) in &layout_fields {
        fields.push(crate::ir::IrField::new(name.clone(), *field_ty).with_is_private(false));
    }

    // Constructor value-indices: `this`=0, then (member) the receiver, then each captured value
    // parameter, then the completion `Continuation`. Store the receiver to `this$0` and each captured
    // param to its `L$i` field, then `super(completion)`. A top-level fn with no live params is just
    // `<init>(Continuation)`.
    let mut ctor_args: Vec<IrCtorArg> = Vec::new();
    let mut pre_super_param_fields = Vec::new();
    let mut arg_idx = 1u32; // value-index of the next ctor argument (`this` is 0)
    if let Some(owner) = receiver {
        let recv_ty = Ty::obj_name(owner);
        let receiver_field = fields.len() as u32;
        fields.push(
            crate::ir::IrField::new("this$0".to_string(), recv_ty.clone())
                .with_is_final(true)
                .with_is_private(false),
        );
        // The continuation ABI stores its member receiver before `ContinuationImpl.<init>`, but the
        // receiver field follows result/label/spills and is not a primary-constructor property. Carry
        // the exact parameter/field edge so emission needs neither a field-name rule nor a leading-field
        // assumption. This is independent of language-level inner/static nesting.
        pre_super_param_fields.push((0, receiver_field));
        ctor_args.push(IrCtorArg {
            name: None,
            ty: recv_ty,
            declared_ty: None,
            is_field: false,
            field_index: None,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
        });
        arg_idx += 1;
    }
    ctor_args.push(IrCtorArg {
        name: None,
        ty: continuation_ty(),
        declared_ty: None,
        is_field: false,
        field_index: None,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
    });
    let super_completion_idx = arg_idx;

    let super_arg = ir.add_expr(IrExpr::GetValue(super_completion_idx));
    let class = IrClass {
        fq_name: crate::types::type_name(internal),
        is_source_declared: false,
        is_anonymous_object: false,
        enclosing_function: None,
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_data: false,
        decl_line: 0,
        decl_start_line: 0,
        decl_end_line: 0,
        type_param_bounds: vec![],
        type_params: Vec::new(),
        captured_type_params: Vec::new(),
        supertypes: vec![],
        properties: Vec::new(),
        fields,
        ctor_param_count: 0,
        constructor_prefix_count: 0,
        ctor_args,
        ctor_param_annotations: Vec::new(),
        init_body: None,
        pre_super_param_fields,
        explicit_param_stores: false,
        methods: vec![inv_fid],
        is_interface: false,
        is_fun_interface: false,
        is_annotation: false,
        annotation_impl_of: None,
        is_sealed: false,
        sealed_subclasses: Default::default(),
        is_abstract: false,
        is_open: false,
        superclass: crate::types::type_name(CONTINUATION_IMPL),
        super_arg_prelude: Vec::new(),
        super_args: vec![super_arg],
        // The generated class delegates to `ContinuationImpl(Continuation)`. This field is now the
        // backend's exact selected-constructor contract, so it must describe the target parameter,
        // not the continuation class's `label: Int` storage field.
        super_ctor_params: vec![continuation_ty()],
        super_ctor_is_primary: true,
        enum_entries: vec![],
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: vec![],
        interfaces: Default::default(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        published_nested_classifiers: Vec::new(),
        secondary_ctors: vec![],
        has_primary_ctor: true,
        applied_annotations: crate::ir::DeclarationAnnotations::default(),
        primary_ctor_annotations: crate::ir::DeclarationAnnotations::default(),
        field_annotations: Vec::new(),
        property_annotations: Vec::new(),
        annotation_retention: None,
    };
    ir.add_class(class)
}

/// `kotlin.ResultKt.throwOnFailure(result)` — propagates a failed resume (a no-op on a normal value).
fn throw_on_failure(ir: &mut IrFile, result_v: u32) -> ExprId {
    let r = ir.add_expr(IrExpr::GetValue(result_v));
    add_static_call(
        ir,
        "kotlin/ResultKt",
        "throwOnFailure",
        "(Ljava/lang/Object;)V",
        vec![r],
    )
}

/// Wrap the state-dispatch `when` in `try { <dispatch> } catch (Throwable e) { when(this.label) {
/// <try-region states of handler H> -> { this.result = e; this.label = H } … else -> throw e } }`. An
/// exception thrown while executing a `try`-region state (synchronously OR as a failed resume —
/// `throwOnFailure` runs at each state entry, and `this.label` is a try-region state throughout) routes to
/// that try's handler state; one thrown while `this.label` is any other state re-propagates. The
/// exception is stashed in the `result` field (the handler state reads it back through `r_v`;
/// `throwOnFailure` is a no-op on a raw `Throwable`, which is not a `Result.Failure`). Using `this.label`
/// (an existing field) avoids any per-state flag local — so no slot collides with `emit_try`'s catch var.
/// Returns the dispatch unchanged when no state has a handler.
fn wrap_dispatch_for_handlers(
    ir: &mut IrFile,
    dispatch: ExprId,
    state_handlers: &[Option<usize>],
    catch_var: u32,
    cont_v: u32,
    cont_id: ClassId,
    field_base: u32,
) -> ExprId {
    // Group the try-region states by their handler state, preserving first-seen order.
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for (i, h) in state_handlers.iter().enumerate() {
        if let Some(h) = *h {
            match groups.iter_mut().find(|(gh, _)| *gh == h) {
                Some((_, v)) => v.push(i),
                None => groups.push((h, vec![i])),
            }
        }
    }
    if groups.is_empty() {
        return dispatch;
    }
    let mut branches: Branches = Vec::new();
    for (h, states) in &groups {
        // cond: `this.label == s0 || this.label == s1 || …`
        let mut cond: Option<ExprId> = None;
        for &s in states {
            let recv = ir.add_expr(IrExpr::GetValue(cont_v));
            let lbl = ir.add_expr(IrExpr::GetField {
                receiver: recv,
                class: cont_id,
                index: field_base + 1,
            });
            let sc = ir.add_expr(IrExpr::Const(IrConst::Int(s as i32)));
            let eq = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: lbl,
                rhs: sc,
            });
            cond = Some(match cond {
                None => eq,
                Some(c) => ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Or,
                    lhs: c,
                    rhs: eq,
                }),
            });
        }
        // route: `this.result = e; this.label = h`
        let this_res = ir.add_expr(IrExpr::GetValue(cont_v));
        let exc_v = ir.add_expr(IrExpr::GetValue(catch_var));
        let store_res = ir.add_expr(IrExpr::SetField {
            receiver: this_res,
            class: cont_id,
            index: field_base,
            value: exc_v,
        });
        let this_l = ir.add_expr(IrExpr::GetValue(cont_v));
        let hc = ir.add_expr(IrExpr::Const(IrConst::Int(*h as i32)));
        let set_lbl = ir.add_expr(IrExpr::SetField {
            receiver: this_l,
            class: cont_id,
            index: field_base + 1,
            value: hc,
        });
        let route = ir.add_expr(IrExpr::Block {
            stmts: vec![store_res, set_lbl],
            value: None,
        });
        branches.push((cond, route));
    }
    // else: re-throw the caught exception (it belongs to no active try region).
    let exc = ir.add_expr(IrExpr::GetValue(catch_var));
    let throw = ir.add_expr(IrExpr::Throw { operand: exc });
    let rethrow = ir.add_expr(IrExpr::Block {
        stmts: vec![throw],
        value: None,
    });
    branches.push((None, rethrow));
    let when = ir.add_expr(IrExpr::When { branches });
    let catch = crate::ir::IrCatch {
        var: catch_var,
        binding: None,
        exc_internal: crate::types::type_name("java/lang/Throwable"),
        body: when,
    };
    ir.add_expr(IrExpr::Try {
        body: dispatch,
        catches: vec![catch],
        finally: None,
        result: Ty::Unit,
    })
}

/// Coerce an `Object` value to `target` (unbox a primitive, or checkcast a reference).
fn unbox(ir: &mut IrFile, value: ExprId, target: &Ty) -> ExprId {
    // The CPS resume value is `Object`; a reference target (`Config`, `String`, `List<…>`) needs a real
    // `checkcast` to that type, while a primitive target unboxes. `ImplicitCoercion` unboxes but does not
    // narrow a reference, so a concrete reference result would otherwise stay `Object` (VerifyError at its
    // first typed use). `Cast` (a plain `checkcast`, null-passing) applies for a reference; `kotlin/Any`
    // needs neither (already `Object`).
    let op = if reference_needs_checkcast(target) {
        IrTypeOp::Cast
    } else {
        IrTypeOp::ImplicitCoercion
    };
    ir.add_expr(IrExpr::TypeOp {
        op,
        arg: value,
        type_operand: target.clone(),
    })
}

/// Whether narrowing an erased `Object` resume value to `t` needs an explicit `checkcast` — a concrete
/// reference class (`Config`), `String`, or an array. `kotlin/Any` (already `Object`) and primitives do
/// NOT; crucially a BOXED-primitive object type (`Obj("kotlin/Int")`, a spilled `Int`) also does not —
/// there `ImplicitCoercion` UNBOXES to the primitive, whereas a `checkcast` would leave it boxed and a
/// later primitive use (`istore`/`iadd`) would fail verification.
fn reference_needs_checkcast(t: &Ty) -> bool {
    match *t {
        // A NULLABLE PRIMITIVE (`Int?`) is a boxed reference (`Integer`): the resume value must be
        // checkcast to the wrapper — `ImplicitCoercion` would no-op (it can't unbox to a nullable),
        // leaving the slot `Object` while the spill restore's frame type is the wrapper (VerifyError
        // at the state merge). A nullable REFERENCE keeps the inner type's own answer.
        Ty::Nullable(inner) => t.nullable_primitive().is_some() || reference_needs_checkcast(inner),
        Ty::TyParam(_, inner) => reference_needs_checkcast(inner),
        Ty::String => true,
        Ty::Obj(i, _) => !i.matches("kotlin/Any") && !is_boxed_primitive_internal(&i.render()),
        _ => false,
    }
}

/// A boxed-primitive object internal name (`kotlin/Int`, `java/lang/Integer`, …) — one whose
/// `ImplicitCoercion` unboxes to a JVM primitive rather than acting as a reference.
fn is_boxed_primitive_internal(internal: &str) -> bool {
    matches!(
        internal,
        "kotlin/Int"
            | "kotlin/Long"
            | "kotlin/Short"
            | "kotlin/Byte"
            | "kotlin/Char"
            | "kotlin/Boolean"
            | "kotlin/Float"
            | "kotlin/Double"
            | "java/lang/Integer"
            | "java/lang/Long"
            | "java/lang/Short"
            | "java/lang/Byte"
            | "java/lang/Character"
            | "java/lang/Boolean"
            | "java/lang/Float"
            | "java/lang/Double"
    )
}

/// Wrap the value of every `Return` reachable from `e` in an `ImplicitCoercion` to `Object`.
/// Ensure a leaf suspend fn's body ends with a `return` (its CPS method returns `Object`; without this a
/// fall-through body verifies as "control flow falls through code end"). Idempotent: a body already
/// ending in `return`/`throw` is left alone. A trailing VALUE becomes `return box(value)`; a statement
/// body / a `Unit` fn runs the body for effect and returns `Unit.INSTANCE`.
fn ensure_tail_return(ir: &mut IrFile, body: ExprId, unit_ret: bool) {
    let IrExpr::Block { stmts, value } = ir.exprs[body as usize].clone() else {
        return;
    };
    let mut stmts = stmts;
    match value {
        Some(v) if !unit_ret => {
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: v,
                type_operand: object_ty(),
            });
            stmts.push(ir.add_expr(IrExpr::Return(Some(boxed))));
        }
        Some(v) => {
            // `Unit` fn: run the trailing value for effect, then return the `Unit` singleton.
            stmts.push(v);
            let unit = ir.add_expr(IrExpr::UnitInstance);
            stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
        }
        None => {
            // Statement body. If it doesn't already terminate, return `Unit.INSTANCE` (a leaf suspend fn
            // with a `Unit`/no-value body).
            let terminates = stmts.last().is_some_and(|&s| stmt_diverges(ir, s));
            if !terminates {
                let unit = ir.add_expr(IrExpr::UnitInstance);
                stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
            }
        }
    }
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
}

/// The continuation-field type for a spilled local. A `Unit`-typed local spills as the `kotlin/Unit`
/// object reference — a JVM field cannot carry the `void` ("V") descriptor that `Ty::Unit` produces, and
/// the live value across the suspension is the `Unit` singleton.
/// The per-kind spill field letter (kotlinc: references `L$`, ints `I$`, longs `J$`, …).
fn spill_kind(ty: &Ty) -> char {
    if ty.is_reference() {
        'L'
    } else {
        match *ty {
            Ty::Long => 'J',
            Ty::Float => 'F',
            Ty::Double => 'D',
            Ty::Boolean => 'Z',
            Ty::Char => 'C',
            Ty::Byte => 'B',
            Ty::Short => 'S',
            _ => 'I',
        }
    }
}

/// The spill kind of a reference local. `@DebugMetadata`'s `n`/`s` arrays list these first and keep
/// every other spill in the order it was spilled; field layout instead groups by kind, in the
/// first-spill order recorded by [`SpillLayout`].
const REFERENCE_SPILL_KIND: char = 'L';

/// Annotate each scope-list entry with its kind and position WITHIN that kind (kotlinc's
/// per-suspension positional slot).
/// A local of the BOTTOM type (`var x = null` — `Ty::Null`) has exactly ONE possible value, so kotlinc
/// gives it no continuation field and REMATERIALIZES it (`aconst_null; astore`) in every resume arm.
/// Keeping it out of the spill layout is also what keeps its verification type `null` — assignable to
/// any reference — where restoring it from an `Object`-typed field would widen the slot and break the
/// next typed use of it (`bar(x: String?, …)` → "Bad type on operand stack").
fn is_rematerialized_null(ty: &Ty) -> bool {
    matches!(ty.non_null(), Ty::Null)
}

/// The entries of a suspension's scope list that a resume arm rematerializes rather than reloads.
fn rematerialized_nulls(list: &[(u32, Ty)]) -> Vec<u32> {
    list.iter()
        .filter(|(_, t)| is_rematerialized_null(t))
        .map(|&(l, _)| l)
        .collect()
}

fn kind_positions(list: &[(u32, Ty)]) -> Vec<(u32, Ty, char, u32)> {
    let mut counts: std::collections::HashMap<char, u32> = std::collections::HashMap::new();
    list.iter()
        .filter(|(_, ty)| !is_rematerialized_null(ty))
        .map(|&(l, ty)| {
            let k = spill_kind(&ty);
            let c = counts.entry(k).or_insert(0);
            let pos = *c;
            *c += 1;
            (l, ty, k, pos)
        })
        .collect()
}

fn spill_field_ty(ty: Ty) -> Ty {
    if ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        ty
    }
}

/// A spilled-local shape the state machine's uniform restore doesn't model yet: kotlinc's per-kind
/// positional spilling coerces INT-LIKE sub-int locals (`I$N` restored with `i2b`/`i2s`/`i2c`),
/// restores an array-typed local with its exact frame type, and null-checks a NULLABLE reference
/// restore; krusty's restore emits none of these, so such a machine would fail verification (or
/// corrupt a frame type) — bail (skip, never miscompile).
/// Whether any SUSPENSION under `b` dispatches through `invokespecial` — a `super.f()` whose callee is
/// a `suspend` function. See the bail site for why the machine cannot resume into one.
fn suspends_through_super(ir: &IrFile, b: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if matches!(
        &ir.exprs[b as usize],
        IrExpr::Call {
            callee: crate::ir::Callee::Special { .. },
            ..
        }
    ) && is_suspension_point(ir, b, suspend_set)
    {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, b, &mut |c| {
        if suspends_through_super(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

fn spill_shape_unmodeled(spilled: &[(u32, Ty)]) -> bool {
    spilled.iter().any(|(_, t)| {
        // `Boolean` is excluded: it is the one narrow kind the restore does handle, and it occurs in
        // every `runBlocking { … }` that keeps a flag across a suspension.
        matches!(*t, Ty::Byte | Ty::Short | Ty::Char) || t.non_null().array_elem().is_some()
    })
}

/// A spilled local of type `Nothing`: an expression of the bottom type never yields a value, so the
/// slot has no JVM type at all and merges to `top` at a control-flow join, where the spill's `aload`
/// fails verification ("Bad local variable type"). (`Ty::Null` — the always-`null` `var x = null` — is
/// handled instead of gated; see [`is_rematerialized_null`].) Bail (skip, never miscompile).
fn spills_bottom_typed_local(spilled: &[(u32, Ty)]) -> bool {
    spilled
        .iter()
        .any(|(_, t)| matches!(t.non_null(), Ty::Nothing))
}

/// Whether the statement is (or contains) a loop — the loop-carried spill rule's trigger.
fn stmt_contains_loop(ir: &IrFile, e: ExprId) -> bool {
    if matches!(&ir.exprs[e as usize], IrExpr::While { .. }) {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        found = found || stmt_contains_loop(ir, c);
    });
    found
}

/// A body whose LAST statement is (or wraps) a SUSPENDING loop with nothing after it (`builder {
/// for (…) { susp() } }`): the machine's fall-through return after the loop's exit state isn't
/// modeled (the completion resumes with `null` instead of `Unit`). A trailing statement after the
/// loop (the common corpus shape) is fine. Bail (skip, never miscompile).
fn tail_suspending_loop(ir: &IrFile, stmts: &[ExprId], suspend_set: &HashSet<u32>) -> bool {
    fn wraps_loop(ir: &IrFile, e: ExprId) -> bool {
        match &ir.exprs[e as usize] {
            IrExpr::While { .. } => true,
            // A block tails in its VALUE when present, but a materialized `Unit` value sits after
            // the real trailing loop STATEMENT — check both.
            IrExpr::Block { stmts, value } => {
                value.is_some_and(|v| wraps_loop(ir, v))
                    || stmts.last().is_some_and(|&s| wraps_loop(ir, s))
            }
            // A statement-position loop materialized into a `Unit` temp binding.
            IrExpr::Variable {
                ty: Ty::Unit,
                init: Some(i),
                ..
            } => wraps_loop(ir, *i),
            _ => false,
        }
    }
    // The lambda lowering appends a synthesized `return Unit` after the body — look through it to
    // the last REAL statement.
    let mut last = stmts;
    while let [head @ .., tail] = last {
        let epilogue = match &ir.exprs[*tail as usize] {
            IrExpr::Return(_) => true,
            // The synthesized `Unit` materialization the lambda lowering appends before its return.
            IrExpr::Variable {
                named: false,
                init: Some(i),
                ..
            } => matches!(&ir.exprs[*i as usize], IrExpr::UnitInstance),
            _ => false,
        };
        if epilogue && !head.is_empty() {
            last = head;
        } else {
            break;
        }
    }
    let hit = last
        .last()
        .is_some_and(|&s| wraps_loop(ir, s) && expr_calls_suspend(ir, s, suspend_set));
    if let Some(&s) = last.last() {
        crate::trace_compiler!(
            "suspend",
            "tail_suspending_loop: hit={hit} last={:?} wraps={} susp={}",
            &ir.exprs[s as usize],
            wraps_loop(ir, s),
            expr_calls_suspend(ir, s, suspend_set)
        );
        if let IrExpr::Variable { init: Some(i), .. } = &ir.exprs[s as usize] {
            crate::trace_compiler!("suspend", "tail init = {:?}", &ir.exprs[*i as usize]);
        }
    }
    hit
}

/// The receiver/arguments of a suspension point, in EVALUATION order. Both call shapes suspend: `Call`
/// is a static/top-level callee (its `dispatch_receiver` is the operand-stack receiver of a virtual /
/// `super` callee), `MethodCall` a same-file member (`c.foo(i++)`), whose receiver is just as much an
/// operand as its arguments. Empty for an intrinsic suspension point (an inlined block, not a call).
fn suspension_operand_ids(ir: &IrFile, point: ExprId) -> Vec<ExprId> {
    match &ir.exprs[point as usize] {
        IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } => dispatch_receiver.iter().chain(args).copied().collect(),
        IrExpr::MethodCall { receiver, args, .. } => std::iter::once(*receiver)
            .chain(args.iter().flatten().copied())
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `e`'s subtree assigns any local in `list` — the writes whose effect the spill stores must
/// observe. Both an assignment and a re-declaration at the same value index count.
fn writes_local_in(ir: &IrFile, e: ExprId, list: &[(u32, Ty)]) -> bool {
    let idx = match ir.exprs[e as usize] {
        IrExpr::SetValue { var, .. } => Some(var),
        IrExpr::Variable { index, .. } => Some(index),
        _ => None,
    };
    if idx.is_some_and(|i| list.iter().any(|&(l, _)| l == i)) {
        return true;
    }
    // Constructing a lambda evaluates only its captures. Its retained inline body and standalone
    // implementation have an independent value namespace and execute later; treating their writes
    // as suspension-operand effects falsely requires the lambda object itself to be rebound to a
    // temp (a shape intentionally rejected because inline emission consumes the lambda node).
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        return captures
            .iter()
            .any(|&capture| writes_local_in(ir, capture, list));
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        found = found || writes_local_in(ir, c, list);
    });
    found
}

/// [`suspension_operand_ids`] with each operand paired to the DECLARED type of the parameter (or
/// receiver) it feeds — the type a pre-spill temp must carry, so the temp's store/load is the same JVM
/// kind the call consumes. `None` for a shape whose operands can't be typed or safely re-bound: an
/// intrinsic callee, a `Callee::Static` that is `inline` (SPLICED from its operand nodes rather than
/// called with them) or carries a `dispatch_receiver` (which the non-splice emit path does not even
/// push), a `MethodCall` with omitted arguments, or a `Lambda`/`Vararg` operand.
///
/// A `Callee::Virtual` may also be inline-spliced at emit (its descriptor form has an inline branch) and
/// is deliberately NOT refused for it: splicing reads its operand nodes as values, and a `GetValue` of a
/// temp is one.
fn typed_suspension_operands(ir: &IrFile, point: ExprId) -> Option<Vec<(ExprId, Ty)>> {
    // Parameters are indexed before the continuation value is added. An ordinary suspend call's
    // continuation is the trailing surplus parameter. A `$default` call removes its descriptor-owned
    // continuation slot before this zip, leaving real arguments, masks, and marker aligned exactly.
    fn zip(args: &[ExprId], params: &[Ty], out: &mut Vec<(ExprId, Ty)>) -> Option<()> {
        for (i, &a) in args.iter().enumerate() {
            out.push((a, *params.get(i)?));
        }
        Some(())
    }
    let mut out: Vec<(ExprId, Ty)> = Vec::new();
    match &ir.exprs[point as usize] {
        IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } => {
            let params: Vec<Ty> = match callee {
                Callee::Local(fid) => {
                    dispatch_receiver.is_none().then_some(())?;
                    ir.functions[*fid as usize].params.clone()
                }
                Callee::ClassStatic { function, .. } => {
                    dispatch_receiver.is_none().then_some(())?;
                    ir.functions[*function as usize].params.clone()
                }
                Callee::CrossFile { params, .. } => {
                    dispatch_receiver.is_none().then_some(())?;
                    params.clone()
                }
                Callee::Module { params, .. } => {
                    dispatch_receiver.is_none().then_some(())?;
                    params.clone()
                }
                Callee::External { params, .. } => {
                    dispatch_receiver.is_none().then_some(())?;
                    params.clone()
                }
                Callee::Static {
                    descriptor,
                    name,
                    inline,
                    ..
                } => {
                    (!inline.can_inline() && dispatch_receiver.is_none()).then_some(())?;
                    let mut params = crate::jvm::ir_emit::parse_physical_method_desc(descriptor)?.0;
                    if name.ends_with("$default") {
                        let continuation = default_suspend_continuation_index(&params)?;
                        params.remove(continuation);
                    }
                    params
                }
                Callee::Virtual {
                    owner,
                    descriptor,
                    params,
                    ..
                } => {
                    out.push((
                        dispatch_receiver.as_ref().copied()?,
                        Ty::obj(&owner.render()),
                    ));
                    match params {
                        Some((p, _)) => p.clone(),
                        None => crate::jvm::ir_emit::parse_physical_method_desc(descriptor)?.0,
                    }
                }
                // Realized into `Callee::Special` before this pass runs.
                Callee::Super { .. } => return None,
                Callee::Special {
                    owner,
                    name,
                    descriptor,
                    ..
                } => {
                    // Same `$default` indexing rule as `Callee::Static` above.
                    (!name.ends_with("$default")).then_some(())?;
                    out.push((
                        dispatch_receiver.as_ref().copied()?,
                        Ty::obj(&owner.render()),
                    ));
                    crate::jvm::ir_emit::parse_physical_method_desc(descriptor)?.0
                }
                Callee::Intrinsic { .. }
                | Callee::LocalWithDefaults { .. }
                | Callee::LocalDefault(_)
                | Callee::ClassStaticWithDefaults { .. }
                | Callee::ClassStaticDefault { .. } => return None,
                Callee::ModuleWithDefaults { .. } => return None,
            };
            zip(args, &params, &mut out)?;
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => {
            let c = &ir.classes[*class as usize];
            let f = &ir.functions[*c.methods.get(*index as usize)? as usize];
            out.push((*receiver, Ty::obj(&c.fq_name())));
            let args: Vec<ExprId> = args.iter().copied().collect::<Option<Vec<_>>>()?;
            zip(&args, &f.params, &mut out)?;
        }
        _ => return None,
    }
    out.iter()
        .all(|&(o, _)| {
            !matches!(
                ir.exprs[o as usize],
                IrExpr::Lambda { .. } | IrExpr::Vararg { .. }
            )
        })
        .then_some(out)
}

fn box_returns(ir: &mut IrFile, e: ExprId) -> bool {
    match ir.exprs[e as usize].clone() {
        IrExpr::Return(None) => {
            // The CPS method returns `Object`, so a BARE `return` — a `Unit`-returning suspend fn's early
            // exit (`x ?: return`, `if (…) return`) — must `areturn Unit.INSTANCE`, not a void `return`
            // (which fails verification: "Method expects a return value"). Every other return in the
            // assembled state machine already yields a value.
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.exprs[e as usize] = IrExpr::Return(Some(unit));
            true
        }
        IrExpr::Return(Some(v)) => {
            // Already an Object-yielding suspension return (COROUTINE_SUSPENDED) needs no box; but a
            // double coercion to Object is harmless (identity on a reference), so box uniformly.
            let boxed = ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: v,
                type_operand: object_ty(),
            });
            ir.exprs[e as usize] = IrExpr::Return(Some(boxed));
            box_returns(ir, v)
        }
        // A lambda argument (`m.map { it.value }`) is a VALUE whose impl function is a separate body,
        // but the canonical child walk exposes only what this function owns: its captures, which are
        // evaluated in this frame, and its retained `inline_body`. A `Return` that survives in an
        // inline body is a NON-LOCAL return by construction — the template preparation already turned
        // every local `return@label` into a labelled exit — and the classpath-inline splice realizes
        // it as a return from THIS method. It must box exactly like a return written in the body
        // proper: `twice(1) { x -> if (x == 2) return 100; x }` in a CPS body otherwise leaves
        // `bipush 100; areturn` (VerifyError: "Bad type on operand stack"), and a bare `return` a void
        // `return` where the `Object` result is expected. So a lambda is NOT a leaf here.
        //
        // The invariant covers returns that leave the template being spliced. A `return@outer` in a
        // lambda nested inside another emit-time-spliced lambda is prepared only by the INNER
        // template (`reachable_checked_returns` stops at a nested lambda), survives as a raw `Return`,
        // and the emitter realizes it as this method's — a pre-existing gap; boxing it here keeps
        // that shape at least loadable and is not what makes it wrong.
        //
        // Return boxing is a tree rewrite, not an IR-shape validator. Use the canonical child relation
        // so adding an unrelated expression kind cannot make an otherwise valid suspend function
        // unsupported. Unsupported coroutine control-flow is rejected by the state-machine flattener,
        // where that decision belongs.
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, e, &mut |child| children.push(child));
            children.into_iter().all(|child| box_returns(ir, child))
        }
    }
}

/// Apply `f` to the node at `e` and every node in its subtree (pre-order). Children are snapshotted
/// before recursing, so `f` may freely mutate the current node (the mutable borrow of `ir.exprs[e]` is
/// released before the child walk). The single home for every in-place IR subtree rewrite in this pass.
fn rewrite_subtree(ir: &mut IrFile, e: ExprId, f: &mut impl FnMut(&mut IrExpr)) {
    f(&mut ir.exprs[e as usize]);
    let mut kids = Vec::new();
    for_each_child(&ir.exprs, e, &mut |c| kids.push(c));
    for c in kids {
        rewrite_subtree(ir, c, f);
    }
}

/// Collect the node ids of every `GetValue(var)` in `e`'s subtree (a catch body) that means the catch
/// variable, so each can be rewritten to read the exception from `r_v` instead of a catch-variable local.
/// A `Lambda` has its OWN value numbering: its CAPTURES read the enclosing scope (so a captured catch var
/// is collected), but its `inline_body`'s locals are numbered independently — do NOT descend into it, or a
/// lambda-local that happens to reuse `var`'s index would be wrongly rewritten (mirrors `shift_value_indices`).
fn collect_getvalue(ir: &IrFile, e: ExprId, var: u32, out: &mut Vec<ExprId>) {
    match &ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i == var => {
            out.push(e);
            return;
        }
        IrExpr::Lambda { captures, .. } => {
            let caps = captures.clone();
            for c in caps {
                collect_getvalue(ir, c, var, out);
            }
            return;
        }
        _ => {}
    }
    for_each_child(&ir.exprs, e, &mut |c| collect_getvalue(ir, c, var, out));
}

/// Collect every suspending catch body so [`build_state_machine`] can spill its caught exception across
/// that body's suspension (`r_v` no longer holds it once the catch resumes). Catch BODY identity—not
/// the lexically reused catch-variable slot—distinguishes sibling arms. Does not descend into lambdas,
/// whose bodies own separate state machines and value namespaces.
fn find_suspending_catch_tries(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    out: &mut Vec<(u32, ExprId, crate::types::TypeName)>,
) {
    match &ir.exprs[e as usize] {
        IrExpr::Lambda { .. } => return,
        IrExpr::Try {
            catches, finally, ..
        } if finally.is_none() => {
            for catch in catches {
                if expr_calls_suspend(ir, catch.body, suspend_set)
                    && !catch_body_nests_suspending_catch(ir, catch.body, suspend_set)
                {
                    out.push((catch.var, catch.body, catch.exc_internal));
                }
            }
        }
        _ => {}
    }
    for_each_child(&ir.exprs, e, &mut |c| {
        find_suspending_catch_tries(ir, c, suspend_set, out)
    });
}

/// Whether `e` (a catch body) is, or contains (excluding `Lambda` bodies), a `try/catch` whose own catch
/// body suspends — a nested suspending catch whose exception variable could alias the enclosing one.
fn catch_body_nests_suspending_catch(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::Lambda { .. } => return false,
        IrExpr::Try { catches, .. }
            if catches
                .iter()
                .any(|c| expr_calls_suspend(ir, c.body, suspend_set)) =>
        {
            return true;
        }
        _ => {}
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if !found && catch_body_nests_suspending_catch(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

/// Apply `f` to the node at `e` and every node in its subtree (pre-order), read-only. The single home
/// for every read traversal (collect / find) in this pass.
fn visit_subtree(exprs: &[IrExpr], e: ExprId, f: &mut impl FnMut(&IrExpr)) {
    f(&exprs[e as usize]);
    for_each_child(exprs, e, &mut |c| visit_subtree(exprs, c, f));
}

/// Increment every value-index `>= threshold` in `e`'s subtree (a `GetValue`/`SetValue` read-write or a
/// `Variable` declaration). Used to make room at index `threshold` for the CPS continuation parameter
/// without aliasing a body local. `GetStatic` holds a static-field index (a different namespace) and is
/// left untouched.
fn shift_locals(ir: &mut IrFile, e: ExprId, threshold: u32) {
    // Delegate to the shared index-shifter, which correctly treats a nested `Lambda` as a separate
    // value-index scope: it shifts the lambda's CAPTURES (enclosing-frame reads) but NOT its body/params
    // (numbered independently). The previous `rewrite_subtree` here descended into the lambda body too —
    // for a TOP-LEVEL suspend fn (threshold 0) that shifted a `filter { it > 0 }` predicate's own `it`
    // from 0 to 1, leaving `GetValue(1)` unallocated in the extracted lambda method (a class method escaped
    // because its lambda `it`=0 was below the threshold 1).
    crate::ir::shift_value_indices(ir, e, threshold, 1);
}

/// Resolve every current-coroutine placeholder in `e` against the continuation value at `slot` (the
/// trailing `Continuation` parameter's value-index). `CurrentContinuation` becomes a direct local
/// read; the checked `coroutineContext` intrinsic becomes the ordinary interface call on that same
/// value. Neither realization repeats property lookup or invokes the stdlib's private throwing getter.
fn rewrite_current_continuation(ir: &mut IrFile, e: ExprId, slot: u32) {
    let mut reads_context = false;
    visit_subtree(&ir.exprs, e, &mut |node| {
        reads_context |= matches!(
            node,
            IrExpr::Call {
                callee: Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::CoroutineContext,
                    ..
                },
                ..
            }
        );
    });
    let context_receiver = reads_context.then(|| ir.add_expr(IrExpr::GetValue(slot)));
    rewrite_subtree(ir, e, &mut |node| {
        if matches!(node, IrExpr::CurrentContinuation) {
            *node = IrExpr::GetValue(slot);
            return;
        }
        if matches!(
            node,
            IrExpr::Call {
                callee: Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::CoroutineContext,
                    ..
                },
                ..
            }
        ) {
            let IrExpr::Call {
                dispatch_receiver,
                args,
                ..
            } = node
            else {
                unreachable!("matched coroutine-context call")
            };
            debug_assert!(dispatch_receiver.is_none());
            debug_assert!(args.is_empty());
            *node = IrExpr::Call {
                callee: Callee::Virtual {
                    owner: type_name("kotlin/coroutines/Continuation"),
                    name: "getContext".to_string(),
                    descriptor: "()Lkotlin/coroutines/CoroutineContext;".to_string(),
                    params: None,
                    interface: true,
                },
                dispatch_receiver: context_receiver,
                args: Vec::new(),
            };
        }
    });
}

/// The maximum value-index referenced anywhere in the arena (params, locals). New state-machine locals
/// are allocated above this so they never collide with an existing index in any function.
fn max_value_index(ir: &IrFile) -> u32 {
    let mut m = 0u32;
    for e in &ir.exprs {
        match e {
            IrExpr::GetValue(i) | IrExpr::GetStatic(i) => m = m.max(*i),
            IrExpr::SetValue { var, .. } => m = m.max(*var),
            IrExpr::Variable { index, .. } => m = m.max(*index),
            _ => {}
        }
    }
    m
}
