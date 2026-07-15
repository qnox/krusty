//! JVM coroutine (`suspend fun`) IR lowering pass — an **optional, JVM-only** IR→IR transform.
//!
//! `ir_lower` keeps a `suspend fun` as a plain function (its declared Kotlin signature) and records its
//! `FunId` in `ir.suspend_funs`, so the platform-agnostic IR stays neutral (a JS backend realizes
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

use crate::ir::{
    for_each_child, Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrCtorArg, IrExpr, IrFile,
    IrFunction, IrTypeOp,
};
use crate::libraries::InlineKind;
use crate::types::Ty;
use std::collections::HashSet;

const I32_MIN: i32 = i32::MIN;
/// `when` branches: each `(condition, body)` (an `else` branch has `condition = None`).
type Branches = Vec<(Option<ExprId>, ExprId)>;
/// A direct suspension at a statement: `(optional bound local + type, the call ExprId)`. The call (a
/// `Call` or `MethodCall`) is reused — the continuation is threaded into it by `emit_call`.
type Suspension = (Option<(u32, Ty)>, ExprId);
const CONTINUATION: &str = "kotlin/coroutines/Continuation";
const CONTINUATION_IMPL: &str = "kotlin/coroutines/jvm/internal/ContinuationImpl";

fn object_ty() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}
fn int_ty() -> Ty {
    Ty::obj("kotlin/Int")
}
fn continuation_ty() -> Ty {
    Ty::obj(CONTINUATION)
}

/// Rewrite every `suspend fun` in `ir` to the JVM CPS ABI. `facade` is the file's facade class internal
/// name (e.g. `SKt`) — the continuation class for `bar` is `SKt$bar$1`. Returns `false` (skip the whole
/// file, never miscompile) on any suspend shape this pass can't yet transform.
#[must_use]
pub fn lower_suspend(ir: &mut IrFile, facade: &str) -> bool {
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    // Snapshot every function's *declared* (pre-CPS) return type, so hoisted suspension temps are typed
    // by the callee's logical result type even after the callee has itself been CPS-rewritten to `Object`.
    let orig_rets: Vec<Ty> = ir.functions.iter().map(|f| f.ret.clone()).collect();
    let fids = ir.suspend_funs.clone();
    crate::trace_compiler!(
        "suspend",
        "lower_suspend facade={facade} suspend_funs={fids:?} suspend_lambda_sm={}",
        ir.suspend_lambda_sm.len()
    );
    for fid in fids {
        let body = ir.functions[fid as usize].body;
        // Normalize `return { stmts…; value }` into `stmts…; return value`. An elvis / safe-call subject
        // that suspends lowers to a value-position `Block` binding a temp (`{ val t = susp()…; when{…} }`);
        // hoisting can't see into a value block, so the suspension would hide there and the flattener bail.
        // Splicing lifts the block's statements to the top level where the hoister/flattener handle them.
        if let Some(b) = body {
            splice_return_blocks(ir, b);
        }
        // Hoist a suspension nested at an unconditional position in an expression (`foo() + 2`) into a
        // preceding `val tmp = foo()` temp, so the flattener only meets suspensions at handled positions.
        if let Some(b) = body {
            hoist_suspensions(ir, b, &suspend_set, &orig_rets);
        }
        // Desugar `return <suspend call>` (incl. an `= <suspend call>` expression body) into
        // `val tmp = <suspend call>; return tmp` so a tail-position suspension becomes a uniform
        // bound-local point. Uses the function's (pre-CPS) declared return type for `tmp`.
        let ret_ty = ir.functions[fid as usize].ret.clone();
        if let Some(b) = body {
            desugar_value_try(ir, b, &suspend_set, &ret_ty);
            desugar_value_when(ir, b, &suspend_set, &ret_ty);
            desugar_tail_suspend(ir, b, &suspend_set, &ret_ty);
        }
        let has_susp = body.is_some_and(|b| expr_calls_suspend(ir, b, &suspend_set));
        crate::trace_compiler!(
            "suspend",
            "fn fid={fid} name={} has_susp={has_susp}",
            ir.functions[fid as usize].name
        );
        let is_static = ir.functions[fid as usize].is_static;
        // CPS signature: append the continuation parameter, erase the return to Object.
        let f = &mut ir.functions[fid as usize];
        f.params.push(continuation_ty());
        f.param_checks.push(None);
        f.ret = object_ty();

        // The continuation parameter's value-index is `params + (this ? 1 : 0)`; ir_lower numbered body
        // locals from that same index, so shift every body local up by one to make room for it.
        let p_old =
            ir.functions[fid as usize].params.len() as u32 - 1 + if is_static { 0 } else { 1 };
        if let Some(b) = body {
            shift_locals(ir, b, p_old);
            // `suspendCoroutineUninterceptedOrReturn { c -> … }` bound `c` to a `CurrentContinuation`
            // placeholder; now that the trailing `Continuation` parameter exists at value-index `p_old`,
            // resolve the placeholder to read it.
            rewrite_current_continuation(ir, b, p_old);
        }

        if !has_susp {
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
            if !build_state_machine(ir, facade, fid, body.unwrap(), unit_ret) {
                return false;
            }
        }
    }
    // Suspend LAMBDAS with multiple suspensions / control flow: their `invokeSuspend` is a state machine
    // whose continuation is the lambda instance itself (ir_lower handled the single-suspension shapes).
    for (fid, class_id, field_base) in ir.suspend_lambda_sm.clone() {
        if !build_lambda_state_machine(ir, fid, class_id, field_base, &orig_rets) {
            return false;
        }
    }
    true
}

/// Lift a value-position `Block` out of a top-level statement's direct operand, so a suspension buried in
/// the block's statements surfaces at the top level where the hoister/flattener handle it. An elvis /
/// safe-call whose subject suspends lowers to `{ val t = susp()…; when{…} }` in `return`/`val =`/assign
/// position — a block the hoister can't see into, so the flattener would bail. This rewrites:
///   `return { s…; v }`      → `s…; return v`
///   `val x = { s…; v }`     → `s…; val x = v`
///   `x = { s…; v }`         → `s…; x = v`
/// Only a value-bearing block is spliced (a value-less / divergent block is left alone). Re-runs until
/// settled, so nested blocks (safe-call inside elvis) fully unfold; lifted statements are reprocessed.
fn splice_return_blocks(ir: &mut IrFile, b: ExprId) {
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
        let spliced = match ir.exprs[s as usize].clone() {
            IrExpr::Return(Some(inner)) => value_block(ir, inner).map(|(bs, bv)| {
                new_stmts.extend(bs);
                ir.add_expr(IrExpr::Return(Some(bv)))
            }),
            IrExpr::Variable {
                index,
                ty,
                init: Some(inner),
            } => value_block(ir, inner).map(|(bs, bv)| {
                new_stmts.extend(bs);
                ir.add_expr(IrExpr::Variable {
                    index,
                    ty,
                    init: Some(bv),
                })
            }),
            IrExpr::SetValue { var, value: inner } => value_block(ir, inner).map(|(bs, bv)| {
                new_stmts.extend(bs);
                ir.add_expr(IrExpr::SetValue { var, value: bv })
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

/// If `e` is a value-bearing `Block`, return `(its statements, its value)`; else `None`.
fn value_block(ir: &IrFile, e: ExprId) -> Option<(Vec<ExprId>, ExprId)> {
    match &ir.exprs[e as usize] {
        IrExpr::Block {
            stmts,
            value: Some(v),
        } => Some((stmts.clone(), *v)),
        _ => None,
    }
}

/// Rewrite each top-level `return <suspend call>` in `b` into `val tmp = <suspend call>; return tmp`
/// (a fresh local typed `ret_ty`), so a tail-position suspension is handled as an ordinary bound-local
/// suspension point. Runs before the CPS rewrite, so `ret_ty` is the function's declared return type.
fn desugar_tail_suspend(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts = Vec::with_capacity(stmts.len() + 1);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if is_suspend_call(ir, e, suspend_set) {
                let tmp = max_value_index(ir) + 1;
                let var = ir.add_expr(IrExpr::Variable {
                    index: tmp,
                    ty: ret_ty.clone(),
                    init: Some(e),
                });
                let get = ir.add_expr(IrExpr::GetValue(tmp));
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(var);
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

/// Desugar a VALUE-position `try` whose body suspends into a STATEMENT-position one binding a temp, so the
/// flattener (which models a `try` STATEMENT) can handle it: `return try { … } catch { … }` becomes
/// `var tmp = <default>; try { … tmp = <body value> } catch { … tmp = <catch value> }; return tmp`. A
/// suspending branch value is bound to a fresh `Variable` first (the flattener's `stmt_suspension` handles
/// a suspend `Variable` init, not a `SetValue`), then copied to `tmp`. Only `return <try>` is rewritten
/// (the shape mission-core uses); a `val`/assignment of a suspending `try` is left to skip the file.
fn desugar_value_try(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if matches!(ir.exprs[e as usize], IrExpr::Try { .. })
                && expr_calls_suspend(ir, e, suspend_set)
            {
                let IrExpr::Try {
                    body,
                    catches,
                    finally,
                    ..
                } = ir.exprs[e as usize].clone()
                else {
                    unreachable!()
                };
                let tmp = max_value_index(ir) + 1;
                let dflt = zero_value(ir, ret_ty);
                let decl = ir.add_expr(IrExpr::Variable {
                    index: tmp,
                    ty: *ret_ty,
                    init: Some(dflt),
                });
                let new_body = assign_branch_to_tmp(ir, body, tmp, ret_ty, suspend_set);
                let new_catches: Vec<crate::ir::IrCatch> = catches
                    .into_iter()
                    .map(|c| crate::ir::IrCatch {
                        var: c.var,
                        exc_internal: c.exc_internal,
                        body: assign_branch_to_tmp(ir, c.body, tmp, ret_ty, suspend_set),
                    })
                    .collect();
                let new_try = ir.add_expr(IrExpr::Try {
                    body: new_body,
                    catches: new_catches,
                    finally,
                    result: Ty::Unit,
                });
                let get = ir.add_expr(IrExpr::GetValue(tmp));
                let ret = ir.add_expr(IrExpr::Return(Some(get)));
                new_stmts.push(decl);
                new_stmts.push(new_try);
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

/// Desugar a VALUE-position `when`/`if` in `return` position whose BRANCH VALUES suspend (but whose
/// CONDITIONS do not — a suspending condition is hoisted earlier) into a STATEMENT-position `when` binding
/// a temp: `return when (x) { a -> v0; else -> v1 }` becomes `var tmp = <default>; when (x) { a -> { …
/// tmp = v0 }; else -> { … tmp = v1 } }; return tmp`. The flattener models a `when` STATEMENT with
/// suspending branch bodies (`emit_when_stmt`), so each branch's suspension surfaces there. Only
/// `return <when>` is rewritten (the shape mission-core's `applyOperation` uses); a `val`/assignment of a
/// suspending value-`when` is left to the flattener's `stmt_cond_suspension` / a skip.
fn desugar_value_when(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &Ty) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            // Only a `when` whose BRANCH values suspend and whose CONDITIONS do NOT (those are hoisted
            // before this pass) — otherwise leave it to the condition-hoist / a skip.
            if matches!(ir.exprs[e as usize], IrExpr::When { .. })
                && expr_calls_suspend(ir, e, suspend_set)
                && !when_cond_suspends(ir, e, suspend_set)
            {
                let IrExpr::When { branches } = ir.exprs[e as usize].clone() else {
                    unreachable!()
                };
                let tmp = max_value_index(ir) + 1;
                let dflt = zero_value(ir, ret_ty);
                let decl = ir.add_expr(IrExpr::Variable {
                    index: tmp,
                    ty: *ret_ty,
                    init: Some(dflt),
                });
                let new_branches: Branches = branches
                    .into_iter()
                    .map(|(cond, body)| {
                        (
                            cond,
                            assign_branch_to_tmp(ir, body, tmp, ret_ty, suspend_set),
                        )
                    })
                    .collect();
                let new_when = ir.add_expr(IrExpr::When {
                    branches: new_branches,
                });
                let get = ir.add_expr(IrExpr::GetValue(tmp));
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
) -> ExprId {
    let (mut stmts, value) = match ir.exprs[branch as usize].clone() {
        IrExpr::Block { stmts, value } => (stmts, value),
        _ => (Vec::new(), Some(branch)),
    };
    if let Some(v) = value {
        if stmt_diverges(ir, v) {
            // A divergent branch VALUE (`else -> throw …`, `-> return …`, or a nested all-arms-divergent
            // `if`/`when`) produces no value to bind: emit it as a plain statement. Assigning it to `tmp`
            // would leave a dead `goto` after the `athrow`/`return` (a frameless VerifyError);
            // `stmt_diverges` on this same value suppresses that trailing goto.
            stmts.push(v);
        } else if expr_calls_suspend(ir, v, suspend_set) {
            let fresh = max_value_index(ir) + 1;
            let var = ir.add_expr(IrExpr::Variable {
                index: fresh,
                ty: *ty,
                init: Some(v),
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

/// If `when_expr` is an `if`/`when` EXPRESSION with a suspension in one of its CONDITIONS (which evaluate
/// unconditionally, before any branch), hoist those conditions to preceding bound temps (pushed onto
/// `out`) and return a NEW `When` whose conditions read the temps. `None` when `when_expr` isn't a `When`
/// or no condition suspends — the caller keeps the original. Branch VALUES are left untouched (a
/// branch-value suspension is the flattener's job). Shared by the tail-value / `return` / `val =` arms.
fn hoist_when_cond_suspensions(
    ir: &mut IrFile,
    when_expr: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    out: &mut Vec<ExprId>,
) -> Option<ExprId> {
    let IrExpr::When { branches } = ir.exprs[when_expr as usize].clone() else {
        return None;
    };
    let cond_suspends = branches
        .iter()
        .any(|(c, _)| c.is_some_and(|c| expr_calls_suspend(ir, c, suspend_set)));
    if !cond_suspends {
        return None;
    }
    let mut prelude: Vec<ExprId> = Vec::new();
    let new_branches: Branches = branches
        .into_iter()
        .map(|(cond, body)| {
            (
                cond.map(|c| hoist_expr(ir, c, suspend_set, orig_rets, &mut prelude)),
                body,
            )
        })
        .collect();
    out.extend(prelude);
    Some(ir.add_expr(IrExpr::When {
        branches: new_branches,
    }))
}

fn hoist_suspensions(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, orig_rets: &[Ty]) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut out: Vec<ExprId> = Vec::with_capacity(stmts.len());
    for s in stmts {
        hoist_stmt(ir, s, suspend_set, orig_rets, &mut out);
    }
    // A block's TAIL VALUE that is an `if`/`when` EXPRESSION (a lambda's tail expression, `runBlocking { …;
    // if (susp()) a else b }`) — hoist a suspension in its CONDITIONS to a preceding temp, exactly as a
    // `return if (susp()) …` statement above, so the flattener never meets a condition-suspending When it
    // can't model.
    let new_value = value
        .map(|v| hoist_when_cond_suspensions(ir, v, suspend_set, orig_rets, &mut out).unwrap_or(v));
    ir.exprs[b as usize] = IrExpr::Block {
        stmts: out,
        value: new_value,
    };
}

/// Append `stmt` (with unconditional nested suspensions hoisted) plus its hoist temps to `out`.
fn hoist_stmt(
    ir: &mut IrFile,
    stmt: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    out: &mut Vec<ExprId>,
) {
    // Statements the flattener handles directly keep their suspension in place.
    match &ir.exprs[stmt as usize] {
        // An `if`/`when` STATEMENT: its CONDITIONS evaluate unconditionally (before any branch), so a
        // suspension there (`if (c && check())`) is hoisted to a preceding bound temp; the BODIES stay
        // for the flattener (`emit_when_stmt`). A `while` keeps its suspension in place.
        IrExpr::When { branches } => {
            let branches = branches.clone();
            let cond_suspends = branches
                .iter()
                .any(|(c, _)| c.is_some_and(|c| expr_calls_suspend(ir, c, suspend_set)));
            if !cond_suspends {
                out.push(stmt);
                return;
            }
            let mut prelude: Vec<ExprId> = Vec::new();
            let new_branches: Branches = branches
                .into_iter()
                .map(|(cond, body)| {
                    let nc = cond.map(|c| hoist_expr(ir, c, suspend_set, orig_rets, &mut prelude));
                    (nc, body)
                })
                .collect();
            out.extend(prelude);
            let nw = ir.add_expr(IrExpr::When {
                branches: new_branches,
            });
            out.push(nw);
            return;
        }
        // A `return if (susp()) a else b` / `return when (susp()) { … }` — the tail `if`/`when` EXPRESSION's
        // CONDITIONS evaluate unconditionally (before any branch), so a suspension there is hoisted to a
        // preceding bound temp, then the `return` re-wraps the When with the hoisted condition. Without this
        // the flattener meets a `Return(When{cond suspends})` it can't model and bails. (Only the condition
        // is hoisted; a branch VALUE that suspends stays for the flattener / a later skip.)
        IrExpr::Return(Some(v)) if when_cond_suspends(ir, *v, suspend_set) => {
            let nw = hoist_when_cond_suspensions(ir, *v, suspend_set, orig_rets, out)
                .expect("guard ensured a condition suspends");
            let nr = ir.add_expr(IrExpr::Return(Some(nw)));
            out.push(nr);
            return;
        }
        IrExpr::While { body, .. } => {
            // The loop CONDITION/update stay for the flattener; but a statement in the loop BODY with a
            // suspension buried in a call argument (`list.addAll(repo.get())` in a `for`) must be hoisted
            // to `val tmp = repo.get(); list.addAll(tmp)` — the flattener models a bound-local suspension,
            // not one in an argument. Recurse into the body block (in place); nested loops recurse too.
            let body = *body;
            if matches!(ir.exprs[body as usize], IrExpr::Block { .. }) {
                hoist_suspensions(ir, body, suspend_set, orig_rets);
            }
            out.push(stmt);
            return;
        }
        // A `Block` STATEMENT — a `for` loop lowers to `{ val it = xs.iterator(); while(…){…} }`, a spliced
        // scope block, etc. Recurse so a suspension buried in a call argument inside it (or its nested
        // loops) is hoisted to a preceding bound temp before the flattener sees it.
        IrExpr::Block { value: None, .. } => {
            hoist_suspensions(ir, stmt, suspend_set, orig_rets);
            out.push(stmt);
            return;
        }
        IrExpr::Variable { init: Some(i), .. } if is_suspend_call(ir, *i, suspend_set) => {
            out.push(stmt);
            return;
        }
        IrExpr::Variable {
            init: Some(i),
            index,
            ty,
        } if matches!(ir.exprs[*i as usize], IrExpr::When { .. }) => {
            let (i, index, ty) = (*i, *index, *ty);
            // `val a = if (susp()) x else y` — hoist the CONDITION suspension to a preceding temp, then
            // re-bind `a` to the When with the hoisted condition. A branch VALUE that suspends stays for
            // the flattener's `stmt_cond_suspension` (`val a = when { … -> susp() }`), which this arm still
            // routes to (`hoist_when_cond_suspensions` returns `None`) when the condition doesn't suspend.
            match hoist_when_cond_suspensions(ir, i, suspend_set, orig_rets, out) {
                Some(nw) => {
                    let nv = ir.add_expr(IrExpr::Variable {
                        index,
                        ty,
                        init: Some(nw),
                    });
                    out.push(nv);
                }
                None => out.push(stmt),
            }
            return;
        }
        _ if is_suspend_call(ir, stmt, suspend_set) => {
            out.push(stmt);
            return;
        }
        _ => {}
    }
    // Hoist suspensions in the statement's unconditional sub-expressions.
    let new_stmt = hoist_expr(ir, stmt, suspend_set, orig_rets, out);
    out.push(new_stmt);
}

/// Replace each unconditional suspension call in `e` with a fresh `tmp`, appending `val tmp = <call>` to
/// `prelude`. Recurses through value nodes that always evaluate their children; stops at conditional
/// nodes (an inner `if`/`when`/elvis), leaving suspensions there for the flattener (or a later skip).
fn hoist_expr(
    ir: &mut IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    prelude: &mut Vec<ExprId>,
) -> ExprId {
    if is_suspend_call(ir, e, suspend_set) {
        // Hoist nested suspensions in the receiver/arguments first (they evaluate before the call).
        match ir.exprs[e as usize].clone() {
            IrExpr::Call { args, .. } => {
                let na: Vec<ExprId> = args
                    .iter()
                    .map(|&a| hoist_expr(ir, a, suspend_set, orig_rets, prelude))
                    .collect();
                if let IrExpr::Call { args, .. } = &mut ir.exprs[e as usize] {
                    *args = na;
                }
            }
            IrExpr::MethodCall { receiver, args, .. } => {
                let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, prelude);
                let na: Vec<Option<ExprId>> = args
                    .iter()
                    .map(|&a| a.map(|x| hoist_expr(ir, x, suspend_set, orig_rets, prelude)))
                    .collect();
                if let IrExpr::MethodCall {
                    receiver: r,
                    args: a,
                    ..
                } = &mut ir.exprs[e as usize]
                {
                    *r = nr;
                    *a = na;
                }
            }
            _ => {}
        }
        // Logical return type of the suspension: from ir_lower for a cross-unit call, else the callee's
        // `orig_rets` entry (a same-file callee), else `Object`.
        let ty = ir
            .suspend_calls
            .get(&e)
            .cloned()
            .or_else(|| {
                suspend_call_fid(ir, e, suspend_set)
                    .and_then(|fid| orig_rets.get(fid as usize).cloned())
            })
            .unwrap_or_else(object_ty);
        let tmp = max_value_index(ir) + 1;
        let var = ir.add_expr(IrExpr::Variable {
            index: tmp,
            ty,
            init: Some(e),
        });
        prelude.push(var);
        return ir.add_expr(IrExpr::GetValue(tmp));
    }
    match ir.exprs[e as usize].clone() {
        // Unconditional value nodes: recurse, rewriting children.
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            let nl = hoist_expr(ir, lhs, suspend_set, orig_rets, prelude);
            let nr = hoist_expr(ir, rhs, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::PrimitiveBinOp {
                op,
                lhs: nl,
                rhs: nr,
            };
            e
        }
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let na = hoist_expr(ir, arg, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::TypeOp {
                op,
                arg: na,
                type_operand,
            };
            e
        }
        IrExpr::Variable { index, ty, init } => {
            if let Some(i) = init {
                let ni = hoist_expr(ir, i, suspend_set, orig_rets, prelude);
                ir.exprs[e as usize] = IrExpr::Variable {
                    index,
                    ty,
                    init: Some(ni),
                };
            }
            e
        }
        IrExpr::SetValue { var, value } => {
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::SetValue { var, value: nv };
            e
        }
        // A write to a captured `var` (a `Ref`-cell field) or any object field whose right-hand side
        // suspends (`result = await(…)`): hoist the receiver then the value so the suspension becomes a
        // preceding bound temp (`val tmp = await(…); ref.element = tmp`), which the flattener handles.
        IrExpr::SetField {
            receiver,
            class,
            index,
            value,
        } => {
            let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, prelude);
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::SetField {
                receiver: nr,
                class,
                index,
                value: nv,
            };
            e
        }
        IrExpr::Return(Some(v)) => {
            let nv = hoist_expr(ir, v, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::Return(Some(nv));
            e
        }
        // A write to a captured `var` (a `Ref`-cell holder) whose right-hand side suspends
        // (`result = await(…)` for a captured `result`): hoist the holder then the value, so the
        // suspension becomes a preceding bound temp (`val tmp = await(…); ref.element = tmp`).
        IrExpr::RefSet {
            holder,
            elem,
            value,
        } => {
            let nh = hoist_expr(ir, holder, suspend_set, orig_rets, prelude);
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::RefSet {
                holder: nh,
                elem,
                value: nv,
            };
            e
        }
        // A NON-suspend call/member-access whose receiver (or arguments) suspends
        // (`return r.all().size` — the suspend `r.all()` is the receiver of the `.size` read): the
        // receiver and arguments evaluate UNCONDITIONALLY and left-to-right before the access, so hoist
        // each suspension there to a preceding bound temp (`val tmp = r.all(); return tmp.size`), which
        // the flattener handles. A suspend call in this position was already intercepted above.
        IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } => {
            let nr = dispatch_receiver.map(|r| hoist_expr(ir, r, suspend_set, orig_rets, prelude));
            let na: Vec<ExprId> = args
                .iter()
                .map(|&a| hoist_expr(ir, a, suspend_set, orig_rets, prelude))
                .collect();
            ir.exprs[e as usize] = IrExpr::Call {
                callee,
                dispatch_receiver: nr,
                args: na,
            };
            e
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => {
            let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, prelude);
            let na: Vec<Option<ExprId>> = args
                .iter()
                .map(|&a| a.map(|x| hoist_expr(ir, x, suspend_set, orig_rets, prelude)))
                .collect();
            ir.exprs[e as usize] = IrExpr::MethodCall {
                class,
                index,
                receiver: nr,
                args: na,
            };
            e
        }
        IrExpr::GetField {
            receiver,
            class,
            index,
        } => {
            let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, prelude);
            ir.exprs[e as usize] = IrExpr::GetField {
                receiver: nr,
                class,
                index,
            };
            e
        }
        // A leaf or a conditional/unhandled node: leave it (any suspension inside surfaces to the
        // flattener, which restructures it or skips the file).
        _ => e,
    }
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
        IrExpr::MethodCall { class, index, .. } => {
            let fid = *ir.classes[*class as usize].methods.get(*index as usize)?;
            suspend_set.contains(&fid).then_some(fid)
        }
        _ => None,
    }
}

/// Whether `e` is a DIRECT call to a suspend function — same-file (in `suspend_set`, via
/// [`suspend_call_fid`]) OR cross-unit (an `ExprId` recorded in `ir.suspend_calls` by ir_lower from the
/// resolver). The flattener threads the continuation into every such call uniformly.
fn is_suspend_call(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    suspend_call_fid(ir, e, suspend_set).is_some() || ir.suspend_calls.contains_key(&e)
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

/// Append the continuation `cont` as the trailing argument of suspend call `call_e` (a `Call` or
/// `MethodCall`) — the CPS parameter the callee now expects. For a cross-unit `Callee::Static` (resolved
/// by its logical signature), also rewrite the descriptor to the physical CPS form so the emitted
/// `invokestatic` matches the callee. Returns the (unchanged) `ExprId`.
fn append_continuation(ir: &mut IrFile, call_e: ExprId, cont: ExprId) -> ExprId {
    match &mut ir.exprs[call_e as usize] {
        IrExpr::Call {
            args,
            callee: Callee::Static {
                descriptor, name, ..
            },
            ..
        } => {
            // A `suspend` method's `$default` synthetic already spells the `Continuation` in its descriptor
            // — BEFORE the trailing `int mask` and `Object marker`. Insert the continuation VALUE at that
            // position (two before the end) and leave the descriptor unchanged, rather than appending it
            // after the marker (which would pass the mask where the `Continuation` is expected).
            if name.ends_with("$default") && args.len() >= 2 {
                args.insert(args.len() - 2, cont);
            } else {
                *descriptor = cps_descriptor(descriptor);
                args.push(cont);
            }
        }
        // A sibling-file suspend callee: its CPS signature appends a `Continuation` parameter and erases
        // the return to `Object` (the JVM backend builds the descriptor from these `Ty`s).
        IrExpr::Call {
            args,
            callee: Callee::CrossFile { params, ret, .. },
            ..
        } => {
            params.push(continuation_ty());
            *ret = object_ty();
            args.push(cont);
        }
        // A classpath `suspend` MEMBER (`repo.getConfig(id)`, an invokevirtual/invokeinterface): its
        // physical CPS method appends the `Continuation` and erases the return to `Object`, so rewrite the
        // (logical) descriptor to the CPS form before threading the continuation argument.
        IrExpr::Call {
            args,
            callee: Callee::Virtual { descriptor, .. },
            ..
        } => {
            *descriptor = cps_descriptor(descriptor);
            args.push(cont);
        }
        IrExpr::Call { args, .. } => args.push(cont),
        IrExpr::MethodCall { args, .. } => args.push(Some(cont)),
        // A suspend function VALUE call (`block(a)`): the value implements `Function{N+1}`, so append the
        // continuation — the emitter picks `Function{N+1}.invoke` from the arg count.
        IrExpr::InvokeFunction { args, .. } => args.push(cont),
        _ => {}
    }
    call_e
}

/// Rewrite each top-level `Variable { init: Block { stmts: prelude, value: Some(inner) } }` into the
/// `prelude` statements followed by `Variable { init: inner }`. Elvis (`x ?: foo()`) and primitive
/// safe-call elvis lower to such a block-valued initializer; unwrapping it lifts the inner `When` (whose
/// branch value suspends) to a position the flattener's `stmt_cond_suspension` recognizes.
fn normalize_block_inits(ir: &mut IrFile, b: ExprId) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut out: Vec<ExprId> = Vec::with_capacity(stmts.len());
    for s in stmts {
        if let IrExpr::Variable {
            index,
            ref ty,
            init: Some(init),
        } = ir.exprs[s as usize].clone()
        {
            if let IrExpr::Block {
                stmts: pre,
                value: inner_val,
            } = ir.exprs[init as usize].clone()
            {
                // Bind to the block's value; a value-less `Unit` block (`val x: Unit = { …stmts… }`, e.g.
                // a lambda whose tail expression is an assignment) runs its statements then binds the
                // `Unit` singleton — so the binding always leaves a value for its `astore`.
                let inner = match inner_val {
                    Some(inner) => Some(inner),
                    None if *ty == Ty::Unit => Some(ir.add_expr(IrExpr::UnitInstance)),
                    None => None,
                };
                if let Some(inner) = inner {
                    out.extend(pre);
                    let nv = ir.add_expr(IrExpr::Variable {
                        index,
                        ty: ty.clone(),
                        init: Some(inner),
                    });
                    out.push(nv);
                    continue;
                }
            }
        }
        out.push(s);
    }
    ir.exprs[b as usize] = IrExpr::Block { stmts: out, value };
}

/// Whether `e`'s subtree contains any call to a suspend function (used to reject shapes this pass can't
/// restructure — a suspend call nested in an expression, a branch, a loop, etc.).
fn expr_calls_suspend(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if is_suspend_call(ir, e, suspend_set) {
        return true;
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if expr_calls_suspend(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

/// Build the coroutine state machine for `fid` (whose body `b` is a top-level block). The body is
/// flattened into a state graph: each suspension point (including one inside an `if`/`when` branch value)
/// ends a state and starts a resume state, and control flow becomes `label = next` transitions through a
/// `while(true){ r = cont.result; <restore spilled>; when(label){ states } else throw }` dispatch loop. A
/// local live across any suspension point is spilled to a continuation field (restored at the loop top so
/// its slot is frame-consistent on every dispatch path). Returns `false` (skip, never miscompile) for a
/// shape the flattener doesn't handle yet (a suspension nested deeper than a branch value, in a loop, …).
fn build_state_machine(ir: &mut IrFile, facade: &str, fid: u32, b: ExprId, unit_ret: bool) -> bool {
    // Normalize a block-valued initializer (`val a = (x ?: foo())`, `a?.b ?: foo()` — elvis / safe-call
    // lower to `Variable{ init: Block{ prelude…, value: When } }`) into `prelude…; Variable{ init: When }`,
    // so the conditional suspension surfaces as a `Variable{init: When}` the flattener handles.
    normalize_block_inits(ir, b);
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
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
        return false; // a suspending trailing-value body isn't modeled (desugar to a `return` first)
    }
    if binds_value_class_suspension(ir, b, &suspend_set) {
        crate::trace_compiler!(
            "suspend",
            "build_state_machine fid={fid} BAIL: inline-class suspension result across CPS boundary"
        );
        return false; // an inline-class suspension result across the CPS boundary isn't modeled
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

    // For an instance method `this` is value-index 0, so params (and the appended continuation) shift up
    // by one; the receiver's class internal name is the dispatch receiver (the continuation captures it).
    let receiver: Option<String> = ir.functions[fid as usize].dispatch_receiver.clone();
    let this_offset = u32::from(receiver.is_some());
    // Real value parameters (excluding the appended CPS `Continuation`), at value-indices
    // `this_offset .. this_offset + real_params.len()`.
    let real_params: Vec<Ty> = {
        let p = &ir.functions[fid as usize].params;
        p[..p.len().saturating_sub(1)].to_vec()
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
    let mut spilled: Vec<(u32, Ty)> = Vec::new();
    for idx in reads {
        if let Some(ty) = param_ty(idx).or_else(|| find_local_ty(ir, b, idx)) {
            spilled.push((idx, spill_field_ty(ty)));
        }
    }
    // The spilled value parameters — captured at continuation construction (in spilled order).
    let param_caps: Vec<(u32, Ty)> = spilled
        .iter()
        .filter(|(idx, _)| param_ty(*idx).is_some())
        .cloned()
        .collect();
    crate::trace_compiler!(
        "suspend",
        "build_state_machine fid={fid} this_offset={this_offset} completion_idx={completion_idx} spilled={:?} param_caps={:?}",
        spilled.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        param_caps.iter().map(|(i, _)| *i).collect::<Vec<_>>()
    );

    let fname = ir.functions[fid as usize].name.clone();
    // kotlinc nests a suspend method's continuation class under its ENCLOSING class
    // (`Svc$work$1`), and a top-level suspend fun's under the file facade (`FooKt$foo$1`). The
    // dispatch receiver is the enclosing class internal name; a top-level/extension fun has none.
    let cont_owner = receiver.as_deref().unwrap_or(facade);
    // The continuation class uses the SOURCE method name, never the value-class-mangled JVM name:
    // kotlinc names `create-SCm-oBs`'s continuation `<Owner>$create$1`. `-` can't occur in a Kotlin
    // identifier, so it only ever separates the mangle hash — strip from the first `-`.
    let cont_fname = fname.split('-').next().unwrap_or(&fname);
    let cont_internal = format!("{cont_owner}${cont_fname}$1");
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
    let mut catch_spills: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut next_ev = base + 4;
    {
        let mut tries: Vec<(u32, ExprId, String)> = Vec::new();
        find_suspending_catch_tries(ir, b, &suspend_set, &mut tries);
        for (cvar, cbody, exc_internal) in tries {
            let ev = next_ev;
            next_ev += 1;
            let mut reads: Vec<ExprId> = Vec::new();
            collect_getvalue(ir, cbody, cvar, &mut reads);
            for n in reads {
                ir.exprs[n as usize] = IrExpr::GetValue(ev);
            }
            spilled.push((ev, spill_field_ty(Ty::obj(&exc_internal))));
            catch_spills.insert(cvar, ev);
        }
    }
    // Derive the flattener's first fresh local from the actual number of exception spills allocated
    // (`next_ev`), NOT `catch_spills.len()` — so even if two catches ever shared a value-index (making
    // the map shorter than the allocations) no `fresh()` local could alias an `ev`.
    let flat_next_local = next_ev;

    let cont_id = build_continuation_class(
        ir,
        &cont_internal,
        fid,
        &spilled,
        receiver.as_deref(),
        &real_params,
        &param_caps,
    );

    // Flatten the body into a state graph.
    let mut flat = Flat {
        ir,
        suspend: &suspend_set,
        cont_v,
        r_v,
        suspended_v,
        cont_id,
        field_base: 0, // dedicated continuation class: result/label/spilled at field 0..
        spilled: spilled.clone(),
        states: vec![Vec::new()],
        state_handlers: vec![None],
        cur_handler: None,
        catch_var: base + 3,
        catch_spills,
        // Value parameters are assigned on entry (captured at construction, restored at the loop top).
        assigned: param_caps.iter().map(|(l, _)| *l).collect(),
        next_local: flat_next_local,
        loop_targets: Vec::new(),
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
    let spill_field =
        |local: u32| 2 + spilled.iter().position(|(l, _)| *l == local).unwrap() as u32;

    // For an instance method, `new C$fn$1(this, …, completion)` also captures the receiver (value-index
    // 0); a member/top-level with live params additionally passes those param values (`param_caps`).
    let receiver_this = receiver.as_ref().map(|_| 0u32);
    let cap_indices: Vec<u32> = param_caps.iter().map(|(i, _)| *i).collect();
    let get_or_create = build_get_or_create(
        ir,
        completion_idx,
        &cont_ty,
        cont_id,
        receiver_this,
        &cap_indices,
    );
    let var_cont = k(
        ir,
        IrExpr::Variable {
            index: cont_v,
            ty: cont_ty.clone(),
            init: Some(get_or_create),
        },
    );
    let suspended_call = k(
        ir,
        IrExpr::Call {
            callee: Callee::Static {
                owner: "kotlin/coroutines/intrinsics/IntrinsicsKt".to_string(),
                name: "getCOROUTINE_SUSPENDED".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        },
    );
    let var_suspended = k(
        ir,
        IrExpr::Variable {
            index: suspended_v,
            ty: object_ty(),
            init: Some(suspended_call),
        },
    );

    let mut loop_stmts: Vec<ExprId> = Vec::new();
    let cont_for_r = k(ir, IrExpr::GetValue(cont_v));
    let r_init = getf(ir, cont_for_r, 0);
    loop_stmts.push(k(
        ir,
        IrExpr::Variable {
            index: r_v,
            ty: object_ty(),
            init: Some(r_init),
        },
    ));
    let is_param = |local: u32| param_caps.iter().any(|(p, _)| *p == local);
    for (local, ty) in spilled.clone() {
        let cont_for_f = k(ir, IrExpr::GetValue(cont_v));
        let fld = spill_field(local);
        let init = getf(ir, cont_for_f, fld);
        // A value PARAMETER already owns its local slot (from the method signature), so assign it
        // (`SetValue`) rather than declaring a fresh `Variable` — a re-declaration would allocate a
        // phantom second slot and leave the param slot `top` at the loop back-edge (a frame conflict).
        // An after-suspension LOCAL has no slot yet on the first iteration, so it must be declared.
        let restore = if is_param(local) {
            IrExpr::SetValue {
                var: local,
                value: init,
            }
        } else {
            IrExpr::Variable {
                index: local,
                ty,
                init: Some(init),
            }
        };
        loop_stmts.push(k(ir, restore));
    }

    let mut branches: Branches = Vec::new();
    for (i, st) in states.iter().enumerate() {
        let mut ss = vec![throw_on_failure(ir, r_v)];
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
        IrExpr::Const(IrConst::String(
            "call to 'resume' before 'invoke' with coroutine".to_string(),
        )),
    );
    let exc = k(
        ir,
        IrExpr::NewExternal {
            internal: "java/lang/IllegalStateException".to_string(),
            ctor_desc: "(Ljava/lang/String;)V".to_string(),
            args: vec![msg],
        },
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
    let new_body = k(
        ir,
        IrExpr::Block {
            stmts: vec![var_cont, var_suspended, while_loop],
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
    hoist_suspensions(ir, b, &suspend_set, orig_rets);
    if binds_value_class_suspension(ir, b, &suspend_set) {
        crate::trace_compiler!(
            "suspend",
            "build_lambda_sm fid={fid} SKIP: value-class suspension result not modeled"
        );
        return false;
    }
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

    // Append `result`, `label`, then one field per spilled local — after the captures/parameters.
    {
        let cls = &mut ir.classes[class_id as usize];
        let mut push = |name: &str, ty: Ty| {
            // State-machine fields are mutable and non-private (read/written cross-class).
            cls.fields.push(crate::ir::IrField {
                is_private: false,
                ..crate::ir::IrField::new(name.to_string(), ty)
            });
        };
        push("result", object_ty());
        push("label", int_ty());
        for (i, (_, ty)) in spilled.iter().enumerate() {
            push(&format!("L${i}"), ty.clone());
        }
    }

    let base = max_value_index(ir) + 1;
    let r_v = base;
    let suspended_v = base + 1;

    let mut flat = Flat {
        ir,
        suspend: &suspend_set,
        cont_v: 0, // `this`
        r_v,
        suspended_v,
        cont_id: class_id,
        field_base,
        spilled: spilled.clone(),
        states: vec![Vec::new()],
        state_handlers: vec![None],
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
        failed: false,
    };
    for (n, &s) in stmts.iter().enumerate() {
        crate::trace_compiler!(
            "suspend",
            "lambda stmt[{n}] = {:?}",
            flat.ir.exprs[s as usize]
        );
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
    let spill_field =
        |local: u32| 2 + spilled.iter().position(|(l, _)| *l == local).unwrap() as u32;

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
    let suspended_call = k(
        ir,
        IrExpr::Call {
            callee: Callee::Static {
                owner: "kotlin/coroutines/intrinsics/IntrinsicsKt".to_string(),
                name: "getCOROUTINE_SUSPENDED".to_string(),
                descriptor: "()Ljava/lang/Object;".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![],
        },
    );
    let var_suspended = k(
        ir,
        IrExpr::Variable {
            index: suspended_v,
            ty: object_ty(),
            init: Some(suspended_call),
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
        },
    ));
    for (local, ty) in spilled.clone() {
        let this_f = k(ir, IrExpr::GetValue(0));
        let fld = spill_field(local);
        let init = getf(ir, this_f, fld);
        loop_stmts.push(k(
            ir,
            IrExpr::Variable {
                index: local,
                ty,
                init: Some(init),
            },
        ));
    }
    let mut branches: Branches = Vec::new();
    for (i, st) in states.iter().enumerate() {
        let mut ss = vec![throw_on_failure(ir, r_v)];
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
        IrExpr::Const(IrConst::String(
            "call to 'resume' before 'invoke' with coroutine".to_string(),
        )),
    );
    let exc = k(
        ir,
        IrExpr::NewExternal {
            internal: "java/lang/IllegalStateException".to_string(),
            ctor_desc: "(Ljava/lang/String;)V".to_string(),
            args: vec![msg],
        },
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
            },
        ));
    }
    prologue.extend([store_result, var_suspended, while_loop]);
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
    /// user catch variable's value-index to a fresh, spilled value-index holding the caught exception.
    /// The catch body's reads of `e` are pre-rewritten to this index; the handler state binds it from
    /// `r_v` (`e = (E) r_v`) once on entry, and it is spilled/restored like any local so it survives the
    /// catch's own suspension (after which `r_v` holds the resume value, no longer the exception).
    catch_spills: std::collections::HashMap<u32, u32>,
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
    fn spill_field(&self, l: u32) -> u32 {
        2 + self.spilled.iter().position(|(x, _)| *x == l).unwrap() as u32
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
    fn spill_all(&mut self, out: &mut Vec<ExprId>) {
        for (l, ty) in self.spilled.clone() {
            // A spilled local not definitely assigned on this path is dead here (its slot may hold a
            // coalesced value of another type); skip it — its field keeps a type-correct prior/default.
            if !self.assigned.contains(&l) {
                continue;
            }
            let f = self.spill_field(l);
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
        self.spill_all(out);
        self.set_label(out, target);
    }
    /// Emit the suspend-call sequence into `out`, transferring to state `resume` (the loop re-dispatches
    /// `resume` on synchronous completion; on `COROUTINE_SUSPENDED` the function returns and a later
    /// resume re-enters at `resume`).
    fn emit_call(&mut self, out: &mut Vec<ExprId>, call: ExprId, resume: usize) {
        crate::trace_compiler!(
            "suspend",
            "emit_call {call}:{:?}",
            &self.ir.exprs[call as usize]
        );
        self.spill_all(out);
        self.set_label(out, resume);
        let cont_arg = {
            let c = self.gv(self.cont_v);
            self.add(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: c,
                type_operand: continuation_ty(),
            })
        };
        // Thread the continuation into the call (a static `Call` or a member `MethodCall`) — the CPS
        // parameter the callee now expects.
        append_continuation(self.ir, call, cont_arg);
        let vv = self.fresh();
        let var = self.add(IrExpr::Variable {
            index: vv,
            ty: object_ty(),
            init: Some(call),
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
    /// Bind a suspension result from `cont.result` (loaded into `r`) at a resume state's entry.
    fn bind_from_r(&mut self, out: &mut Vec<ExprId>, local: u32, ty: &Ty) {
        crate::trace_compiler!(
            "suspend",
            "bind_from_r local={local} ty={ty:?} spilled={}",
            self.is_spilled(local)
        );
        let rg = self.gv(self.r_v);
        let unb = unbox(self.ir, rg, ty);
        self.mark_assigned(local);
        if self.is_spilled(local) {
            out.push(self.add(IrExpr::SetValue {
                var: local,
                value: unb,
            }));
        } else {
            out.push(self.add(IrExpr::Variable {
                index: local,
                ty: ty.clone(),
                init: Some(unb),
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
    fn loop_jump_target(&self, label: Option<&str>, is_break: bool) -> Option<usize> {
        let frame = match label {
            Some(l) => self
                .loop_targets
                .iter()
                .rev()
                .find(|(fl, _, _)| fl.as_deref() == Some(l)),
            None => self.loop_targets.last(),
        };
        frame.map(|&(_, cont, exit)| if is_break { exit } else { cont })
    }
    fn stmt_suspension(&self, stmt: ExprId) -> Option<Suspension> {
        match &self.ir.exprs[stmt as usize] {
            IrExpr::Variable {
                index,
                ty,
                init: Some(init),
            } => is_suspend_call(self.ir, *init, self.suspend)
                .then(|| (Some((*index, ty.clone())), *init)),
            _ => is_suspend_call(self.ir, stmt, self.suspend).then_some((None, stmt)),
        }
    }
    /// If `stmt` is `val L = when { … }` where a branch value is a direct suspension, return
    /// `(L, ty, branches)`. Sets `failed` if a branch hides a suspension the flattener can't lift.
    fn stmt_cond_suspension(&mut self, stmt: ExprId) -> Option<(u32, Ty, Branches)> {
        let IrExpr::Variable {
            index,
            ty,
            init: Some(init),
        } = &self.ir.exprs[stmt as usize]
        else {
            return None;
        };
        let (index, ty, init) = (*index, ty.clone(), *init);
        let IrExpr::When { branches } = &self.ir.exprs[init as usize] else {
            return None;
        };
        let branches = branches.clone();
        let any_susp = branches
            .iter()
            .any(|(_, v)| is_suspend_call(self.ir, *v, self.suspend));
        // `val v = expr ?: continue` lowers to `val v = when { c -> expr; else -> continue }` — a branch
        // whose VALUE is a loop-jump binds nothing and diverges to the loop's cont/break state. Route the
        // whole binding through `emit_cond` (state-split) so the jump becomes a tail `goto`; otherwise the
        // structured `Continue`/`Break` sits in the merge's value slot → a stackmap/verify mismatch.
        let any_jump = branches
            .iter()
            .any(|(_, v)| match &self.ir.exprs[*v as usize] {
                IrExpr::Break { label } => self.loop_jump_target(label.as_deref(), true).is_some(),
                IrExpr::Continue { label } => {
                    self.loop_jump_target(label.as_deref(), false).is_some()
                }
                _ => false,
            });
        if !any_susp && !any_jump {
            return None;
        }
        // A branch value must be either a direct suspension, a direct loop-jump, or free of both.
        for (_, v) in &branches {
            let direct_jump = matches!(
                self.ir.exprs[*v as usize],
                IrExpr::Break { .. } | IrExpr::Continue { .. }
            );
            if !is_suspend_call(self.ir, *v, self.suspend)
                && !direct_jump
                && (expr_calls_suspend(self.ir, *v, self.suspend) || self.expr_has_loop_jump(*v))
            {
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
            if let Some((label, is_break)) = jump {
                // A loop-jump branch: transfer to the loop's cont/break state; bind nothing (it diverges).
                let target = self
                    .loop_jump_target(label.as_deref(), is_break)
                    .unwrap_or(merge);
                self.goto(&mut bb, target);
            } else if is_suspend_call(self.ir, *value, self.suspend) {
                let br_resume = self.new_state();
                self.emit_call(&mut bb, *value, br_resume);
                let mut rs: Vec<ExprId> = Vec::new();
                self.bind_from_r(&mut rs, local, ty);
                self.goto(&mut rs, merge);
                self.states[br_resume] = rs;
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
                    }));
                }
                self.mark_assigned(local);
                self.goto(&mut bb, merge);
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
            } = self.ir.exprs[stmt as usize].clone()
            {
                if let IrExpr::Block {
                    stmts: bs,
                    value: Some(bv),
                } = self.ir.exprs[init as usize].clone()
                {
                    if self.expr_has_loop_jump(stmt)
                        || expr_calls_suspend(self.ir, stmt, self.suspend)
                        || self.expr_jumps_to_active_frame(stmt)
                    {
                        let rebind = self.add(IrExpr::Variable {
                            index,
                            ty,
                            init: Some(bv),
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
                if let Some(target) = self.loop_jump_target(label.as_deref(), is_break) {
                    self.goto(&mut out, target);
                    self.states[cur] = out;
                    return;
                }
            }
            if let Some((bind, call)) = self.stmt_suspension(stmt) {
                let resume = self.new_state();
                self.emit_call(&mut out, call, resume);
                self.states[cur] = out;
                let mut rs: Vec<ExprId> = Vec::new();
                if let Some((local, ty)) = bind {
                    self.bind_from_r(&mut rs, local, &ty);
                }
                self.states[resume] = rs;
                self.flatten(&stmts[i + 1..], resume, after);
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
                    let merge = self.new_state();
                    let when = self.emit_when_stmt(branches, merge);
                    out.push(when);
                    self.states[cur] = out;
                    self.flatten(&stmts[i + 1..], merge, after);
                    return;
                }
            }
            // A `while`/`do`-`while` loop whose body suspends: header (test) ↔ body ↔ exit. A pre-test
            // loop enters at the header; a post-test (`do`-`while`) enters at the body (runs once first).
            if let IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } = &self.ir.exprs[stmt as usize]
            {
                if expr_calls_suspend(self.ir, *body, self.suspend)
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
                        branches: vec![(Some(cond), t_block), (None, e_block)],
                    });
                    hs.push(hwhen);
                    self.states[header] = hs;
                    // body → cont (back to header after the update). A `continue` in the body targets
                    // `cont` (the update+re-test), a `break` targets `exit`; push the frame so a
                    // `Continue`/`Break` statement flattens to the right `goto` rather than surviving as a
                    // structured node aimed at the dispatch loop.
                    let body_stmts = self.block_stmts(body);
                    self.loop_targets.push((label, cont, exit));
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
            // A `try { … } catch (e) { … }` STATEMENT whose body suspends. Model the common shape: a
            // SINGLE catch, no `finally`, a straight-line catch body (which MAY itself suspend). The
            // try-body states are marked with a handler; the assembly's dispatch `catch` routes an
            // exception thrown while `this.label` is one of them to the handler state, leaving a suspension
            // BEFORE/AFTER the try uncaught. Richer shapes (finally, multiple catches, a BRANCH in the
            // catch) skip the whole file.
            if let IrExpr::Try {
                body,
                catches,
                finally,
                ..
            } = &self.ir.exprs[stmt as usize]
            {
                if expr_calls_suspend(self.ir, stmt, self.suspend) {
                    let (body, catches, finally) = (*body, catches.clone(), *finally);
                    // A BRANCH (`When`) in the catch body — a `?.`/elvis/`if` — introduces a temp/local
                    // whose slot the state machine's exception-handler frame can't reconcile with the try
                    // region (the handler range spans states where that slot is uninitialized), producing
                    // a stack-map mismatch. Skip the file rather than miscompile; a straight-line catch
                    // body (the common `catch (e) { log(e); default }` shape) is fine.
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
                            self.flatten(&body_stmts, try_entry, Some(fin_normal));
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
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    // Anything other than a single `catch` (no finally) is unmodeled.
                    if catches.len() != 1 {
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    let catch_suspends = expr_calls_suspend(self.ir, catches[0].body, self.suspend);
                    if expr_contains_when(self.ir, catches[0].body)
                        // A suspending catch must have been pre-allocated an exception spill in
                        // `build_state_machine`; if not (e.g. a shape reached only after a lambda
                        // boundary), skip rather than emit an unbound read.
                        || (catch_suspends && !self.catch_spills.contains_key(&catches[0].var))
                    {
                        self.failed = true;
                        self.states[cur] = out;
                        return;
                    }
                    let catch = catches.into_iter().next().unwrap();
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
                    // Handler state: the stashed exception arrives in `result` (loaded into `r_v` at the
                    // loop top, like a resume value).
                    let exc_ty = Ty::obj(&catch.exc_internal);
                    let catch_stmts = if catch_suspends {
                        // The catch body itself suspends, so `r_v` is clobbered by its own resume. Bind
                        // the exception ONCE from `r_v` on handler entry into its spilled local `ev`
                        // (whose reads were pre-rewritten in `build_state_machine`); the spill machinery
                        // then carries it across the catch's suspension and restores it for the later
                        // reads (`throw e`).
                        let ev = self.catch_spills[&catch.var];
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
                        // A NON-suspending catch body: `r_v` still holds the exception throughout, so
                        // read it there directly. Avoids a catch-variable LOCAL — which the IR's
                        // value-index reuse can alias with a body local of another type, and which the
                        // emitter can slot-coalesce with an `int` temp (a ref stored into an int slot →
                        // VerifyError).
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
                    self.flatten(&catch_stmts, handler, Some(try_after));
                    // `try_after` joins the body and handler paths: a spilled local is definitely
                    // assigned there only if assigned on BOTH (intersection).
                    let a_handler = std::mem::take(&mut self.assigned);
                    self.assigned = a_body.intersection(&a_handler).copied().collect();
                    self.flatten(&stmts[i + 1..], try_after, after);
                    return;
                }
            }
            if expr_calls_suspend(self.ir, stmt, self.suspend) {
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
            let s2 = self.rewrite_plain(stmt);
            out.push(s2);
        }
        // Transfer to `after` on fall-through — but ONLY if the sequence can fall through. If the last
        // statement diverges (`return`/`throw`), the transition is unreachable dead code: emitting it
        // would leave a `goto` after a `return`/`athrow` with no stack-map frame → a load-time
        // VerifyError. A `return` STATEMENT inside a suspend `try` body, or a `throw` ending a catch body,
        // both hit this.
        let diverges = stmts.last().is_some_and(|&s| stmt_diverges(self.ir, s));
        if !diverges {
            if let Some(a) = after {
                self.goto(&mut out, a);
            }
        }
        self.states[cur] = out;
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
    match &ir.exprs[s as usize] {
        IrExpr::Return(_) | IrExpr::Throw { .. } => true,
        IrExpr::Block { stmts, value: None } => {
            stmts.last().is_some_and(|&last| stmt_diverges(ir, last))
        }
        IrExpr::When { branches } => {
            // A `when` diverges only if it is exhaustive (has an `else`) AND every arm diverges.
            !branches.is_empty()
                && branches.last().is_some_and(|(cond, _)| cond.is_none())
                && branches.iter().all(|(_, body)| stmt_diverges(ir, *body))
        }
        _ => false,
    }
}

/// Collect the value-indices read (`GetValue`) anywhere in `e`'s subtree.
fn collect_reads(ir: &IrFile, e: ExprId, out: &mut Vec<u32>) {
    visit_subtree(&ir.exprs, e, &mut |node| {
        if let IrExpr::GetValue(i) = node {
            out.push(*i);
        }
    });
}

/// Value-indices WRITTEN in `e` that are LIVE across a suspension — the writes NOT confined to a
/// non-suspending (STRUCTURAL) loop. A write inside a structural loop is redone every iteration and read
/// within the same iteration, so it never carries a value across the enclosing suspension; spilling such a
/// local mis-frames the structural loop's back-edge. A write inside a SUSPENDING loop IS live (loop-carried
/// across the inner suspension), so descend there.
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
            if let IrExpr::Variable { index, ty, .. } = node {
                if *index == idx {
                    found = Some(*ty);
                }
            }
        }
    });
    found
}

/// Build the get-or-create prologue: `$completion instanceof Cont && (label & MIN_VALUE) != 0` ⇒ reuse
/// the continuation (clearing the resume bit), else `new Cont($completion)`. Nested `when`s avoid
/// relying on `&&` short-circuit (the cast/getfield must not run when `$completion` isn't our type).
fn build_get_or_create(
    ir: &mut IrFile,
    completion_idx: u32,
    cont_ty: &Ty,
    cont_id: ClassId,
    receiver_this: Option<u32>,
    param_caps: &[u32],
) -> ExprId {
    let k = |ir: &mut IrFile, e: IrExpr| ir.add_expr(e);
    let cast = |ir: &mut IrFile| {
        let c = ir.add_expr(IrExpr::GetValue(completion_idx));
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: c,
            type_operand: cont_ty.clone(),
        })
    };
    let label_of = |ir: &mut IrFile, recv: ExprId| {
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: cont_id,
            index: 1,
        })
    };
    // `new Cont([this,] [param_caps…,] $completion)` — a member continuation captures the receiver as
    // its first arg, then each live value parameter (so the loop-top restore reads correct values on
    // the first iteration), then the completion continuation.
    let new_cont = |ir: &mut IrFile| {
        let mut args = Vec::new();
        if let Some(this_idx) = receiver_this {
            args.push(ir.add_expr(IrExpr::GetValue(this_idx)));
        }
        for &p in param_caps {
            args.push(ir.add_expr(IrExpr::GetValue(p)));
        }
        args.push(ir.add_expr(IrExpr::GetValue(completion_idx)));
        ir.add_expr(IrExpr::New {
            class: cont_id,
            args,
            ctor_params: None,
        })
    };

    let comp = k(ir, IrExpr::GetValue(completion_idx));
    let is_inst = k(
        ir,
        IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: comp,
            type_operand: cont_ty.clone(),
        },
    );
    // (label & MIN_VALUE) != 0
    let c1 = cast(ir);
    let lbl1 = label_of(ir, c1);
    let min1 = k(ir, IrExpr::Const(IrConst::Int(I32_MIN)));
    let masked = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::BitAnd,
            lhs: lbl1,
            rhs: min1,
        },
    );
    let zero = k(ir, IrExpr::Const(IrConst::Int(0)));
    let bit_set = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Ne,
            lhs: masked,
            rhs: zero,
        },
    );
    // reuse: cont.label -= MIN_VALUE; yield cont
    let c_recv = cast(ir);
    let c_read = cast(ir);
    let old = label_of(ir, c_read);
    let min2 = k(ir, IrExpr::Const(IrConst::Int(I32_MIN)));
    let newl = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Sub,
            lhs: old,
            rhs: min2,
        },
    );
    let set = k(
        ir,
        IrExpr::SetField {
            receiver: c_recv,
            class: cont_id,
            index: 1,
            value: newl,
        },
    );
    let cval = cast(ir);
    let reuse = k(
        ir,
        IrExpr::Block {
            stmts: vec![set],
            value: Some(cval),
        },
    );
    let new1 = new_cont(ir);
    let inner = k(
        ir,
        IrExpr::When {
            branches: vec![(Some(bit_set), reuse), (None, new1)],
        },
    );
    let new2 = new_cont(ir);
    k(
        ir,
        IrExpr::When {
            branches: vec![(Some(is_inst), inner), (None, new2)],
        },
    )
}

/// Synthesize the `Facade$fn$1 extends ContinuationImpl` continuation class: `result`/`label` fields, a
/// field per spilled local, a `<init>(Continuation)` delegating to super, and `invokeSuspend` (store the
/// resume value, set the `MIN_VALUE` label bit, re-enter the outer function).
/// A type-correct zero/`null` placeholder for `ty`, used as a value-parameter argument when
/// `invokeSuspend` re-enters the outer function — the real value is restored from the continuation
/// field at the loop top, so this placeholder is immediately overwritten (kotlinc passes `iconst_0`).
pub(crate) fn zero_value(ir: &mut IrFile, ty: &Ty) -> ExprId {
    use crate::types::Ty;
    let c = match super::ir_emit::ir_ty_to_jvm(ty) {
        Ty::Boolean => IrConst::Boolean(false),
        Ty::Byte => IrConst::Byte(0),
        Ty::Short => IrConst::Short(0),
        Ty::Int => IrConst::Int(0),
        Ty::Long => IrConst::Long(0),
        Ty::Float => IrConst::Float(0.0),
        Ty::Double => IrConst::Double(0.0),
        Ty::Char => IrConst::Char('\0'),
        _ => IrConst::Null,
    };
    ir.add_expr(IrExpr::Const(c))
}

fn build_continuation_class(
    ir: &mut IrFile,
    internal: &str,
    outer_fid: u32,
    spilled: &[(u32, Ty)],
    receiver: Option<&str>,
    params: &[Ty],
    param_caps: &[(u32, Ty)],
) -> ClassId {
    let class_id = ir.classes.len() as ClassId;
    // result(0), label(1), spilled(2..), and — for a member — the captured receiver `this$0` last.
    let recv_field_idx = 2 + spilled.len() as u32;

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
            ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: owner.to_string(),
                    name,
                    descriptor,
                    interface: false,
                },
                dispatch_receiver: Some(recv),
                args: reentry_args,
            })
        }
    };
    let ret = ir.add_expr(IrExpr::Return(Some(call_outer)));
    let inv_body = ir.add_expr(IrExpr::Block {
        stmts: vec![set_result, set_label, ret],
        value: None,
    });
    let inv_fid = ir.add_fun(IrFunction {
        name: "invokeSuspend".to_string(),
        params: vec![object_ty()],
        ret: object_ty(),
        body: Some(inv_body),
        is_static: false,
        dispatch_receiver: Some(internal.to_string()),
        param_checks: vec![None],
    });

    // State-machine fields: `result`/`label`/`L$i` are mutable and non-private (read/written
    // cross-class by the resume machinery).
    let mut fields = vec![
        crate::ir::IrField {
            is_private: false,
            ..crate::ir::IrField::new("result".to_string(), object_ty())
        },
        crate::ir::IrField {
            is_private: false,
            ..crate::ir::IrField::new("label".to_string(), int_ty())
        },
    ];
    for (i, (_, ty)) in spilled.iter().enumerate() {
        fields.push(crate::ir::IrField {
            is_private: false,
            ..crate::ir::IrField::new(format!("L${i}"), ty.clone())
        });
    }

    // Constructor value-indices: `this`=0, then (member) the receiver, then each captured value
    // parameter, then the completion `Continuation`. Store the receiver to `this$0` and each captured
    // param to its `L$i` field, then `super(completion)`. A top-level fn with no live params is just
    // `<init>(Continuation)`.
    let mut ctor_args: Vec<IrCtorArg> = Vec::new();
    let mut ctor_stores: Vec<ExprId> = Vec::new();
    let mut arg_idx = 1u32; // value-index of the next ctor argument (`this` is 0)
    if let Some(owner) = receiver {
        let recv_ty = Ty::obj(owner);
        fields.push(crate::ir::IrField {
            is_final: true,
            is_private: false,
            ..crate::ir::IrField::new("this$0".to_string(), recv_ty.clone())
        });
        let this_c = ir.add_expr(IrExpr::GetValue(0));
        let recv_v = ir.add_expr(IrExpr::GetValue(arg_idx));
        ctor_stores.push(ir.add_expr(IrExpr::SetField {
            receiver: this_c,
            class: class_id,
            index: recv_field_idx,
            value: recv_v,
        }));
        ctor_args.push(IrCtorArg {
            ty: recv_ty,
            is_field: false,
            check: None,
        });
        arg_idx += 1;
    }
    for (v, ty) in param_caps {
        let field = 2 + spilled.iter().position(|(l, _)| l == v).unwrap() as u32;
        let this_c = ir.add_expr(IrExpr::GetValue(0));
        let val = ir.add_expr(IrExpr::GetValue(arg_idx));
        ctor_stores.push(ir.add_expr(IrExpr::SetField {
            receiver: this_c,
            class: class_id,
            index: field,
            value: val,
        }));
        ctor_args.push(IrCtorArg {
            ty: ty.clone(),
            is_field: false,
            check: None,
        });
        arg_idx += 1;
    }
    ctor_args.push(IrCtorArg {
        ty: continuation_ty(),
        is_field: false,
        check: None,
    });
    let super_completion_idx = arg_idx;
    let init_body = (!ctor_stores.is_empty()).then(|| {
        ir.add_expr(IrExpr::Block {
            stmts: ctor_stores,
            value: None,
        })
    });

    let super_arg = ir.add_expr(IrExpr::GetValue(super_completion_idx));
    let class = IrClass {
        fq_name: internal.to_string(),
        serial_names: Vec::new(),
        custom_serializer: None,
        field_serializers: Vec::new(),
        contextual_fields: Vec::new(),
        is_value: false,
        type_param_bounds: vec![],
        type_params: Vec::new(),
        supertypes: vec![],
        fields,
        ctor_param_count: 0,
        ctor_args,
        init_body,
        explicit_param_stores: false,
        methods: vec![inv_fid],
        is_interface: false,
        is_annotation: false,
        annotation_impl_of: None,
        is_sealed: false,
        is_abstract: false,
        superclass: CONTINUATION_IMPL.to_string(),
        super_args: vec![super_arg],
        enum_entries: vec![],
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: vec![],
        interfaces: vec![],
        is_object: false,
        is_companion: false,
        companion_class: None,
        secondary_ctors: vec![],
        has_primary_ctor: true,
        applied_annotations: Vec::new(),
        field_annotations: Vec::new(),
        runtime_retained: false,
    };
    ir.add_class(class)
}

/// `kotlin.ResultKt.throwOnFailure(result)` — propagates a failed resume (a no-op on a normal value).
fn throw_on_failure(ir: &mut IrFile, result_v: u32) -> ExprId {
    let r = ir.add_expr(IrExpr::GetValue(result_v));
    ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: "kotlin/ResultKt".to_string(),
            name: "throwOnFailure".to_string(),
            descriptor: "(Ljava/lang/Object;)V".to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![r],
    })
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
        exc_internal: "java/lang/Throwable".to_string(),
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
    match t {
        Ty::Nullable(inner) | Ty::TyParam(_, inner) => reference_needs_checkcast(inner),
        Ty::String => true,
        Ty::Obj(i, _) => *i != "kotlin/Any" && !is_boxed_primitive_internal(i),
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
            let terminates = stmts.last().is_some_and(|&s| {
                matches!(
                    ir.exprs[s as usize],
                    IrExpr::Return(_) | IrExpr::Throw { .. }
                )
            });
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
fn spill_field_ty(ty: Ty) -> Ty {
    if ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        ty
    }
}

/// True if `e`'s subtree binds the result of a suspension to a value(inline)-class-typed local. An
/// inline-class value returned across the `Object`-typed CPS resume boundary needs box-impl/unbox-impl
/// handling that the state-machine restore doesn't model yet, so such a suspend body is SKIPPED (the file
/// cleanly falls back to unsupported — never a miscompile). The common (non-value-class) result path is
/// unaffected.
fn binds_value_class_suspension(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if let IrExpr::Variable {
        ty: Ty::Obj(internal, _),
        init: Some(init),
        ..
    } = &ir.exprs[e as usize]
    {
        if is_suspend_call(ir, *init, suspend_set)
            && ir
                .classes
                .iter()
                .any(|c| c.fq_name == *internal && c.is_value)
        {
            return true;
        }
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        found = found || binds_value_class_suspension(ir, c, suspend_set);
    });
    found
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
        IrExpr::Block { stmts, value } => {
            for s in stmts {
                if !box_returns(ir, s) {
                    return false;
                }
            }
            value.is_none_or(|val| box_returns(ir, val))
        }
        IrExpr::When { branches } => branches
            .into_iter()
            .all(|(cond, body)| cond.is_none_or(|c| box_returns(ir, c)) && box_returns(ir, body)),
        IrExpr::Const(_)
        | IrExpr::GetValue(_)
        | IrExpr::GetStatic(_)
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::UnitInstance => true,
        IrExpr::TypeOp { arg, .. } | IrExpr::NotNullAssert { operand: arg } => box_returns(ir, arg),
        IrExpr::Throw { operand } => box_returns(ir, operand),
        IrExpr::StringConcat(parts) => parts.into_iter().all(|p| box_returns(ir, p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => box_returns(ir, lhs) && box_returns(ir, rhs),
        IrExpr::SetValue { value, .. } => box_returns(ir, value),
        IrExpr::SetField { value, .. } => box_returns(ir, value),
        IrExpr::RefGet { holder, .. } => box_returns(ir, holder),
        IrExpr::RefSet { holder, value, .. } => box_returns(ir, holder) && box_returns(ir, value),
        IrExpr::Variable { init, .. } => init.is_none_or(|i| box_returns(ir, i)),
        IrExpr::GetField { receiver, .. } => box_returns(ir, receiver),
        IrExpr::Call { args, .. } => args.into_iter().all(|a| box_returns(ir, a)),
        IrExpr::MethodCall { receiver, args, .. } => {
            box_returns(ir, receiver) && args.into_iter().flatten().all(|a| box_returns(ir, a))
        }
        IrExpr::New { args, .. } | IrExpr::NewExternal { args, .. } => {
            args.into_iter().all(|a| box_returns(ir, a))
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            box_returns(ir, cond)
                && box_returns(ir, body)
                && update.is_none_or(|u| box_returns(ir, u))
        }
        // A lambda argument (`m.map { it.value }`) is a VALUE — its body is a separate impl function,
        // not a `return` of the suspend function being boxed — so it is a leaf here (no outer return to
        // box inside it). Its captures are ordinary outer-scope value reads handled by the other arms.
        IrExpr::Lambda { .. } => true,
        // A `vararg` argument's elements evaluate in the enclosing expression — validate each.
        IrExpr::Vararg { elements, .. } => elements.into_iter().all(|el| box_returns(ir, el)),
        // `try { … } catch … finally { … }`: box a `return` in the try body, in each catch body, and in
        // the finally. The try/finally is emitted with its own exception table (unchanged by the CPS
        // return-boxing); a suspension INSIDE the try body is a separate case the flattener still
        // declines (its `finally`-across-states isn't modeled), so this only enables non-suspending try
        // bodies inside a suspend function.
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            box_returns(ir, body)
                && catches.into_iter().all(|c| box_returns(ir, c.body))
                && finally.is_none_or(|f| box_returns(ir, f))
        }
        IrExpr::Break { .. } | IrExpr::Continue { .. } => true,
        other => {
            crate::trace_compiler!("suspend", "box_returns BAIL: unhandled node {other:?}");
            false
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

/// Whether `e`'s subtree contains a `When` (a branch — `if`/`when`/`?.`/elvis) in its OWN (directly
/// flattened) flow. A branch in a suspend try's CATCH body creates a temp whose slot the exception-
/// handler frame can't reconcile → skip. A `When` nested inside a `Lambda` is compiled to a SEPARATE
/// method (not inlined into the handler state), so it is NOT a conflict — don't descend into lambdas.
fn expr_contains_when(ir: &IrFile, e: ExprId) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::When { .. } => return true,
        IrExpr::Lambda { .. } => return false,
        _ => {}
    }
    let mut found = false;
    for_each_child(&ir.exprs, e, &mut |c| {
        if !found && expr_contains_when(ir, c) {
            found = true;
        }
    });
    found
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

/// Collect `(catch_var, catch_body, exc_internal)` for each `try { … } catch (e) { … }` in `e`'s
/// subtree whose CATCH body itself suspends and matches the state machine's straight-line single-catch
/// shape — so [`build_state_machine`] can spill each caught exception across the catch's own suspension
/// (`r_v` no longer holds it once the catch resumes). Does NOT descend into `Lambda` bodies (a suspend
/// lambda has its own state machine, and its value-indices are numbered independently). Skips a catch
/// whose body nests another suspending catch: the two exception variables may alias the same reused
/// value-index, which would make the scoped read-rewrite unsound — `flatten` then bails that shape.
fn find_suspending_catch_tries(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    out: &mut Vec<(u32, ExprId, String)>,
) {
    match &ir.exprs[e as usize] {
        IrExpr::Lambda { .. } => return,
        IrExpr::Try {
            catches, finally, ..
        } if finally.is_none()
            && catches.len() == 1
            && expr_calls_suspend(ir, catches[0].body, suspend_set)
            && !expr_contains_when(ir, catches[0].body)
            && !catch_body_nests_suspending_catch(ir, catches[0].body, suspend_set) =>
        {
            let c = &catches[0];
            out.push((c.var, c.body, c.exc_internal.clone()));
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

/// Resolve every `CurrentContinuation` placeholder in `e` to read the continuation value at `slot` (the
/// trailing `Continuation` parameter's value-index). Emitted by `ir_lower` for the lambda parameter of
/// `suspendCoroutineUninterceptedOrReturn { c -> … }`.
fn rewrite_current_continuation(ir: &mut IrFile, e: ExprId, slot: u32) {
    rewrite_subtree(ir, e, &mut |node| {
        if matches!(node, IrExpr::CurrentContinuation) {
            *node = IrExpr::GetValue(slot);
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
