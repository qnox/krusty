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
        let points = match body {
            Some(b) => suspension_points(ir, b, &suspend_set),
            None => Some(Vec::new()),
        };
        let points = match points {
            Some(p) => p,
            None => return false, // a suspend call in a position this pass can't restructure
        };
        // CPS signature: append the continuation parameter, erase the return to Object.
        let f = &mut ir.functions[fid as usize];
        f.params.push(continuation_ty());
        f.param_checks.push(None);
        f.ret = object_ty();

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
                if expr_calls_suspend(ir, s, suspend_set) {
                    return None;
                }
            }
        }
    }
    Some(points)
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
/// points `points`. Slice 2: exactly one suspension point, with no locals declared before it that the
/// tail uses (no field spilling yet) — otherwise skip.
fn build_state_machine(
    ir: &mut IrFile,
    facade: &str,
    fid: u32,
    b: ExprId,
    points: &[Point],
) -> bool {
    if points.len() != 1 {
        return false; // multiple suspension points (needs N states + spilling) — later slice
    }
    let p = &points[0];
    let IrExpr::Block { stmts, .. } = ir.exprs[b as usize].clone() else {
        return false;
    };
    let before = &stmts[..p.stmt];
    let after = &stmts[p.stmt + 1..];
    // No local declared before the suspension point may be used after it (would need field spilling).
    if before
        .iter()
        .any(|&s| matches!(&ir.exprs[s as usize], IrExpr::Variable { .. }))
    {
        return false;
    }

    let fname = ir.functions[fid as usize].name.clone();
    let cont_internal = format!("{facade}${fname}$1");
    let cont_ty = IrType::Class {
        fq_name: cont_internal.clone(),
        type_args: vec![],
        nullable: false,
    };
    // $completion is the suspend function's last parameter (after CPS rewrite).
    let completion_idx = (ir.functions[fid as usize].params.len() - 1) as u32;

    // Fresh local value-indices, safely above every index used anywhere in the arena.
    let mut next = max_value_index(ir) + 1;
    let mut fresh = || {
        let v = next;
        next += 1;
        v
    };
    let cont_v = fresh();
    let result_v = fresh();
    let suspended_v = fresh();
    let tmp_v = fresh();

    // --- the continuation class `Facade$fn$1` ---
    let cont_id = build_continuation_class(ir, &cont_internal, fid, completion_idx);

    // helpers
    let k = |ir: &mut IrFile, e: IrExpr| ir.add_expr(e);
    let cint = |ir: &mut IrFile, n: i32| ir.add_expr(IrExpr::Const(IrConst::Int(n)));
    let label_get = |ir: &mut IrFile, recv: ExprId| {
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: cont_id,
            index: 1,
        })
    };

    // --- prologue: cont = (resume?) reuse $completion : new Facade$fn$1($completion) ---
    let get_or_create = {
        let comp1 = k(ir, IrExpr::GetValue(completion_idx));
        let is_inst = k(
            ir,
            IrExpr::TypeOp {
                op: IrTypeOp::InstanceOf,
                arg: comp1,
                type_operand: cont_ty.clone(),
            },
        );
        // reuse branch: ((Facade$fn$1)$completion).label -= MIN_VALUE ; yield the cast
        let cast_for_get = {
            let c = k(ir, IrExpr::GetValue(completion_idx));
            k(
                ir,
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: c,
                    type_operand: cont_ty.clone(),
                },
            )
        };
        let lbl = label_get(ir, cast_for_get);
        let min1 = cint(ir, I32_MIN);
        let masked = k(
            ir,
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::BitAnd,
                lhs: lbl,
                rhs: min1,
            },
        );
        let zero = cint(ir, 0);
        let bit_set = k(
            ir,
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Ne,
                lhs: masked,
                rhs: zero,
            },
        );
        let cast_set_recv = {
            let c = k(ir, IrExpr::GetValue(completion_idx));
            k(
                ir,
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: c,
                    type_operand: cont_ty.clone(),
                },
            )
        };
        let cast_set_read = {
            let c = k(ir, IrExpr::GetValue(completion_idx));
            k(
                ir,
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: c,
                    type_operand: cont_ty.clone(),
                },
            )
        };
        let old_lbl = label_get(ir, cast_set_read);
        let min2 = cint(ir, I32_MIN);
        let new_lbl = k(
            ir,
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Sub,
                lhs: old_lbl,
                rhs: min2,
            },
        );
        let set_lbl = k(
            ir,
            IrExpr::SetField {
                receiver: cast_set_recv,
                class: cont_id,
                index: 1,
                value: new_lbl,
            },
        );
        let cast_value = {
            let c = k(ir, IrExpr::GetValue(completion_idx));
            k(
                ir,
                IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: c,
                    type_operand: cont_ty.clone(),
                },
            )
        };
        let reuse = k(
            ir,
            IrExpr::Block {
                stmts: vec![set_lbl],
                value: Some(cast_value),
            },
        );
        let new_comp = k(ir, IrExpr::GetValue(completion_idx));
        let new_cont = k(
            ir,
            IrExpr::New {
                class: cont_id,
                args: vec![new_comp],
                ctor_params: None,
            },
        );
        // if (instanceof && bit set) reuse else new  — nested When avoids &&-short-circuit assumptions.
        let inner = k(
            ir,
            IrExpr::When {
                branches: vec![(Some(bit_set), reuse), (None, new_cont)],
            },
        );
        let new_comp2 = k(ir, IrExpr::GetValue(completion_idx));
        let new_cont2 = k(
            ir,
            IrExpr::New {
                class: cont_id,
                args: vec![new_comp2],
                ctor_params: None,
            },
        );
        k(
            ir,
            IrExpr::When {
                branches: vec![(Some(is_inst), inner), (None, new_cont2)],
            },
        )
    };
    let var_cont = k(
        ir,
        IrExpr::Variable {
            index: cont_v,
            ty: cont_ty.clone(),
            init: Some(get_or_create),
        },
    );

    // result = cont.result ; suspended = getCOROUTINE_SUSPENDED()
    let cont_read = k(ir, IrExpr::GetValue(cont_v));
    let result_field = k(
        ir,
        IrExpr::GetField {
            receiver: cont_read,
            class: cont_id,
            index: 0,
        },
    );
    let var_result = k(
        ir,
        IrExpr::Variable {
            index: result_v,
            ty: object_ty(),
            init: Some(result_field),
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

    // --- state 0 (label==0): run `before`, throwOnFailure(result), label=1, call suspend fn ---
    let mut s0: Vec<ExprId> = before.to_vec();
    // The dispatch and both states are EXPRESSIONS that yield the resume value (an `Int` here), bound
    // ONCE to the suspension local `a` via a `when`-value — assigning `a` in two branches of a
    // statement-`when` instead would type its slot eagerly and trip the verifier at the tail merge.
    s0.push(throw_on_failure(ir, result_v));
    let cont_for_set = k(ir, IrExpr::GetValue(cont_v));
    let one = cint(ir, 1);
    s0.push(k(
        ir,
        IrExpr::SetField {
            receiver: cont_for_set,
            class: cont_id,
            index: 1,
            value: one,
        },
    ));
    // tmp = callee(args..., (Continuation)cont)
    let cont_for_arg = k(ir, IrExpr::GetValue(cont_v));
    let cont_arg = k(
        ir,
        IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: cont_for_arg,
            type_operand: continuation_ty(),
        },
    );
    let mut call_args = p.args.clone();
    call_args.push(cont_arg);
    let call = k(
        ir,
        IrExpr::Call {
            callee: Callee::Local(p.callee),
            dispatch_receiver: None,
            args: call_args,
        },
    );
    s0.push(k(
        ir,
        IrExpr::Variable {
            index: tmp_v,
            ty: object_ty(),
            init: Some(call),
        },
    ));
    // value = if (tmp === SUSPENDED) return SUSPENDED else unbox(tmp)  — a two-branch `when`-expression
    // (the suspended branch diverges via `return`, the else branch yields the unboxed result).
    let tmp_r = k(ir, IrExpr::GetValue(tmp_v));
    let susp_r = k(ir, IrExpr::GetValue(suspended_v));
    let is_susp = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: tmp_r,
            rhs: susp_r,
        },
    );
    let susp_ret_val = k(ir, IrExpr::GetValue(suspended_v));
    let ret_susp = k(ir, IrExpr::Return(Some(susp_ret_val)));
    let tmp_for_unbox = k(ir, IrExpr::GetValue(tmp_v));
    let unboxed0 = unbox(ir, tmp_for_unbox, &p.local_ty);
    let s0_value = k(
        ir,
        IrExpr::When {
            branches: vec![(Some(is_susp), ret_susp), (None, unboxed0)],
        },
    );
    let s0_block = k(
        ir,
        IrExpr::Block {
            stmts: s0,
            value: Some(s0_value),
        },
    );

    // --- resume branch: throwOnFailure(result); yield unbox(result) ---
    let tf1 = throw_on_failure(ir, result_v);
    let result_for_unbox = k(ir, IrExpr::GetValue(result_v));
    let unboxed1 = unbox(ir, result_for_unbox, &p.local_ty);
    let s1_block = k(
        ir,
        IrExpr::Block {
            stmts: vec![tf1],
            value: Some(unboxed1),
        },
    );

    // dispatch on cont.label: 0 -> state 0, else -> resume — yields the suspension value.
    let cont_for_dispatch = k(ir, IrExpr::GetValue(cont_v));
    let lbl_d = label_get(ir, cont_for_dispatch);
    let zero_d = cint(ir, 0);
    let is_zero = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: lbl_d,
            rhs: zero_d,
        },
    );
    let dispatch = k(
        ir,
        IrExpr::When {
            branches: vec![(Some(is_zero), s0_block), (None, s1_block)],
        },
    );
    // bind the suspension local once
    let var_a = k(
        ir,
        IrExpr::Variable {
            index: p.local,
            ty: p.local_ty.clone(),
            init: Some(dispatch),
        },
    );

    // assemble the new body: prologue + (a = dispatch) + tail (the original after-statements)
    let mut new_stmts = vec![var_cont, var_result, var_suspended, var_a];
    new_stmts.extend_from_slice(after);
    let new_body = k(
        ir,
        IrExpr::Block {
            stmts: new_stmts,
            value: None,
        },
    );
    ir.functions[fid as usize].body = Some(new_body);

    // box the tail's return value(s) to Object (the function now returns Object).
    box_returns(ir, new_body)
}

/// Synthesize the `Facade$fn$1 extends ContinuationImpl` continuation class and its `invokeSuspend`.
fn build_continuation_class(
    ir: &mut IrFile,
    internal: &str,
    outer_fid: u32,
    _completion_idx: u32,
) -> ClassId {
    let cont_ty = IrType::Class {
        fq_name: internal.to_string(),
        type_args: vec![],
        nullable: false,
    };
    let class_id = ir.classes.len() as ClassId;

    // invokeSuspend(Object result): this.result = result; this.label |= MIN_VALUE; return outer(this).
    // this = value 0, the result arg = value 1.
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

    let class = IrClass {
        fq_name: internal.to_string(),
        is_value: false,
        type_param_bounds: vec![],
        field_type_params: vec![None, None],
        supertypes: vec![],
        fields: vec![
            ("result".to_string(), object_ty()),
            ("label".to_string(), int_ty()),
        ],
        ctor_param_count: 0,
        // one non-field constructor parameter: the completion Continuation, forwarded to super.
        ctor_args: vec![(continuation_ty(), false)],
        init_body: None,
        methods: vec![inv_fid],
        is_interface: false,
        superclass: CONTINUATION_IMPL.to_string(),
        super_args: vec![{
            // in <init>: this = value 0, the ctor param = value 1.
            ir.add_expr(IrExpr::GetValue(1))
        }],
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
        field_final: vec![false, false],
        field_private: vec![false, false],
        secondary_ctors: vec![],
        has_primary_ctor: true,
    };
    let _ = cont_ty;
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
        _ => false,
    }
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
