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
//! The state machine is built as ordinary IR (the existing emitter produces the bytecode + frames), in
//! a form that is *runtime-equivalent* to kotlinc's (an `if`-chain dispatch rather than a fall-through
//! `tableswitch`). Shapes not yet handled (multiple suspension points, suspension inside control flow,
//! cross-suspension locals needing field spilling) cause the pass to skip the file — never miscompile.

use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrExpr, IrFile, IrFunction, IrType,
    IrTypeOp,
};
use std::collections::HashSet;

const I32_MIN: i32 = i32::MIN;
/// A suspension point whose result is discarded (a bare `suspendCall()` statement) binds no local.
const NO_LOCAL: u32 = u32::MAX;
const CONTINUATION: &str = "kotlin/coroutines/Continuation";
const CONTINUATION_IMPL: &str = "kotlin/coroutines/jvm/internal/ContinuationImpl";

fn object_ty() -> IrType {
    IrType::Class {
        fq_name: "kotlin/Any".to_string(),
        type_args: vec![],
        nullable: true,
    }
}
fn int_ty() -> IrType {
    IrType::Class {
        fq_name: "kotlin/Int".to_string(),
        type_args: vec![],
        nullable: false,
    }
}
fn continuation_ty() -> IrType {
    IrType::Class {
        fq_name: CONTINUATION.to_string(),
        type_args: vec![],
        nullable: false,
    }
}

/// Rewrite every `suspend fun` in `ir` to the JVM CPS ABI. `facade` is the file's facade class internal
/// name (e.g. `SKt`) — the continuation class for `bar` is `SKt$bar$1`. Returns `false` (skip the whole
/// file, never miscompile) on any suspend shape this pass can't yet transform.
#[must_use]
pub fn lower_suspend(ir: &mut IrFile, facade: &str) -> bool {
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    let fids = ir.suspend_funs.clone();
    for fid in fids {
        let body = ir.functions[fid as usize].body;
        // Desugar `return <suspend call>` (incl. an `= <suspend call>` expression body) into
        // `val tmp = <suspend call>; return tmp` so a tail-position suspension becomes a uniform
        // bound-local point. Uses the function's (pre-CPS) declared return type for `tmp`.
        let ret_ty = ir.functions[fid as usize].ret.clone();
        if let Some(b) = body {
            desugar_tail_suspend(ir, b, &suspend_set, &ret_ty);
        }
        let points = match body {
            Some(b) => suspension_points(ir, b, &suspend_set),
            None => Some(Vec::new()),
        };
        let mut points = match points {
            Some(p) => p,
            None => return false, // a suspend call in a position this pass can't restructure
        };
        // CPS signature: append the continuation parameter, erase the return to Object.
        let p_old = ir.functions[fid as usize].params.len() as u32; // original param count
        let f = &mut ir.functions[fid as usize];
        f.params.push(continuation_ty());
        f.param_checks.push(None);
        f.ret = object_ty();

        // ir_lower assigned body locals value-indices starting at `p_old`, which now collides with the
        // appended continuation parameter (also value-index `p_old`). Shift every body local up by one so
        // the continuation owns `p_old` and no local aliases its JVM slot.
        if let Some(b) = body {
            shift_locals(ir, b, p_old);
        }
        for pt in &mut points {
            if pt.local != NO_LOCAL && pt.local >= p_old {
                pt.local += 1;
            }
        }

        if points.is_empty() {
            // Leaf: just box the returns (no state machine).
            if let Some(b) = body {
                if !box_returns(ir, b) {
                    return false;
                }
            }
        } else if !build_state_machine(ir, facade, fid, body.unwrap(), &points) {
            return false;
        }
    }
    true
}

/// A suspension point: the body statement index (in the top-level block) plus the bound-local index and
/// type and the suspend call's `(FunId, args)`. Slice: the suspension is `val x = <suspend call>(...)`.
struct Point {
    stmt: usize,
    local: u32,
    local_ty: IrType,
    callee: u32,
    args: Vec<ExprId>,
}

/// Identify the suspension points in body `b`. Returns `None` if the body contains a suspend call in a
/// shape this pass can't yet restructure (anything other than a top-level `val x = <suspend call>` whose
/// before-statements declare no locals, and with no suspend call hidden in any other expression). An
/// empty `Vec` means "leaf" (no suspension point).
fn suspension_points(ir: &IrFile, b: ExprId, suspend_set: &HashSet<u32>) -> Option<Vec<Point>> {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        // Not a block body — only acceptable if it contains no suspend call at all (leaf).
        return if expr_calls_suspend(ir, b, suspend_set) {
            None
        } else {
            Some(Vec::new())
        };
    };
    if value.is_some_and(|v| expr_calls_suspend(ir, v, suspend_set)) {
        return None;
    }
    let mut points = Vec::new();
    for (i, &s) in stmts.iter().enumerate() {
        match &ir.exprs[s as usize] {
            IrExpr::Variable {
                index,
                ty,
                init: Some(init),
            } => {
                if let Some((callee, args)) = as_suspend_call(ir, *init, suspend_set) {
                    points.push(Point {
                        stmt: i,
                        local: *index,
                        local_ty: ty.clone(),
                        callee,
                        args,
                    });
                    continue;
                }
                // A non-suspension local: it must not itself hide a suspend call.
                if expr_calls_suspend(ir, *init, suspend_set) {
                    return None;
                }
            }
            _ => {
                // A bare `suspendCall()` statement: a suspension point whose result is discarded.
                if let Some((callee, args)) = as_suspend_call(ir, s, suspend_set) {
                    points.push(Point {
                        stmt: i,
                        local: NO_LOCAL,
                        local_ty: object_ty(),
                        callee,
                        args,
                    });
                    continue;
                }
                if expr_calls_suspend(ir, s, suspend_set) {
                    return None;
                }
            }
        }
    }
    Some(points)
}

/// Rewrite each top-level `return <suspend call>` in `b` into `val tmp = <suspend call>; return tmp`
/// (a fresh local typed `ret_ty`), so a tail-position suspension is handled as an ordinary bound-local
/// suspension point. Runs before the CPS rewrite, so `ret_ty` is the function's declared return type.
fn desugar_tail_suspend(ir: &mut IrFile, b: ExprId, suspend_set: &HashSet<u32>, ret_ty: &IrType) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts = Vec::with_capacity(stmts.len() + 1);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if as_suspend_call(ir, e, suspend_set).is_some() {
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

/// If `e` is a direct call to a suspend function, return its `(FunId, args)`.
fn as_suspend_call(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
) -> Option<(u32, Vec<ExprId>)> {
    if let IrExpr::Call {
        callee: Callee::Local(fid),
        args,
        ..
    } = &ir.exprs[e as usize]
    {
        if suspend_set.contains(fid) {
            return Some((*fid, args.clone()));
        }
    }
    None
}

/// Whether `e`'s subtree contains any call to a suspend function (used to reject shapes this pass can't
/// restructure — a suspend call nested in an expression, a branch, a loop, etc.).
fn expr_calls_suspend(ir: &IrFile, e: ExprId, suspend_set: &HashSet<u32>) -> bool {
    if as_suspend_call(ir, e, suspend_set).is_some() {
        return true;
    }
    let mut found = false;
    for_each_child(ir, e, &mut |c| {
        if expr_calls_suspend(ir, c, suspend_set) {
            found = true;
        }
    });
    found
}

/// Build the coroutine state machine for `fid` (whose body `b` is a top-level block) with the suspension
/// points `points`. Generalizes to N straight-line suspension points: the body is split into per-state
/// segments and rewritten as `while(true){ r = cont.result; <restore spilled>; when(label){states} }`.
/// A suspension-result local that is read in a later state is spilled to a continuation field (restored
/// at the loop top so its slot is frame-consistent on every dispatch path). A *non-suspension* local
/// that crosses a suspension point isn't modeled yet → skip (never miscompile).
fn build_state_machine(
    ir: &mut IrFile,
    facade: &str,
    fid: u32,
    b: ExprId,
    points: &[Point],
) -> bool {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return false;
    };
    if value.is_some() {
        return false; // a block trailing-value body isn't modeled (suspend bodies use `return`)
    }
    let n = points.len();

    // Per-state segments. State 0 runs the statements before the first suspension call; state k
    // (1..n-1) runs those between points[k-1] and points[k]; state n runs the tail. Entering state k
    // (1..=n) binds `points[k-1].local` (the previous suspension's result).
    let mut segs: Vec<Vec<ExprId>> = Vec::with_capacity(n + 1);
    segs.push(stmts[..points[0].stmt].to_vec());
    for w in 1..n {
        segs.push(stmts[points[w - 1].stmt + 1..points[w].stmt].to_vec());
    }
    segs.push(stmts[points[n - 1].stmt + 1..].to_vec());

    // A suspension-result local bound entering state k that is read in a LATER state must be spilled.
    let mut spilled: Vec<(u32, IrType)> = Vec::new();
    for k in 1..=n {
        let local = points[k - 1].local;
        let later_used = segs[k + 1..]
            .iter()
            .flatten()
            .any(|&s| expr_uses_value(ir, s, local));
        if later_used {
            spilled.push((local, points[k - 1].local_ty.clone()));
        }
    }
    // A plain (non-suspension) local declared in a segment and read after a later suspension point isn't
    // modeled yet — skip rather than miscompile.
    for si in 0..segs.len() {
        for idx in 0..segs[si].len() {
            let s = segs[si][idx];
            if let IrExpr::Variable { index, .. } = ir.exprs[s as usize] {
                let later = segs[si + 1..]
                    .iter()
                    .flatten()
                    .any(|&t| expr_uses_value(ir, t, index));
                if later {
                    return false;
                }
            }
        }
    }

    let fname = ir.functions[fid as usize].name.clone();
    let cont_internal = format!("{facade}${fname}$1");
    let cont_ty = IrType::Class {
        fq_name: cont_internal.clone(),
        type_args: vec![],
        nullable: false,
    };
    let completion_idx = (ir.functions[fid as usize].params.len() - 1) as u32;

    let mut next = max_value_index(ir) + 1;
    let mut fresh = || {
        let v = next;
        next += 1;
        v
    };
    let cont_v = fresh();
    let r_v = fresh();
    let suspended_v = fresh();

    let cont_id = build_continuation_class(ir, &cont_internal, fid, &spilled);
    // Continuation field indices: result = 0, label = 1, then each spilled local in order.
    let spilled_idx = spilled.clone();
    let spill_field =
        move |local: u32| 2 + spilled_idx.iter().position(|(l, _)| *l == local).unwrap() as u32;

    let k = |ir: &mut IrFile, e: IrExpr| ir.add_expr(e);
    let cint = |ir: &mut IrFile, n: i32| ir.add_expr(IrExpr::Const(IrConst::Int(n)));
    let getf = |ir: &mut IrFile, recv: ExprId, idx: u32| {
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: cont_id,
            index: idx,
        })
    };

    // --- prologue: var cont = get-or-create; var suspended = COROUTINE_SUSPENDED ---
    let get_or_create = build_get_or_create(ir, completion_idx, &cont_ty, cont_id);
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
                inline: false,
                must_inline: false,
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

    // --- loop body: r = cont.result; restore each spilled local from its field; dispatch(label) ---
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
    for (local, ty) in spilled.clone() {
        let cont_for_f = k(ir, IrExpr::GetValue(cont_v));
        let fld = spill_field(local);
        let init = getf(ir, cont_for_f, fld);
        loop_stmts.push(k(
            ir,
            IrExpr::Variable {
                index: local,
                ty,
                init: Some(init),
            },
        ));
    }

    // --- per-state branches ---
    let mut branches: Vec<(Option<ExprId>, ExprId)> = Vec::new();
    for state in 0..=n {
        let mut ss: Vec<ExprId> = Vec::new();
        ss.push(throw_on_failure(ir, r_v));
        // Bind the previous suspension's result (states 1..=n) — unless it was discarded.
        if state >= 1 && points[state - 1].local != NO_LOCAL {
            let local = points[state - 1].local;
            let ty = points[state - 1].local_ty.clone();
            let r_get = k(ir, IrExpr::GetValue(r_v));
            let unb = unbox(ir, r_get, &ty);
            if spilled.iter().any(|(l, _)| *l == local) {
                // Already declared (restored) at the loop top — reassign.
                ss.push(k(
                    ir,
                    IrExpr::SetValue {
                        var: local,
                        value: unb,
                    },
                ));
            } else {
                ss.push(k(
                    ir,
                    IrExpr::Variable {
                        index: local,
                        ty,
                        init: Some(unb),
                    },
                ));
            }
        }
        ss.extend(segs[state].iter().copied());
        if state < n {
            // Spill every already-bound spilled local before the next suspension call.
            for (local, _) in spilled.clone() {
                let binding_state = points.iter().position(|p| p.local == local).unwrap() + 1;
                if binding_state <= state {
                    let recv = k(ir, IrExpr::GetValue(cont_v));
                    let v = k(ir, IrExpr::GetValue(local));
                    let fld = spill_field(local);
                    ss.push(k(
                        ir,
                        IrExpr::SetField {
                            receiver: recv,
                            class: cont_id,
                            index: fld,
                            value: v,
                        },
                    ));
                }
            }
            // cont.label = state + 1
            let recv = k(ir, IrExpr::GetValue(cont_v));
            let nextlbl = cint(ir, state as i32 + 1);
            ss.push(k(
                ir,
                IrExpr::SetField {
                    receiver: recv,
                    class: cont_id,
                    index: 1,
                    value: nextlbl,
                },
            ));
            // v = callee(args..., (Continuation)cont)
            let cont_arg_recv = k(ir, IrExpr::GetValue(cont_v));
            let cont_arg = k(
                ir,
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: cont_arg_recv,
                    type_operand: continuation_ty(),
                },
            );
            let mut args = points[state].args.clone();
            args.push(cont_arg);
            let call = k(
                ir,
                IrExpr::Call {
                    callee: Callee::Local(points[state].callee),
                    dispatch_receiver: None,
                    args,
                },
            );
            let v_v = fresh();
            ss.push(k(
                ir,
                IrExpr::Variable {
                    index: v_v,
                    ty: object_ty(),
                    init: Some(call),
                },
            ));
            // if (v === COROUTINE_SUSPENDED) return COROUTINE_SUSPENDED  (two-branch `when`: a single-
            // branch `when` statement drops its body, so the else branch is an explicit empty block).
            let v_r = k(ir, IrExpr::GetValue(v_v));
            let susp_r = k(ir, IrExpr::GetValue(suspended_v));
            let is_susp = k(
                ir,
                IrExpr::PrimitiveBinOp {
                    op: IrBinOp::RefEq,
                    lhs: v_r,
                    rhs: susp_r,
                },
            );
            let susp_val = k(ir, IrExpr::GetValue(suspended_v));
            let ret_susp = k(ir, IrExpr::Return(Some(susp_val)));
            let empty = k(
                ir,
                IrExpr::Block {
                    stmts: vec![],
                    value: None,
                },
            );
            ss.push(k(
                ir,
                IrExpr::When {
                    branches: vec![(Some(is_susp), ret_susp), (None, empty)],
                },
            ));
            // cont.result = v  (store the synchronous result for the next loop iteration)
            let recv = k(ir, IrExpr::GetValue(cont_v));
            let v_g = k(ir, IrExpr::GetValue(v_v));
            ss.push(k(
                ir,
                IrExpr::SetField {
                    receiver: recv,
                    class: cont_id,
                    index: 0,
                    value: v_g,
                },
            ));
        }
        // dispatch: label == state for a non-final state; the final state is the `else`.
        let cond = if state < n {
            let recv = k(ir, IrExpr::GetValue(cont_v));
            let lbl = getf(ir, recv, 1);
            let sc = cint(ir, state as i32);
            Some(k(
                ir,
                IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: lbl,
                    rhs: sc,
                },
            ))
        } else {
            None
        };
        let block = k(
            ir,
            IrExpr::Block {
                stmts: ss,
                value: None,
            },
        );
        branches.push((cond, block));
    }
    let dispatch = k(ir, IrExpr::When { branches });
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

/// Build the get-or-create prologue: `$completion instanceof Cont && (label & MIN_VALUE) != 0` ⇒ reuse
/// the continuation (clearing the resume bit), else `new Cont($completion)`. Nested `when`s avoid
/// relying on `&&` short-circuit (the cast/getfield must not run when `$completion` isn't our type).
fn build_get_or_create(
    ir: &mut IrFile,
    completion_idx: u32,
    cont_ty: &IrType,
    cont_id: ClassId,
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
    let new_cont = |ir: &mut IrFile| {
        let c = ir.add_expr(IrExpr::GetValue(completion_idx));
        ir.add_expr(IrExpr::New {
            class: cont_id,
            args: vec![c],
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
fn build_continuation_class(
    ir: &mut IrFile,
    internal: &str,
    outer_fid: u32,
    spilled: &[(u32, IrType)],
) -> ClassId {
    let class_id = ir.classes.len() as ClassId;

    // invokeSuspend(Object result): this.result = result; this.label |= MIN_VALUE; return outer(this).
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
    let call_outer = ir.add_expr(IrExpr::Call {
        callee: Callee::Local(outer_fid),
        dispatch_receiver: None,
        args: vec![this_as_cont],
    });
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

    let mut fields = vec![
        ("result".to_string(), object_ty()),
        ("label".to_string(), int_ty()),
    ];
    let mut field_final = vec![false, false];
    let mut field_private = vec![false, false];
    let mut field_type_params = vec![None, None];
    for (i, (_, ty)) in spilled.iter().enumerate() {
        fields.push((format!("L${i}"), ty.clone()));
        field_final.push(false);
        field_private.push(false);
        field_type_params.push(None);
    }

    let super_arg = ir.add_expr(IrExpr::GetValue(1));
    let class = IrClass {
        fq_name: internal.to_string(),
        is_value: false,
        type_param_bounds: vec![],
        field_type_params,
        supertypes: vec![],
        fields,
        ctor_param_count: 0,
        ctor_args: vec![(continuation_ty(), false)],
        init_body: None,
        methods: vec![inv_fid],
        is_interface: false,
        superclass: CONTINUATION_IMPL.to_string(),
        super_args: vec![super_arg],
        enum_entries: vec![],
        enum_entry_subclass: vec![],
        enum_entry_of: None,
        prop_ref: None,
        bridges: vec![],
        interfaces: vec![],
        is_object: false,
        ctor_param_checks: vec![],
        is_companion: false,
        companion_class: None,
        field_final,
        field_private,
        secondary_ctors: vec![],
        has_primary_ctor: true,
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
            inline: false,
            must_inline: false,
        },
        dispatch_receiver: None,
        args: vec![r],
    })
}

/// Coerce an `Object` value to `target` (unbox a primitive, or checkcast a reference).
fn unbox(ir: &mut IrFile, value: ExprId, target: &IrType) -> ExprId {
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: target.clone(),
    })
}

/// Wrap the value of every `Return` reachable from `e` in an `ImplicitCoercion` to `Object`.
fn box_returns(ir: &mut IrFile, e: ExprId) -> bool {
    match ir.exprs[e as usize].clone() {
        IrExpr::Return(None) => true,
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
        IrExpr::Const(_) | IrExpr::GetValue(_) | IrExpr::GetStatic(_) | IrExpr::UnitInstance => {
            true
        }
        IrExpr::TypeOp { arg, .. } | IrExpr::NotNullAssert { operand: arg } => box_returns(ir, arg),
        IrExpr::Throw { operand } => box_returns(ir, operand),
        IrExpr::StringConcat(parts) => parts.into_iter().all(|p| box_returns(ir, p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => box_returns(ir, lhs) && box_returns(ir, rhs),
        IrExpr::SetValue { value, .. } => box_returns(ir, value),
        IrExpr::SetField { value, .. } => box_returns(ir, value),
        IrExpr::Variable { init, .. } => init.is_none_or(|i| box_returns(ir, i)),
        IrExpr::GetField { receiver, .. } => box_returns(ir, receiver),
        IrExpr::Call { args, .. } => args.into_iter().all(|a| box_returns(ir, a)),
        IrExpr::New { args, .. } => args.into_iter().all(|a| box_returns(ir, a)),
        IrExpr::While {
            cond, body, update, ..
        } => {
            box_returns(ir, cond)
                && box_returns(ir, body)
                && update.is_none_or(|u| box_returns(ir, u))
        }
        _ => false,
    }
}

/// Increment every value-index `>= threshold` in `e`'s subtree (a `GetValue`/`SetValue` read-write or a
/// `Variable` declaration). Used to make room at index `threshold` for the CPS continuation parameter
/// without aliasing a body local. `GetStatic` holds a static-field index (a different namespace) and is
/// left untouched.
fn shift_locals(ir: &mut IrFile, e: ExprId, threshold: u32) {
    match &mut ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= threshold => *i += 1,
        IrExpr::SetValue { var, .. } if *var >= threshold => *var += 1,
        IrExpr::Variable { index, .. } if *index >= threshold => *index += 1,
        _ => {}
    }
    let mut kids = Vec::new();
    for_each_child(ir, e, &mut |c| kids.push(c));
    for c in kids {
        shift_locals(ir, c, threshold);
    }
}

/// Whether `e`'s subtree reads the local/parameter value index `idx` (used to decide which
/// suspension-result locals are live across a later suspension point and must be spilled).
fn expr_uses_value(ir: &IrFile, e: ExprId, idx: u32) -> bool {
    if matches!(ir.exprs[e as usize], IrExpr::GetValue(i) if i == idx) {
        return true;
    }
    let mut found = false;
    for_each_child(ir, e, &mut |c| {
        if expr_uses_value(ir, c, idx) {
            found = true;
        }
    });
    found
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

/// Invoke `f` on each direct child expression of `e` (for the suspend-call scan).
fn for_each_child(ir: &IrFile, e: ExprId, f: &mut impl FnMut(ExprId)) {
    match &ir.exprs[e as usize] {
        IrExpr::Block { stmts, value } => {
            stmts.iter().for_each(|&s| f(s));
            value.iter().for_each(|&v| f(v));
        }
        IrExpr::When { branches } => branches.iter().for_each(|(c, b)| {
            c.iter().for_each(|&c| f(c));
            f(*b);
        }),
        IrExpr::Return(v) => v.iter().for_each(|&v| f(v)),
        IrExpr::TypeOp { arg, .. } | IrExpr::NotNullAssert { operand: arg } => f(*arg),
        IrExpr::Throw { operand } => f(*operand),
        IrExpr::StringConcat(parts) => parts.iter().for_each(|&p| f(p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            f(*lhs);
            f(*rhs);
        }
        IrExpr::SetValue { value, .. } => f(*value),
        IrExpr::SetField {
            receiver, value, ..
        } => {
            f(*receiver);
            f(*value);
        }
        IrExpr::Variable { init, .. } => init.iter().for_each(|&i| f(i)),
        IrExpr::GetField { receiver, .. } => f(*receiver),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            dispatch_receiver.iter().for_each(|&r| f(r));
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            f(*receiver);
            args.iter().flatten().for_each(|&a| f(a));
        }
        IrExpr::New { args, .. } | IrExpr::NewExternal { args, .. } => {
            args.iter().for_each(|&a| f(a))
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            f(*cond);
            f(*body);
            update.iter().for_each(|&u| f(u));
        }
        _ => {}
    }
}
