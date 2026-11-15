//! Boxing a value-class value at each tail of a value-producing position: a return, a `when`
//! branch or a block's value, including a suspending function's tails that must stay boxed.

use super::*;

/// Whether the expr at `id` is an UNBOXED value-class value of class `x` (a `constructor-impl`/
/// `unbox-impl` result, or an identity block over one).
fn is_unboxed_vc(exprs: &[IrExpr], id: ExprId, x: TypeName) -> bool {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: call_callee,
            ..
        } if let Callee::Static { owner, name, .. } = &**call_callee
            && (*owner == x && (name == "constructor-impl" || name == "unbox-impl")) =>
        {
            true
        }
        IrExpr::Block { value: Some(v), .. } => is_unboxed_vc(exprs, *v, x),
        _ => false,
    }
}

/// At a value-producing (return) position, box an unboxed `X` with `box-impl`, recursing through
/// `when`/block tails so each branch is boxed (a `null` branch is left alone).
pub(super) fn box_tail(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_tail(ir, r, x, under);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_tail(ir, v, x, under);
        }
        // A statement-only block (`{ … ; return x }`) tails on its last statement.
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_tail(ir, last, x, under);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_tail(ir, v, x, under);
        }
        _ => {
            if is_unboxed_vc(&ir.exprs, id, x) {
                box_wrap(ir, id, x, under);
            }
        }
    }
}

/// In a suspend function whose CPS result is the box of its value class, return a tail that unboxes
/// a suspend call with that same boxed result (`= delegate.parcel()`) as the box it already is,
/// before [`box_ref_tail`] would box it again: kotlinc returns the callee's `Object` untouched, so
/// the call stays a tail call. `unboxes` holds the exact `unbox-impl` nodes realized over such calls.
pub(super) fn restore_boxed_suspension_tails(
    ir: &mut IrFile,
    id: ExprId,
    unboxes: &HashSet<ExprId>,
) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let tails: Vec<ExprId> = branches.iter().map(|(_, tail)| *tail).collect();
            for tail in tails {
                restore_boxed_suspension_tails(ir, tail, unboxes);
            }
        }
        IrExpr::Block {
            value: Some(tail), ..
        }
        | IrExpr::Return(Some(tail)) => {
            let tail = *tail;
            restore_boxed_suspension_tails(ir, tail, unboxes);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                restore_boxed_suspension_tails(ir, last, unboxes);
            }
        }
        // The checked coercion to the value class, realized as its carrier over the unbox.
        IrExpr::TypeOp {
            op: krusty::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } if unboxes.contains(arg) => {
            let IrExpr::Call {
                dispatch_receiver: Some(boxed),
                ..
            } = ir.exprs[*arg as usize]
            else {
                return;
            };
            // The CPS result is already the box, so it needs no cast to it either. The call keeps
            // its own identity, which its suspension and representation facts are keyed by.
            let value = match ir.exprs[boxed as usize] {
                IrExpr::TypeOp {
                    op: krusty::ir::IrTypeOp::Cast,
                    arg,
                    ..
                } => arg,
                _ => boxed,
            };
            ir.exprs[id as usize] = IrExpr::Block {
                stmts: Vec::new(),
                value: Some(value),
            };
        }
        _ => {}
    }
}

/// Box the tail of `id` to value class `X` for a REFERENCE-slot return (a lambda's `Object`-returning
/// `invoke`): recurse `when`/block/`return` tails, and box any tail value that is not ALREADY a boxed `X`.
/// Unlike [`box_tail`] (which only boxes the syntactic `constructor-impl`/`unbox-impl` forms), this boxes
/// EVERY unboxed tail — `this`, a captured field, a library call returning the unboxed underlying — since
/// the declared value-class return `X` fixes what the box must be. Uniform across all value classes.
pub(super) fn box_ref_tail(ir: &mut IrFile, id: ExprId, x: TypeName, inputs: ReprInputs<'_>) {
    let under = inputs.under;
    // A structural intrinsic point may contain an inlined user block whose own tail has a different
    // type (`suspendCoroutineUninterceptedOrReturn<T> { ... }` may end in a `Unit` block). Its exact physical result belongs
    // to the point as a whole; never recurse through that semantic boundary and reinterpret the block
    // tail as the enclosing function's value-class result.
    if ir.physical_type(id).is_some_and(|ty| {
        ty.non_null()
            .obj_internal()
            .is_some_and(|classifier| classifier == x)
    }) {
        return;
    }
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_ref_tail(ir, r, x, inputs);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_ref_tail(ir, v, x, inputs);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_ref_tail(ir, last, x, inputs);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_ref_tail(ir, v, x, inputs);
        }
        _ => {
            // Already a boxed `X` (a `box-impl` result, a call/slot typed `X`, a `?.`-`when` box) → leave it;
            // otherwise the tail is the unboxed underlying and must be boxed to `X`.
            if !inputs.over(ir).is_boxed_vc(id, x) {
                box_wrap(ir, id, x, under);
            }
        }
    }
}

/// Box the tail of a NULLABLE value-class return `X?` (boxed descriptor `LX;`). Recurses `when`/block/
/// `return` tails like [`box_ref_tail`], but boxes ONLY a tail whose representation IS an unboxed `X`
/// (a value-class field read, a call returning the unboxed underlying, a `constructor-impl`). A `null`
/// tail, an already-boxed `X`, and any UNRELATED value — e.g. a suspend continuation's `kotlin/Result`
/// resume value, which shares the boxed-`Result` return descriptor but is not itself an unboxed
/// `Result` — are left untouched. The widening counterpart of the checker accepting `X` where `X?` is
/// expected. Works for a classpath value class too (it is in `under`, so `box_wrap` emits its `box-impl`).
pub(super) fn box_nullable_vc_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    inputs: ReprInputs<'_>,
    is_tail: bool,
) {
    let under = inputs.under;
    let recur = |ir: &mut IrFile, e: ExprId, t: bool| box_nullable_vc_tail(ir, e, x, inputs, t);
    match ir.exprs[id as usize].clone() {
        // Control flow whose branch RESULTS are tails (they inherit `is_tail`); a `when`/`if` CONDITION
        // is a plain sub-expression that may itself contain a `return` to box.
        IrExpr::When { branches } => {
            for (cond, body) in branches {
                if let Some(c) = cond {
                    recur(ir, c, false);
                }
                recur(ir, body, is_tail);
            }
        }
        IrExpr::Block { stmts, value } => {
            let n = stmts.len();
            for (i, s) in stmts.iter().enumerate() {
                // With no explicit `value`, the LAST statement is the block's value (an implicit return).
                let stmt_tail = is_tail && value.is_none() && i + 1 == n;
                recur(ir, *s, stmt_tail);
            }
            if let Some(v) = value {
                recur(ir, v, is_tail);
            }
        }
        // An explicit `return <v>` (tail OR a guard clause) boxes its returned value uniformly.
        IrExpr::Return(Some(v)) => recur(ir, v, true),
        IrExpr::Return(None) => {}
        // A loop is never a tail value, but a `return` inside its body still belongs to this function.
        IrExpr::While {
            cond, body, update, ..
        } => {
            recur(ir, cond, false);
            recur(ir, body, false);
            if let Some(u) = update {
                recur(ir, u, false);
            }
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            recur(ir, body, is_tail);
            for c in &catches {
                recur(ir, c.body, is_tail);
            }
            if let Some(f) = finally {
                recur(ir, f, false);
            }
        }
        // A lambda's `return`s are the LAMBDA's, not this function's — do not descend.
        IrExpr::Lambda { .. } => {}
        _ => {
            // First descend into any nested `return` (e.g. one inside a call argument), never a tail.
            let mut kids = Vec::new();
            krusty::ir::for_each_child(&ir.exprs, id, &mut |c| kids.push(c));
            for c in kids {
                recur(ir, c, false);
            }
            // Then, at a TAIL, box this value if it is a VC-`x` value not already boxed. A tail is a VC-`x`
            // value when its logical (checker) type IS `x` — a member/local call returning `x`, an
            // `x`-typed field read — or its repr is a syntactic unboxed `x` (a `constructor-impl`). A tail
            // whose logical type is NOT `x` (a `null`, or an unrelated value that merely shares the boxed
            // return descriptor — a suspend continuation's `kotlin/Result` resume value) is left untouched.
            if is_tail {
                let logical_is_x = ir
                    .logical_type(id)
                    .and_then(|t| t.non_null().obj_internal())
                    .is_some_and(|n| n == x);
                let repr_unboxed_x = matches!(
                    inputs.over(ir).repr(id),
                    Repr::Unboxed(c) if c == x
                );
                let already_boxed = inputs.over(ir).is_boxed_vc(id, x);
                if (logical_is_x || repr_unboxed_x) && !already_boxed {
                    box_wrap(ir, id, x, under);
                }
            }
        }
    }
}
