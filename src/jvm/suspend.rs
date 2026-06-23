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
//! The body is *flattened* into a flat state graph (`Flat`): each suspension point — including one
//! inside an `if`/`when` branch value — ends a state and begins a resume state, control flow becomes
//! `label = next` transitions, and a local live across a suspension point is spilled to a continuation
//! field. The whole thing is ordinary IR (`while(true){ when(label){…} }`), so the existing emitter
//! produces the bytecode + stack-map frames; it is runtime-equivalent to kotlinc's `tableswitch` form
//! (an `if`-chain dispatch). Shapes the flattener doesn't model yet (a suspension nested deeper than a
//! branch value, inside a loop, or a member/extension suspend fn) cause the pass to skip the file —
//! never miscompile.

use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrExpr, IrFile, IrFunction, IrType,
    IrTypeOp,
};
use std::collections::HashSet;

const I32_MIN: i32 = i32::MIN;
/// `when` branches: each `(condition, body)` (an `else` branch has `condition = None`).
type Branches = Vec<(Option<ExprId>, ExprId)>;
/// A direct suspension at a statement: `(optional bound local + type, callee FunId, call args)`.
type Suspension = (Option<(u32, IrType)>, u32, Vec<ExprId>);
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
        let has_susp = body.is_some_and(|b| expr_calls_suspend(ir, b, &suspend_set));
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

        if !has_susp {
            // Leaf: just box the returns (no state machine).
            if let Some(b) = body {
                if !box_returns(ir, b) {
                    return false;
                }
            }
        } else if !build_state_machine(ir, facade, fid, body.unwrap()) {
            return false;
        }
    }
    true
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

/// Build the coroutine state machine for `fid` (whose body `b` is a top-level block). The body is
/// flattened into a state graph: each suspension point (including one inside an `if`/`when` branch value)
/// ends a state and starts a resume state, and control flow becomes `label = next` transitions through a
/// `while(true){ r = cont.result; <restore spilled>; when(label){ states } else throw }` dispatch loop. A
/// local live across any suspension point is spilled to a continuation field (restored at the loop top so
/// its slot is frame-consistent on every dispatch path). Returns `false` (skip, never miscompile) for a
/// shape the flattener doesn't handle yet (a suspension nested deeper than a branch value, in a loop, …).
fn build_state_machine(ir: &mut IrFile, facade: &str, fid: u32, b: ExprId) -> bool {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return false;
    };
    if value.is_some() {
        return false; // a block trailing-value body isn't modeled (suspend bodies use `return`)
    }
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();

    // Spilled locals: any local read at or after the first statement that contains a suspension — a
    // sound over-approximation of "live across a suspension point". Each maps to its declared type.
    let Some(first) = stmts
        .iter()
        .position(|&s| expr_calls_suspend(ir, s, &suspend_set))
    else {
        return false; // caller guarantees a suspension exists
    };
    let mut reads: Vec<u32> = Vec::new();
    for &s in &stmts[first..] {
        collect_reads(ir, s, &mut reads);
    }
    reads.sort_unstable();
    reads.dedup();
    let mut spilled: Vec<(u32, IrType)> = Vec::new();
    for idx in reads {
        if let Some(ty) = find_local_ty(ir, b, idx) {
            spilled.push((idx, ty));
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

    let base = max_value_index(ir) + 1;
    let cont_v = base;
    let r_v = base + 1;
    let suspended_v = base + 2;

    let cont_id = build_continuation_class(ir, &cont_internal, fid, &spilled);

    // Flatten the body into a state graph.
    let mut flat = Flat {
        ir,
        suspend: &suspend_set,
        cont_v,
        r_v,
        suspended_v,
        cont_id,
        spilled: spilled.clone(),
        states: vec![Vec::new()],
        next_local: base + 3,
        failed: false,
    };
    flat.flatten(&stmts, 0, None);
    if flat.failed {
        return false;
    }
    let states = std::mem::take(&mut flat.states);

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

/// Flattener: turns the structured suspend-function body into a flat list of states connected by
/// `label = next` transitions (see [`build_state_machine`]).
struct Flat<'a> {
    ir: &'a mut IrFile,
    suspend: &'a HashSet<u32>,
    cont_v: u32,
    r_v: u32,
    suspended_v: u32,
    cont_id: ClassId,
    spilled: Vec<(u32, IrType)>,
    states: Vec<Vec<ExprId>>,
    next_local: u32,
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
        self.states.len() - 1
    }
    fn is_spilled(&self, l: u32) -> bool {
        self.spilled.iter().any(|(x, _)| *x == l)
    }
    fn spill_field(&self, l: u32) -> u32 {
        2 + self.spilled.iter().position(|(x, _)| *x == l).unwrap() as u32
    }
    fn setfield(&mut self, out: &mut Vec<ExprId>, idx: u32, val: ExprId) {
        let recv = self.gv(self.cont_v);
        let e = self.add(IrExpr::SetField {
            receiver: recv,
            class: self.cont_id,
            index: idx,
            value: val,
        });
        out.push(e);
    }
    fn set_label(&mut self, out: &mut Vec<ExprId>, target: usize) {
        let v = self.add(IrExpr::Const(IrConst::Int(target as i32)));
        self.setfield(out, 1, v);
    }
    fn spill_all(&mut self, out: &mut Vec<ExprId>) {
        for (l, _) in self.spilled.clone() {
            let f = self.spill_field(l);
            let v = self.gv(l);
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
    fn emit_call(&mut self, out: &mut Vec<ExprId>, callee: u32, args: &[ExprId], resume: usize) {
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
        let mut a = args.to_vec();
        a.push(cont_arg);
        let call = self.add(IrExpr::Call {
            callee: Callee::Local(callee),
            dispatch_receiver: None,
            args: a,
        });
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
        let empty = self.add(IrExpr::Block {
            stmts: vec![],
            value: None,
        });
        let when = self.add(IrExpr::When {
            branches: vec![(Some(is), ret), (None, empty)],
        });
        out.push(when);
        let vg = self.gv(vv);
        self.setfield(out, 0, vg); // cont.result = v (so the resume reads the synchronous value)
    }
    /// Bind a suspension result from `cont.result` (loaded into `r`) at a resume state's entry.
    fn bind_from_r(&mut self, out: &mut Vec<ExprId>, local: u32, ty: &IrType) {
        let rg = self.gv(self.r_v);
        let unb = unbox(self.ir, rg, ty);
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
    /// If `stmt` is a (possibly result-discarding) direct suspension, return `(bound local, callee, args)`.
    fn stmt_suspension(&self, stmt: ExprId) -> Option<Suspension> {
        match &self.ir.exprs[stmt as usize] {
            IrExpr::Variable {
                index,
                ty,
                init: Some(init),
            } => as_suspend_call(self.ir, *init, self.suspend)
                .map(|(c, a)| (Some((*index, ty.clone())), c, a)),
            _ => as_suspend_call(self.ir, stmt, self.suspend).map(|(c, a)| (None, c, a)),
        }
    }
    /// If `stmt` is `val L = when { … }` where a branch value is a direct suspension, return
    /// `(L, ty, branches)`. Sets `failed` if a branch hides a suspension the flattener can't lift.
    fn stmt_cond_suspension(&mut self, stmt: ExprId) -> Option<(u32, IrType, Branches)> {
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
            .any(|(_, v)| as_suspend_call(self.ir, *v, self.suspend).is_some());
        if !any_susp {
            return None;
        }
        // A branch value must be either a direct suspension or suspension-free.
        for (_, v) in &branches {
            if as_suspend_call(self.ir, *v, self.suspend).is_none()
                && expr_calls_suspend(self.ir, *v, self.suspend)
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
        ty: &IrType,
        branches: &[(Option<ExprId>, ExprId)],
        merge: usize,
    ) -> ExprId {
        let mut out_branches: Branches = Vec::new();
        for (cond, value) in branches {
            let mut bb: Vec<ExprId> = Vec::new();
            if let Some((callee, args)) = as_suspend_call(self.ir, *value, self.suspend) {
                let br_resume = self.new_state();
                self.emit_call(&mut bb, callee, &args, br_resume);
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
                return self.add(IrExpr::SetValue {
                    var: index,
                    value: init,
                });
            }
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
            if let Some((bind, callee, args)) = self.stmt_suspension(stmt) {
                let resume = self.new_state();
                self.emit_call(&mut out, callee, &args, resume);
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
            // An `if`/`when` STATEMENT whose branch body suspends: route each branch through its own
            // entry state (which flattens the branch), all converging at `merge`.
            if let IrExpr::When { branches } = &self.ir.exprs[stmt as usize] {
                if expr_calls_suspend(self.ir, stmt, self.suspend) {
                    let branches = branches.clone();
                    let merge = self.new_state();
                    let when = self.emit_when_stmt(branches, merge);
                    out.push(when);
                    self.states[cur] = out;
                    self.flatten(&stmts[i + 1..], merge, after);
                    return;
                }
            }
            // A `while` loop whose body suspends: header (test) → body (back-edge to header) → exit.
            if let IrExpr::While {
                cond,
                body,
                update,
                post_test,
                ..
            } = &self.ir.exprs[stmt as usize]
            {
                if !*post_test && expr_calls_suspend(self.ir, *body, self.suspend) {
                    let (cond, body, update) = (*cond, *body, *update);
                    let header = self.new_state();
                    let body_entry = self.new_state();
                    let cont = self.new_state();
                    let exit = self.new_state();
                    // cur → header
                    self.goto(&mut out, header);
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
                    // body → cont (back to header after the update)
                    let body_stmts = self.block_stmts(body);
                    self.flatten(&body_stmts, body_entry, Some(cont));
                    // cont: run the loop update (a `for`-loop increment), then back to header
                    let mut cs: Vec<ExprId> = Vec::new();
                    if let Some(u) = update {
                        let u2 = self.rewrite_plain(u);
                        cs.push(u2);
                    }
                    self.goto(&mut cs, header);
                    self.states[cont] = cs;
                    // exit: the rest
                    self.flatten(&stmts[i + 1..], exit, after);
                    return;
                }
            }
            if expr_calls_suspend(self.ir, stmt, self.suspend) {
                self.failed = true;
                self.states[cur] = out;
                return;
            }
            let s2 = self.rewrite_plain(stmt);
            out.push(s2);
        }
        if let Some(a) = after {
            self.goto(&mut out, a);
        }
        self.states[cur] = out;
    }
}

/// Collect the value-indices read (`GetValue`) anywhere in `e`'s subtree.
fn collect_reads(ir: &IrFile, e: ExprId, out: &mut Vec<u32>) {
    if let IrExpr::GetValue(i) = ir.exprs[e as usize] {
        out.push(i);
    }
    for_each_child(ir, e, &mut |c| collect_reads(ir, c, out));
}

/// The declared type of local `idx`, from its `Variable` declaration somewhere in `b`'s subtree.
fn find_local_ty(ir: &IrFile, b: ExprId, idx: u32) -> Option<IrType> {
    if let IrExpr::Variable { index, ty, .. } = &ir.exprs[b as usize] {
        if *index == idx {
            return Some(ty.clone());
        }
    }
    let mut found = None;
    for_each_child(ir, b, &mut |c| {
        if found.is_none() {
            found = find_local_ty(ir, c, idx);
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
