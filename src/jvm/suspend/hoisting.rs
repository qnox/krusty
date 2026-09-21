//! Lifting a suspension out of an operand position, so the state machine can split there.
//!
//! The flattener can only split a suspend function at a STATEMENT boundary. A suspension buried in
//! an operand — an enum lookup's name, an array's size, a class literal's receiver — has no such
//! boundary, so it is first rewritten to a preceding temp and the operand becomes a read of that
//! temp. Every node kind that evaluates an operand before itself needs an arm here; a node with no
//! arm leaves the suspension where it is and the whole FILE is declined:
//!
//! ```text
//! error: krusty: this suspend-function shape is not yet supported by the IR backend
//! ```
//!
//! Operand order is the contract. Operands are hoisted left to right so each temp is declared in
//! the order the source evaluates it, and each operand is hoisted exactly once.

use super::*;

/// Turn a value `when` with a suspending condition into a result local plus a statement decision tree.
/// A Kotlin `when` does *not* evaluate all conditions eagerly: condition `n + 1` is reached only after
/// condition `n` was false. Consequently a suspending condition cannot simply be hoisted into the
/// surrounding statement prelude. [`hoist_when_statement_branches`] nests the remaining conditions in
/// the preceding arm's `else`, preserving that scope-tower-like evaluation order exactly.
fn hoist_when_cond_suspensions(
    ir: &mut IrFile,
    expression: ExprId,
    expected_ty: Option<Ty>,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
    out: &mut Vec<ExprId>,
) -> Option<ExprId> {
    let when_expr = value_when(ir, expression)?;
    if !when_cond_suspends(ir, when_expr, suspend_set) {
        return None;
    }
    let ty = expected_ty
        .or_else(|| match ir.exprs[expression as usize] {
            IrExpr::TypeOp { type_operand, .. } => Some(type_operand),
            _ => ir.logical_types.get(&expression).copied(),
        })
        .or_else(|| ir.logical_types.get(&when_expr).copied())?;
    let (declaration, conditional, value) =
        bind_value_when_to_fresh_local(ir, expression, &ty, suspend_set)?;
    value_types.insert(
        match ir.exprs[declaration as usize] {
            IrExpr::Variable { index, .. } => index,
            _ => unreachable!("value-when binding must declare its result local"),
        },
        ty,
    );
    out.push(declaration);
    let IrExpr::When { branches } = ir.exprs[conditional as usize].clone() else {
        unreachable!("value-when binding must produce a statement conditional")
    };
    out.extend(hoist_when_statement_branches(
        ir,
        branches,
        suspend_set,
        orig_rets,
        value_types,
    ));
    Some(value)
}

/// Normalize a statement `when` into a left-to-right decision tree. Each condition's hoisted prelude
/// stays immediately before that condition; later conditions live in the previous condition's `else`
/// block and therefore remain unevaluated after a match.
fn hoist_when_statement_branches(
    ir: &mut IrFile,
    branches: Branches,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
) -> Vec<ExprId> {
    let Some(((condition, body), rest)) = branches.split_first() else {
        return Vec::new();
    };
    let body = normalize_when_statement_body(ir, *body, suspend_set, orig_rets, value_types);
    let Some(condition) = *condition else {
        debug_assert!(rest.is_empty(), "checked when else branch must be last");
        return vec![body];
    };

    let mut out = Vec::new();
    let condition = hoist_expr(ir, condition, suspend_set, orig_rets, value_types, &mut out);
    let remaining =
        hoist_when_statement_branches(ir, rest.to_vec(), suspend_set, orig_rets, value_types);
    let else_body = ir.add_expr(IrExpr::Block {
        stmts: remaining,
        value: None,
    });
    out.push(ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), body), (None, else_body)],
    }));
    out
}

fn normalize_when_statement_body(
    ir: &mut IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
) -> ExprId {
    if matches!(ir.exprs[body as usize], IrExpr::Block { .. }) {
        demote_block_value_to_statement(ir, body);
        hoist_suspensions(ir, body, suspend_set, orig_rets, value_types);
        return body;
    }
    let mut statements = Vec::new();
    hoist_stmt(
        ir,
        body,
        suspend_set,
        orig_rets,
        value_types,
        &mut statements,
    );
    if let [only] = statements.as_slice() {
        *only
    } else {
        ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: None,
        })
    }
}

/// Hoist the suspensions inside the bodies of lambdas that will be SPLICED into this frame.
///
/// A suspension leaves the method at the `areturn` that yields `COROUTINE_SUSPENDED`, and the
/// operand stack does not survive that — only locals do, in the continuation's spill fields. So a
/// suspension has to stand alone, with its result bound to a local, exactly as this pass arranges
/// for the suspensions the IR machine can see. `f(one(it) + one(it))` reaches the second call with
/// the first one's result on the stack; bound to temps, both are ordinary statements.
///
/// The bodies are rewritten in place where they are blocks; a bare expression body becomes one, so
/// the lambda's `inline_body` is repointed at the new block.
///
/// Each body is typed in ITS OWN value numbering. An `inline_body` is a copy of the lambda's body
/// numbered as the impl method is — captures first, then the lambda's parameters, then the locals
/// the body declares — so the enclosing function's parameter/local table says nothing about the
/// `s` in `s = s + one(x)`, and reads its capture 0 as whatever the enclosing parameter 0 is. The
/// snapshot that keeps `s` off the stack across the suspension is typed from the impl method's
/// declared parameters and the body's own declarations instead; what neither names still declines,
/// as it must.
pub(super) fn hoist_spliced_inline_bodies(
    ir: &mut IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
) {
    let mut seen = HashSet::new();
    hoist_spliced_walk(ir, body, suspend_set, orig_rets, &mut seen);
}

fn hoist_spliced_walk(
    ir: &mut IrFile,
    expression: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    seen: &mut HashSet<ExprId>,
) {
    if !seen.insert(expression) {
        return;
    }
    if let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body: Some(inner),
        ..
    } = ir.exprs[expression as usize].clone()
    {
        // A nested spliced body runs in this frame too, so normalize the innermost first.
        hoist_spliced_walk(ir, inner, suspend_set, orig_rets, seen);
        let mut value_types = spliced_body_value_types(ir, impl_fn, inner);
        let rewritten = hoist_spliced_body(ir, inner, suspend_set, orig_rets, &mut value_types);
        if rewritten != inner {
            if let IrExpr::Lambda { inline_body, .. } = &mut ir.exprs[expression as usize] {
                *inline_body = Some(rewritten);
            }
        }
        for capture in captures {
            hoist_spliced_walk(ir, capture, suspend_set, orig_rets, seen);
        }
        return;
    }
    let mut children = Vec::new();
    crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        hoist_spliced_walk(ir, child, suspend_set, orig_rets, seen);
    }
}

/// One spliced body, normalized. Returns the body to use — the same id when it was a block.
fn hoist_spliced_body(
    ir: &mut IrFile,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
) -> ExprId {
    match ir.exprs[body as usize].clone() {
        IrExpr::Block { stmts, value } => {
            let mut out = Vec::with_capacity(stmts.len());
            for stmt in stmts {
                hoist_stmt(ir, stmt, suspend_set, orig_rets, value_types, &mut out);
            }
            let value = value.map(|v| {
                let mut prelude = Vec::new();
                let hoisted = hoist_expr(ir, v, suspend_set, orig_rets, value_types, &mut prelude);
                out.extend(prelude);
                hoisted
            });
            ir.exprs[body as usize] = IrExpr::Block { stmts: out, value };
            body
        }
        _ => {
            let mut prelude = Vec::new();
            let hoisted = hoist_expr(ir, body, suspend_set, orig_rets, value_types, &mut prelude);
            if prelude.is_empty() {
                body
            } else {
                ir.add_expr(IrExpr::Block {
                    stmts: prelude,
                    value: Some(hoisted),
                })
            }
        }
    }
}

pub(super) fn hoist_suspensions(
    ir: &mut IrFile,
    b: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
) {
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut out: Vec<ExprId> = Vec::with_capacity(stmts.len());
    for s in stmts {
        hoist_stmt(ir, s, suspend_set, orig_rets, value_types, &mut out);
    }
    // A block's TAIL VALUE that is an `if`/`when` EXPRESSION (a lambda's tail expression, `runBlocking { …;
    // if (susp()) a else b }`) — hoist a suspension in its CONDITIONS to a preceding temp, exactly as a
    // `return if (susp()) …` statement above, so the flattener never meets a condition-suspending When it
    // can't model.
    let new_value = value.map(|v| {
        hoist_when_cond_suspensions(ir, v, None, suspend_set, orig_rets, value_types, &mut out)
            .unwrap_or(v)
    });
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
    value_types: &mut HashMap<u32, Ty>,
    out: &mut Vec<ExprId>,
) {
    if let Some(statements) = diverging_value_consumer_statements(ir, &ir.exprs[stmt as usize]) {
        for statement in statements {
            hoist_stmt(ir, statement, suspend_set, orig_rets, value_types, out);
        }
        return;
    }
    if let IrExpr::Variable {
        index,
        ty,
        init: Some(init),
        named,
    } = ir.exprs[stmt as usize].clone()
    {
        if let IrExpr::Block {
            stmts,
            value: Some(value),
        } = ir.exprs[init as usize].clone()
        {
            if !ir.intrinsic_suspension_points.contains_key(&init)
                && (expr_calls_suspend(ir, init, suspend_set)
                    || expr_contains_owned_loop_jump(ir, init))
            {
                for statement in stmts {
                    hoist_stmt(ir, statement, suspend_set, orig_rets, value_types, out);
                }
                let rebound = ir.add_expr(IrExpr::Variable {
                    index,
                    ty,
                    init: Some(value),
                    named,
                });
                hoist_stmt(ir, rebound, suspend_set, orig_rets, value_types, out);
                return;
            }
        }
    }
    // A bare registered suspension point is atomic even when its physical node is an inlined
    // value-bearing `Block`. Its value is the CPS result that must be compared with
    // `COROUTINE_SUSPENDED`; treating it as an ordinary statement block would discard that value
    // before `emit_suspension` can bind it. Callable points take the same path, so operand hoisting
    // remains uniform and selected-call agnostic.
    if is_suspension_point(ir, stmt, suspend_set) {
        hoist_call_operands_in_order(ir, stmt, suspend_set, orig_rets, value_types, out);
        out.push(stmt);
        return;
    }
    // Statements the flattener handles directly keep their suspension in place.
    match &ir.exprs[stmt as usize] {
        // A statement `when` with suspending conditions becomes a nested decision tree. Keeping the
        // remaining conditions inside the preceding `else` is essential: Kotlin stops testing after
        // the first match. A `while` keeps its suspension in its repeatedly evaluated header.
        IrExpr::When { branches } => {
            let branches = branches.clone();
            let cond_suspends = branches
                .iter()
                .any(|(c, _)| c.is_some_and(|c| expr_calls_suspend(ir, c, suspend_set)));
            if !cond_suspends {
                let branches = branches
                    .into_iter()
                    .map(|(condition, body)| {
                        (
                            condition,
                            normalize_when_statement_body(
                                ir,
                                body,
                                suspend_set,
                                orig_rets,
                                value_types,
                            ),
                        )
                    })
                    .collect();
                ir.exprs[stmt as usize] = IrExpr::When { branches };
                out.push(stmt);
                return;
            }
            out.extend(hoist_when_statement_branches(
                ir,
                branches,
                suspend_set,
                orig_rets,
                value_types,
            ));
            return;
        }
        // A `return if (susp()) a else b` / `return when (susp()) { … }` — the tail `if`/`when` EXPRESSION's
        // CONDITIONS evaluate unconditionally (before any branch), so a suspension there is hoisted to a
        // preceding bound temp, then the `return` re-wraps the When with the hoisted condition. Without this
        // the flattener meets a `Return(When{cond suspends})` it can't model and bails. (Only the condition
        // is hoisted; a branch VALUE that suspends stays for the flattener / a later skip.)
        IrExpr::Return(Some(v)) if when_cond_suspends(ir, *v, suspend_set) => {
            let nw =
                hoist_when_cond_suspensions(ir, *v, None, suspend_set, orig_rets, value_types, out)
                    .expect("guard ensured a condition suspends");
            let nr = ir.add_expr(IrExpr::Return(Some(nw)));
            out.push(nr);
            return;
        }
        IrExpr::While {
            cond,
            body,
            update,
            post_test,
            label,
        } => {
            // The loop CONDITION/update stay for the flattener; but a statement in the loop BODY with a
            // suspension buried in a call argument (`list.addAll(repo.get())` in a `for`) must be hoisted
            // to `val tmp = repo.get(); list.addAll(tmp)` — the flattener models a bound-local suspension,
            // not one in an argument. Recurse into the body block (in place); nested loops recurse too.
            let (cond, body, update, post_test, label) =
                (*cond, *body, *update, *post_test, label.clone());
            if matches!(ir.exprs[body as usize], IrExpr::Block { .. }) {
                // A loop body is always a statement region. Inline expansion can leave its final
                // expression in the block's `value` slot (for example `run { captured += await() }`).
                // Feed that value through the ordinary statement hoister as well; otherwise a
                // suspension in the inline body's last assignment remains hidden until `Flat`
                // appends the block value to the state and has no bound suspension to resume into.
                demote_block_value_to_statement(ir, body);
                hoist_suspensions(ir, body, suspend_set, orig_rets, value_types);
            }
            // A loop condition is conditional with respect to entering the loop, but every time the
            // HEADER is reached its operands evaluate unconditionally. Keep its hoisted suspension
            // temporaries inside a value-bearing block on the condition itself so they run on EVERY
            // test (and, for a post-test loop, only after the first body iteration). The state flattener
            // turns that block into the header's state sequence below.
            let cond = if expr_calls_suspend(ir, cond, suspend_set) {
                let mut prelude = Vec::new();
                let value = hoist_expr(ir, cond, suspend_set, orig_rets, value_types, &mut prelude);
                ir.add_expr(IrExpr::Block {
                    stmts: prelude,
                    value: Some(value),
                })
            } else {
                cond
            };
            ir.exprs[stmt as usize] = IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            };
            out.push(stmt);
            return;
        }
        // A `Block` STATEMENT — a `for` loop lowers to `{ val it = xs.iterator(); while(…){…} }`, a spliced
        // scope block, etc. Recurse so a suspension buried in a call argument inside it (or its nested
        // loops) is hoisted to a preceding bound temp before the flattener sees it.
        IrExpr::Block { value: None, .. } => {
            hoist_suspensions(ir, stmt, suspend_set, orig_rets, value_types);
            out.push(stmt);
            return;
        }
        // A VALUE-bearing `Block` in STATEMENT position (`recv?.let { susp(...) }` with the result
        // discarded): the value runs for effect only — demote it to a trailing statement so the
        // flattener sees a plain value-less block.
        IrExpr::Block {
            stmts: bstmts,
            value: Some(v),
        } => {
            let mut ns = bstmts.clone();
            ns.push(*v);
            ir.exprs[stmt as usize] = IrExpr::Block {
                stmts: ns,
                value: None,
            };
            hoist_suspensions(ir, stmt, suspend_set, orig_rets, value_types);
            out.push(stmt);
            return;
        }
        // Hoist inside each protected region; moving calls outside would change exception handling.
        IrExpr::Try {
            body,
            catches,
            finally,
            result,
        } => {
            let value_region = *result != Ty::Unit;
            let mut regions: Vec<(ExprId, bool)> = std::iter::once((*body, value_region))
                .chain(catches.iter().map(|catch| (catch.body, value_region)))
                .collect();
            regions.extend(finally.iter().map(|finally| (*finally, false)));
            for (region, produces_value) in regions {
                hoist_protected_region(
                    ir,
                    region,
                    produces_value,
                    suspend_set,
                    orig_rets,
                    value_types,
                );
            }
            out.push(stmt);
            return;
        }
        // `val r = <suspend call>` — the flattener binds this shape directly, but the CALL's own
        // receiver/arguments still evaluate before it: hoist a suspension there (and snapshot the
        // effectful operands preceding it) into `out`, keeping the direct init. On an untypeable
        // snapshot the operands stay in place and the emit declines the residual nested suspension.
        IrExpr::Variable { init: Some(i), .. } if is_suspension_point(ir, *i, suspend_set) => {
            let i = *i;
            hoist_call_operands_in_order(ir, i, suspend_set, orig_rets, value_types, out);
            out.push(stmt);
            return;
        }
        IrExpr::Variable {
            init: Some(i),
            index,
            ty,
            named,
        } if value_when(ir, *i).is_some() => {
            let (i, index, ty, named) = (*i, *index, *ty, *named);
            let i = normalize_value_when(ir, i).expect("guard selected a value conditional");
            // `val a = if (susp()) x else y` — hoist the CONDITION suspension to a preceding temp, then
            // re-bind `a` to the When with the hoisted condition. A branch VALUE that suspends stays for
            // the flattener's `stmt_cond_suspension` (`val a = when { … -> susp() }`), which this arm still
            // routes to (`hoist_when_cond_suspensions` returns `None`) when the condition doesn't suspend.
            let i = hoist_when_cond_suspensions(
                ir,
                i,
                Some(ty),
                suspend_set,
                orig_rets,
                value_types,
                out,
            )
            .unwrap_or(i);
            if when_has_non_direct_suspending_branch(ir, i, suspend_set)
                || expr_contains_owned_loop_jump(ir, i)
            {
                let (declaration, conditional, value) =
                    bind_value_when_to_fresh_local(ir, i, &ty, suspend_set)
                        .expect("non-direct suspending branch must bind as a value conditional");
                out.push(declaration);
                out.push(conditional);
                out.push(ir.add_expr(IrExpr::Variable {
                    index,
                    ty,
                    init: Some(value),
                    named,
                }));
            } else {
                ir.exprs[stmt as usize] = IrExpr::Variable {
                    index,
                    ty,
                    init: Some(i),
                    named,
                };
                out.push(stmt);
            }
            return;
        }
        // A captured-variable write whose RHS is a value conditional (`captured = if (check())
        // await() else fallback`). The holder is an already-evaluated lexical capture, so it has no
        // source-visible effect to reorder. Hoist suspending CONDITIONS first, then bind suspending
        // BRANCH values through the same typed normalization used by `return when` above. This
        // leaves the flattener a statement-position conditional followed by one ordinary holder
        // write instead of teaching it a storage-specific conditional grammar.
        IrExpr::RefSet {
            holder,
            elem,
            value,
        } if matches!(ir.exprs[*holder as usize], IrExpr::GetValue(_))
            && value_when(ir, *value).is_some()
            && expr_calls_suspend(ir, *value, suspend_set) =>
        {
            let (holder, elem, mut value) = (*holder, *elem, *value);
            if let Some(conditional) = hoist_when_cond_suspensions(
                ir,
                value,
                Some(elem),
                suspend_set,
                orig_rets,
                value_types,
                out,
            ) {
                value = conditional;
            }
            if let Some((declaration, conditional, value)) =
                bind_value_when_to_fresh_local(ir, value, &elem, suspend_set)
            {
                out.push(declaration);
                out.push(conditional);
                out.push(ir.add_expr(IrExpr::RefSet {
                    holder,
                    elem,
                    value,
                }));
            } else {
                out.push(ir.add_expr(IrExpr::RefSet {
                    holder,
                    elem,
                    value,
                }));
            }
            return;
        }
        _ => {}
    }
    // Hoist suspensions in the statement's unconditional sub-expressions.
    let new_stmt = hoist_expr(ir, stmt, suspend_set, orig_rets, value_types, out);
    out.push(new_stmt);
}

fn hoist_protected_region(
    ir: &mut IrFile,
    region: ExprId,
    produces_value: bool,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
) {
    if matches!(ir.exprs[region as usize], IrExpr::Block { .. }) {
        if !produces_value {
            demote_block_value_to_statement(ir, region);
        }
        hoist_suspensions(ir, region, suspend_set, orig_rets, value_types);
        return;
    }
    let original = ir.add_expr(ir.exprs[region as usize].clone());
    if produces_value {
        let mut stmts = Vec::new();
        let value = hoist_expr(
            ir,
            original,
            suspend_set,
            orig_rets,
            value_types,
            &mut stmts,
        );
        ir.exprs[region as usize] = IrExpr::Block {
            stmts,
            value: Some(value),
        };
    } else {
        let mut stmts = Vec::new();
        hoist_stmt(
            ir,
            original,
            suspend_set,
            orig_rets,
            value_types,
            &mut stmts,
        );
        ir.exprs[region as usize] = IrExpr::Block { stmts, value: None };
    }
}

/// Replace each unconditional suspension call in `e` with a fresh `tmp`, appending `val tmp = <call>` to
/// `prelude`. Recurses through value nodes that always evaluate their children; stops at conditional
/// nodes (an inner `if`/`when`/elvis), leaving suspensions there for the flattener (or a later skip).
fn hoist_expr(
    ir: &mut IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
    prelude: &mut Vec<ExprId>,
) -> ExprId {
    if is_suspension_point(ir, e, suspend_set) {
        // Hoist nested suspensions in the receiver/arguments first (they evaluate before the call),
        // preserving left-to-right operand order. On an untypeable snapshot, leave the whole call
        // unhoisted — the flattener declines the residual nested suspension (skip, not miscompile).
        if !hoist_call_operands_in_order(ir, e, suspend_set, orig_rets, value_types, prelude) {
            return e;
        }
        // Logical return type of the suspension: from common lowering for a cross-unit call or intrinsic
        // point, else the callee's `orig_rets` entry (a same-file callee), else `Object`.
        let ty = value_class_suspension_result(ir, e, suspend_set)
            .map(crate::ir::IrValueClassSuspendResult::boundary_ty)
            .or_else(|| recorded_suspension_result(ir, e))
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
            named: false,
        });
        value_types.insert(tmp, ty);
        prelude.push(var);
        crate::trace_compiler!(
            "suspend",
            "hoist suspension expression={e} into local={tmp} type={ty:?}"
        );
        return ir.add_expr(IrExpr::GetValue(tmp));
    }

    // A value conditional nested in an otherwise-unconditional expression position must become a
    // statement conditional before its branch suspension can be state-split. This is the same typed
    // transformation used for a top-level variable/return value: declare one result local, assign it
    // in every selected branch (binding a direct suspension there), then let the enclosing expression
    // consume the local. The enclosing ordered-operand planner has already snapshotted every earlier
    // operand, so lifting these statements into `prelude` preserves Kotlin evaluation order.
    if let Some(when_expr) = value_when(ir, e) {
        if expr_calls_suspend(ir, e, suspend_set) && when_cond_suspends(ir, when_expr, suspend_set)
        {
            if let Some(value) = hoist_when_cond_suspensions(
                ir,
                e,
                None,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) {
                return value;
            }
        }
        if !expr_calls_suspend(ir, e, suspend_set) {
            return e;
        }
        let semantic_ty = match ir.exprs[e as usize] {
            IrExpr::TypeOp { type_operand, .. } => Some(type_operand),
            _ => ir.logical_types.get(&e).copied(),
        };
        if let Some(ty) = semantic_ty {
            if let Some((declaration, conditional, value)) =
                bind_value_when_to_fresh_local(ir, e, &ty, suspend_set)
            {
                if let IrExpr::Variable { index, .. } = ir.exprs[declaration as usize] {
                    value_types.insert(index, ty);
                }
                // The branch binder exposes the conditional at statement level, but a branch value
                // can still wrap its suspension in checked coercions and a value block (notably a
                // nullable `Unit` safe call). Normalize each selected branch in its own block so its
                // effects remain conditional while the direct suspension becomes visible to `Flat`.
                if let IrExpr::When { branches } = ir.exprs[conditional as usize].clone() {
                    for (_, branch) in branches {
                        if matches!(ir.exprs[branch as usize], IrExpr::Block { .. }) {
                            hoist_suspensions(ir, branch, suspend_set, orig_rets, value_types);
                        }
                    }
                }
                prelude.push(declaration);
                prelude.push(conditional);
                return value;
            }
        }
    }
    match ir.exprs[e as usize].clone() {
        // Unconditional value nodes: recurse, rewriting children.
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            // Primitive operations have the same unconditional left-to-right operand contract as
            // calls, constructors, templates, and varargs. The shared planner can sequence any
            // number of direct suspension points (`await() + await()`) and snapshots an effectful
            // left subtree only when a later suspension would otherwise move ahead of it.
            let operands = [Some(lhs), Some(rhs)];
            let Some(new_operands) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return e;
            };
            let nl = new_operands[0].expect("binary operands have no default-argument holes");
            let nr = new_operands[1].expect("binary operands have no default-argument holes");
            ir.exprs[e as usize] = IrExpr::PrimitiveBinOp {
                op,
                lhs: nl,
                rhs: nr,
            };
            e
        }
        IrExpr::ReifiedTypeOp {
            cast,
            negated,
            arg,
            name,
            erased,
        } => {
            let arg = hoist_expr(ir, arg, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::ReifiedTypeOp {
                cast,
                negated,
                arg,
                name,
                erased,
            };
            e
        }
        IrExpr::BottomValue {
            producer,
            completion,
        } => {
            let producer = hoist_expr(ir, producer, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::BottomValue {
                producer,
                completion,
            };
            e
        }
        // `throw classify(status, body())` — the thrown expression evaluates unconditionally and to
        // completion before control leaves, so a suspension inside it hoists to a preceding temp
        // exactly like the single-operand statements beside this arm. Without it the suspension
        // stayed buried in the operand, where the state-machine flattener cannot split it, and the
        // whole function was declined.
        IrExpr::Throw { operand } => {
            let operand = hoist_expr(ir, operand, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::Throw { operand };
            e
        }
        // `enumValueOf<Level>(pickName())` — the looked-up name evaluates unconditionally before the
        // lookup, so a suspension in it hoists like any other single operand.
        IrExpr::EnumValueOf {
            classifier,
            arg,
            declaration,
        } => {
            let arg = hoist_expr(ir, arg, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::EnumValueOf {
                classifier,
                arg,
                declaration,
            };
            e
        }
        // `var total = count()` where a closure captures `total`: the capture boxes the local into a
        // `Ref` holder, and the suspension ends up in the HOLDER'S construction rather than in an
        // ordinary variable initializer. The boxed value is computed before the holder exists, so it
        // hoists to a preceding temp. Assigning the same local from a suspension later needs nothing
        // here — that is a `RefSet`, which already has its own arm.
        IrExpr::RefNew { elem, init } => {
            let init = hoist_expr(ir, init, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::RefNew { elem, init };
            e
        }
        IrExpr::RefGet { holder, elem } => {
            let holder = hoist_expr(ir, holder, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::RefGet { holder, elem };
            e
        }
        // `arrayOfNulls<String>(count())` — the size evaluates before the array exists, so a
        // suspension in it hoists like any other single operand.
        IrExpr::NewArray { array_type, size } => {
            let size = hoist_expr(ir, size, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::NewArray { array_type, size };
            e
        }
        // A BOUND class literal `make()::class`: the receiver is evaluated for its runtime class, so
        // a suspension in it hoists. An UNBOUND `String::class` carries no operand and cannot
        // suspend, which is why the operand is optional here.
        IrExpr::KClassLiteral {
            classifier,
            value: Some(value),
        } => {
            let value = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::KClassLiteral {
                classifier,
                value: Some(value),
            };
            e
        }
        IrExpr::NotNullAssert { operand, message } => {
            let operand = hoist_expr(ir, operand, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::NotNullAssert { operand, message };
            e
        }
        IrExpr::LateinitCheck { operand, name } => {
            let operand = hoist_expr(ir, operand, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::LateinitCheck { operand, name };
            e
        }
        IrExpr::PrimitiveNeg { operand, ty } => {
            let operand = hoist_expr(ir, operand, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::PrimitiveNeg { operand, ty };
            e
        }
        // A string template / `+=` concat: parts evaluate unconditionally left-to-right, so a
        // suspension inside one (`"a ${susp()} b"`, `result += susp()`) hoists to a preceding temp,
        // and every effectful part BEFORE it snapshots first to keep the source order.
        IrExpr::StringConcat(parts) => {
            let operands: Vec<Option<ExprId>> = parts.iter().map(|&p| Some(p)).collect();
            let Some(no) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return e;
            };
            let np: Vec<ExprId> = no.into_iter().map(|p| p.unwrap()).collect();
            ir.exprs[e as usize] = IrExpr::StringConcat(np);
            e
        }
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => {
            let na = hoist_expr(ir, arg, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::TypeOp {
                op,
                arg: na,
                type_operand,
            };
            e
        }
        IrExpr::Variable {
            index,
            ty,
            init,
            named,
        } => {
            if let Some(i) = init {
                let ni = hoist_expr(ir, i, suspend_set, orig_rets, value_types, prelude);
                ir.exprs[e as usize] = IrExpr::Variable {
                    index,
                    ty,
                    init: Some(ni),
                    named,
                };
            }
            e
        }
        IrExpr::SetValue { var, value } => {
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::SetValue { var, value: nv };
            e
        }
        // A top-level-property write evaluates its right-hand side unconditionally just like a local
        // write. Hoist a suspend function-value invocation before the store so the state machine binds
        // and resumes the call, then performs the write exactly once with the resumed value.
        IrExpr::SetStatic { index, value } => {
            let value = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::SetStatic { index, value };
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
            let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude);
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::SetField {
                receiver: nr,
                class,
                index,
                value: nv,
            };
            e
        }
        IrExpr::Return(Some(v)) => {
            let nv = hoist_expr(ir, v, suspend_set, orig_rets, value_types, prelude);
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
            crate::trace_compiler!(
                "suspend",
                "hoist captured write expression={e} holder={holder} value={value} suspends={}",
                expr_calls_suspend(ir, value, suspend_set)
            );
            let nh = hoist_expr(ir, holder, suspend_set, orig_rets, value_types, prelude);
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
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
        IrExpr::Call { .. } => {
            // Use the same ordered-call reconstruction as the suspension-point path. Keeping one
            // implementation matters here: both direct suspension calls and ordinary calls that
            // merely CONTAIN one obey the identical receiver-then-arguments evaluation contract.
            hoist_call_operands_in_order(ir, e, suspend_set, orig_rets, value_types, prelude);
            e
        }
        IrExpr::MethodCall { .. } => {
            hoist_call_operands_in_order(ir, e, suspend_set, orig_rets, value_types, prelude);
            e
        }
        // Function-value invocation has the same receiver-first evaluation order as a member call:
        // evaluate the function object, then each argument from left to right. Treat it as one
        // ordered operand list so nested suspension points become bound statements without moving
        // an effectful function expression or earlier argument across a later suspension.
        IrExpr::InvokeFunction {
            func,
            args,
            params,
            ret,
        } => {
            let mut operands: Vec<Option<ExprId>> = vec![Some(func)];
            operands.extend(args.iter().map(|&arg| Some(arg)));
            let Some(mut operands) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return e;
            };
            let func = operands
                .remove(0)
                .expect("function invocation always has a function operand");
            let args = operands
                .into_iter()
                .map(|arg| arg.expect("function invocation arguments have no holes"))
                .collect();
            ir.exprs[e as usize] = IrExpr::InvokeFunction {
                func,
                args,
                params,
                ret,
            };
            e
        }
        // Constructors obey the same ordered-operand contract as calls and string concatenation.
        // Reuse the shared planner so an effectful argument before a later suspension is snapshotted
        // in source order (`New(effect(), suspend())`), while an operand whose verifier type cannot
        // be recovered still declines safely. A constructor-only purity list would duplicate the
        // planner and reject shapes it already handles generically.
        IrExpr::New { args, .. } => {
            let operands: Vec<Option<ExprId>> = args.iter().map(|&arg| Some(arg)).collect();
            let Some(new_operands) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return e;
            };
            let new_args = new_operands
                .into_iter()
                .map(|arg| arg.expect("constructor operands have no default-argument holes"))
                .collect();
            if let IrExpr::New { args, .. } = &mut ir.exprs[e as usize] {
                *args = new_args;
            }
            e
        }
        // A vararg literal evaluates every listed/spread element unconditionally and in source
        // order before the receiving call. Treat it as the same ordered operand container as a
        // constructor or string template: each suspension becomes a bound temp, and any earlier
        // effectful element is snapshotted before that suspension. `spreads` describes packing only;
        // it does not alter element evaluation order and therefore survives unchanged.
        IrExpr::Vararg {
            array_type,
            spreads,
            elements,
        } => {
            let operands: Vec<Option<ExprId>> =
                elements.iter().map(|&element| Some(element)).collect();
            let Some(new_operands) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return e;
            };
            let new_elements = new_operands
                .into_iter()
                .map(|element| element.expect("vararg operands have no default-argument holes"))
                .collect();
            ir.exprs[e as usize] = IrExpr::Vararg {
                array_type,
                spreads,
                elements: new_elements,
            };
            e
        }
        IrExpr::GetField {
            receiver,
            class,
            index,
        } => {
            let nr = hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::GetField {
                receiver: nr,
                class,
                index,
            };
            e
        }
        IrExpr::EnclosingInstance {
            receiver,
            inner,
            outer,
        } => {
            let receiver = hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::EnclosingInstance {
                receiver,
                inner,
                outer,
            };
            e
        }
        IrExpr::LateinitInitialized {
            receiver,
            class,
            index,
        } => {
            let receiver = hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::LateinitInitialized {
                receiver,
                class,
                index,
            };
            e
        }
        IrExpr::PropertyRead {
            receiver,
            owner,
            name,
            ty,
            interface,
            operation,
        } => {
            let nr = receiver.map(|receiver| {
                hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude)
            });
            ir.exprs[e as usize] = IrExpr::PropertyRead {
                receiver: nr,
                owner,
                name,
                ty,
                interface,
                operation,
            };
            e
        }
        IrExpr::PropertyWrite {
            receiver,
            owner,
            name,
            value,
            ty,
            interface,
            operation,
        } => {
            let nr = receiver.map(|receiver| {
                hoist_expr(ir, receiver, suspend_set, orig_rets, value_types, prelude)
            });
            let nv = hoist_expr(ir, value, suspend_set, orig_rets, value_types, prelude);
            ir.exprs[e as usize] = IrExpr::PropertyWrite {
                receiver: nr,
                owner,
                name,
                value: nv,
                ty,
                interface,
                operation,
            };
            e
        }
        // A value `Block` reached here is in an unconditional expression position. Normalize its
        // statements at that exact operand position, then its value. The enclosing ordered-operand
        // planner has already arranged snapshots for earlier operands, so lifting the block prelude
        // cannot move it ahead of an earlier effect. This also handles nested lowering blocks such as
        // `{ val a = susp(); a + susp() } + tail`, which otherwise leave the second call hidden after
        // the flattener splices only the first declaration.
        IrExpr::Block {
            stmts,
            value: Some(v),
        } => {
            for statement in stmts {
                hoist_stmt(ir, statement, suspend_set, orig_rets, value_types, prelude);
            }
            preserve_replacement_logical_type(ir, e, v);
            hoist_expr(ir, v, suspend_set, orig_rets, value_types, prelude)
        }
        // A leaf or a conditional/unhandled node: leave it (any suspension inside surfaces to the
        // flattener, which restructures it or skips the file).
        _ => e,
    }
}

/// Hoist any call's receiver/argument operands IN PLACE, preserving their shared left-to-right
/// evaluation contract. Both a direct suspension point and an ordinary call containing a nested
/// suspension use this one reconstruction path, rather than maintaining shape-specific copies.
/// Returns `false` — leaving `e` untouched — when a required operand snapshot cannot be typed; the
/// caller keeps the expression unhoisted so the flattener declines the shape. A non-call intrinsic
/// suspension point has no operands and therefore returns `true`.
fn hoist_call_operands_in_order(
    ir: &mut IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
    prelude: &mut Vec<ExprId>,
) -> bool {
    match ir.exprs[e as usize].clone() {
        IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } => {
            let mut operands: Vec<Option<ExprId>> = vec![dispatch_receiver];
            operands.extend(args.iter().map(|&a| Some(a)));
            let Some(mut no) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return false;
            };
            let nr = no.remove(0);
            let na: Vec<ExprId> = no.into_iter().map(|a| a.unwrap()).collect();
            crate::trace_compiler!(
                "suspend",
                "hoist call operands expression={e} receiver={dispatch_receiver:?}->{nr:?} args={args:?}->{na:?}"
            );
            if let IrExpr::Call {
                dispatch_receiver: r,
                args,
                ..
            } = &mut ir.exprs[e as usize]
            {
                *r = nr;
                *args = na;
            }
            true
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            let mut operands: Vec<Option<ExprId>> = vec![Some(receiver)];
            operands.extend(args.iter().copied());
            let Some(mut no) = hoist_operands_in_order(
                ir,
                &operands,
                suspend_set,
                orig_rets,
                value_types,
                prelude,
            ) else {
                return false;
            };
            let nr = no.remove(0).unwrap();
            if let IrExpr::MethodCall {
                receiver: r,
                args: a,
                ..
            } = &mut ir.exprs[e as usize]
            {
                *r = nr;
                *a = no;
            }
            true
        }
        _ => true,
    }
}

/// Hoist suspensions inside an ordered operand list (a call's receiver + arguments, a template's
/// parts) while preserving Kotlin's strict left-to-right evaluation order: any effectful operand
/// that precedes a later suspending operand is bound to a prelude temp (kotlinc spills every
/// operand of such a call), so `f(g(), susp())` runs `g()` before the suspension. `None` slots
/// (default-argument holes) pass through untouched.
///
/// Returns `None` — with `ir` and `prelude` unmodified — when a required snapshot cannot be given
/// a verifier type; the caller leaves the whole expression unhoisted so the flattener declines the
/// shape (skip, never miscompile). The snapshot plan is decided and typed on the ORIGINAL operands
/// before any rewrite: bailing mid-hoist would strand already-bound prelude temps next to a
/// returned original expression, double-evaluating their effects. Typing the original is valid for
/// the residual: a direct suspension point (residual `GetValue`) is skipped outright, a value block
/// has the type of its final value, and all other rewrites replace children in place while preserving
/// the head node and its recovered type.
fn hoist_operands_in_order(
    ir: &mut IrFile,
    operands: &[Option<ExprId>],
    suspend_set: &HashSet<u32>,
    orig_rets: &[Ty],
    value_types: &mut HashMap<u32, Ty>,
    prelude: &mut Vec<ExprId>,
) -> Option<Vec<Option<ExprId>>> {
    let suspends: Vec<bool> = operands
        .iter()
        .map(|o| o.is_some_and(|x| expr_calls_suspend(ir, x, suspend_set)))
        .collect();
    // Plan phase: type every operand that must be snapshot. A direct suspension point never needs
    // one (its residual is a pure `GetValue`); an operand merely CONTAINING a suspension snapshots
    // conservatively (its residual can still be effectful — `h(susp())` hoists to `h(tmp)`).
    let mut snapshot_ty: Vec<Option<Ty>> = vec![None; operands.len()];
    if let Some(last) = suspends.iter().rposition(|&s| s) {
        for (i, o) in operands.iter().enumerate().take(last) {
            let Some(x) = *o else { continue };
            if is_suspension_point(ir, x, suspend_set) {
                continue;
            }
            if suspends[i] || operand_needs_snapshot(ir, x, value_types) {
                let Some(ty) = hoisted_value_ty(ir, x, orig_rets, value_types) else {
                    crate::trace_compiler!(
                        "suspend",
                        "hoist_operands_in_order BAIL: operand {i} untypeable: {:?} logical={:?}",
                        ir.exprs[x as usize],
                        ir.logical_types.get(&x)
                    );
                    return None;
                };
                snapshot_ty[i] = Some(ty);
            }
        }
    }
    // Rewrite phase: hoist each operand's suspensions, then materialize the planned snapshots so
    // every effect lands in the prelude in source order.
    let mut out = Vec::with_capacity(operands.len());
    for (i, o) in operands.iter().enumerate() {
        let Some(x) = *o else {
            out.push(None);
            continue;
        };
        let mut nx = hoist_expr(ir, x, suspend_set, orig_rets, value_types, prelude);
        if let Some(ty) = snapshot_ty[i] {
            let tmp = max_value_index(ir) + 1;
            let bound = ir.add_expr(IrExpr::Variable {
                index: tmp,
                ty,
                init: Some(nx),
                named: false,
            });
            value_types.insert(tmp, ty);
            prelude.push(bound);
            nx = ir.add_expr(IrExpr::GetValue(tmp));
        }
        out.push(Some(nx));
    }
    Some(out)
}

/// Whether evaluating an operand before a later suspension must be materialized. Literal values and a
/// `Null`-typed local are the only values allowed to commute: the latter's semantic domain is the
/// singleton `null`, and the state machine deliberately rematerializes it rather than treating it as an
/// ordinary JVM local spill. Every other runtime read or evaluation snapshots at its source position. A
/// plain `GetValue` is not intrinsically stable because an inline-spliced later operand can write that
/// local before the residual call reads it; likewise, a static `val` read can trigger initialization and
/// is still a runtime field access. This semantic rule preserves exceptions and effects without a
/// growing list of special cases for local/module/classpath storage or wrappers.
fn operand_needs_snapshot(ir: &IrFile, expression: ExprId, value_types: &HashMap<u32, Ty>) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::Const(_) => false,
        IrExpr::GetValue(local) => value_types
            .get(local)
            .is_none_or(|ty| !is_rematerialized_null(ty)),
        _ => true,
    }
}

/// The semantic JVM value type needed when suspend normalization materializes an already-evaluated
/// expression into a temporary. This is deliberately an IR-identity query: it reads the selected
/// callee/field/type node and never re-resolves a source name. It covers the common runtime-read and
/// expression shapes that can precede a suspension in any ordered operand list; returning `None` makes
/// an unexpected lowering shape decline safely instead of emitting a temp with a guessed verifier type.
fn hoisted_value_ty(
    ir: &IrFile,
    expression: ExprId,
    orig_rets: &[Ty],
    value_types: &HashMap<u32, Ty>,
) -> Option<Ty> {
    match &ir.exprs[expression as usize] {
        IrExpr::Const(constant) => Some(match constant {
            IrConst::Boolean(_) => Ty::Boolean,
            IrConst::UByte(_) => Ty::UByte,
            IrConst::UShort(_) => Ty::UShort,
            IrConst::UInt(_) => Ty::UInt,
            IrConst::ULong(_) => Ty::ULong,
            IrConst::Int(_) => Ty::Int,
            IrConst::Long(_) => Ty::Long,
            IrConst::Double(_) => Ty::Double,
            IrConst::Float(_) => Ty::Float,
            IrConst::Char(_) => Ty::Char,
            IrConst::String(_) => Ty::String,
            IrConst::Short(_) => Ty::Short,
            IrConst::Byte(_) => Ty::Byte,
            IrConst::Null => Ty::Null,
        }),
        // A class literal is emitted as an `ldc Class`, which is still a runtime resolution action and
        // therefore must stay before a later suspension (including any linkage failure it can raise).
        IrExpr::ClassConst { .. } => Some(Ty::obj("java/lang/Class")),
        // Value indices are local to one function. Looking through the complete arena can find a
        // declaration with the same numeric index in an unrelated method and poison the verifier type
        // of the new temp. The caller supplies the current function's parameter/local environment.
        IrExpr::GetValue(index) => value_types.get(index).copied(),
        IrExpr::Call { callee, .. } => {
            if let Some(function) = callee.source_function() {
                orig_rets.get(function as usize).copied()
            } else {
                match callee {
                    Callee::CrossFile { ret, .. }
                    | Callee::Module { ret, .. }
                    | Callee::Super { ret, .. }
                    | Callee::External { ret, .. } => Some(*ret),
                    Callee::Static { descriptor, .. } | Callee::Special { descriptor, .. } => {
                        crate::jvm::ir_emit::parse_physical_method_desc(descriptor)
                            .map(|(_, ret)| ret)
                    }
                    Callee::Virtual {
                        descriptor, params, ..
                    } => params.as_ref().map(|(_, ret)| *ret).or_else(|| {
                        crate::jvm::ir_emit::parse_physical_method_desc(descriptor)
                            .map(|(_, ret)| ret)
                    }),
                    Callee::Intrinsic { ret, .. } => Some(*ret),
                    _ => unreachable!("source-function callees were handled above"),
                }
            }
        }
        IrExpr::MethodCall { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.methods.get(*index as usize))
            .and_then(|function| orig_rets.get(*function as usize))
            .copied(),
        IrExpr::InvokeFunction { ret, .. } => Some(*ret),
        IrExpr::PrimitiveBinOp { op, lhs, .. } => Some(match op {
            IrBinOp::Lt
            | IrBinOp::Le
            | IrBinOp::Gt
            | IrBinOp::Ge
            | IrBinOp::Eq
            | IrBinOp::Ne
            | IrBinOp::RefEq
            | IrBinOp::RefNe
            | IrBinOp::And
            | IrBinOp::Or => Ty::Boolean,
            _ => hoisted_value_ty(ir, *lhs, orig_rets, value_types)?,
        }),
        IrExpr::PrimitiveNeg { ty, .. }
        | IrExpr::TypeOp {
            type_operand: ty, ..
        } => Some(*ty),
        // A constructed instance's verifier type is its class; generic arguments are erased at this
        // boundary (the temp only needs the internal name).
        IrExpr::New { internal, .. } => Some(Ty::Obj(*internal, &[])),
        // `operand!!` yields its operand's value unchanged (the assert only throws), matching
        // `value_ty`'s treatment in the emitter.
        IrExpr::BottomValue { .. } => Some(Ty::Nothing),
        IrExpr::NotNullAssert { operand, .. } => {
            hoisted_value_ty(ir, *operand, orig_rets, value_types)
        }
        // A value block is an evaluation wrapper, not a distinct value representation. Hoisting its
        // statements collapses the wrapper to the final expression, so a snapshot uses that final
        // expression's exact type. An empty/statement-only block has no value to materialize.
        IrExpr::Block {
            value: Some(value), ..
        } => hoisted_value_ty(ir, *value, orig_rets, value_types),
        // Declared storage types, read straight off the IR declaration the node indexes — the same
        // sources the emitter's `value_ty` consults. `PropertyRead` carries its type inline; a
        // `Unit`-typed property realizes through a `()V` accessor (nothing to bind) and a bare
        // type-parameter's erasure is the emitter's concern, so both decline.
        IrExpr::GetStatic(i) => Some(ir.statics[*i as usize].ty),
        // Static/singleton reads share the same semantic rule regardless of whether their declaration
        // originated in this file, another module, or the classpath. Recover their physical type from
        // the identity already stored on the IR node; the descriptor parser is shared with emission so
        // array/object/primitive handling cannot drift into a suspend-only copy.
        IrExpr::StaticInstance { ty, .. } => ir
            .classes
            .get(*ty as usize)
            .map(|class| Ty::obj(&class.fq_name())),
        IrExpr::ExternalStaticInstance { ty, .. } => Some(Ty::obj_name(*ty)),
        IrExpr::ExternalStaticField { descriptor, .. } => {
            let ty = crate::jvm::ir_emit::ty_from_field_descriptor(descriptor);
            (!matches!(ty, Ty::Unit | Ty::Error)).then_some(ty)
        }
        IrExpr::EnumEntry { classifier, .. } => Some(Ty::obj_name(*classifier)),
        IrExpr::EnumValueOf { classifier, .. } => Some(Ty::obj_name(*classifier)),
        IrExpr::EnumValues { classifier } => Some(Ty::array(Ty::obj_name(*classifier))),
        IrExpr::EnumEntries { classifier } => Some(Ty::obj_args_name(
            crate::types::type_name("kotlin/enums/EnumEntries"),
            &[Ty::obj_name(*classifier)],
        )),
        IrExpr::UnitInstance => Some(Ty::obj("kotlin/Unit")),
        IrExpr::GetField { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.fields.get(*index as usize))
            .map(|field| field.ty),
        IrExpr::EnclosingInstance { outer, .. } => Some(Ty::obj_name(*outer)),
        IrExpr::LateinitInitialized { class, index, .. } => ir
            .classes
            .get(*class as usize)
            .and_then(|class| class.fields.get(*index as usize))
            .map(|field| field.ty),
        IrExpr::PropertyRead { ty, .. } => {
            (!matches!(ty, Ty::Unit | Ty::Error | Ty::TyParam(..))).then_some(*ty)
        }
        IrExpr::RefGet { elem, .. } => Some(*elem),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrIntrinsicSuspensionKind, IrIntrinsicSuspensionPoint};

    #[test]
    fn a_shared_operand_is_hoisted_independently_at_each_use() {
        let mut ir = IrFile::default();
        let suspension = ir.add_expr(IrExpr::UnitInstance);
        ir.intrinsic_suspension_points.insert(
            suspension,
            IrIntrinsicSuspensionPoint {
                result: Ty::String,
                kind: IrIntrinsicSuspensionKind::Safe,
            },
        );
        let shared = ir.add_expr(IrExpr::EnumValueOf {
            classifier: type_name("example/Level"),
            arg: suspension,
            declaration: crate::ir::EnumValueOfDeclaration::Member,
        });
        let concat = ir.add_expr(IrExpr::StringConcat(vec![shared, shared]));
        let returned = ir.add_expr(IrExpr::Return(Some(concat)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });

        crate::ir::make_expression_children_unique(&mut ir, body);
        hoist_suspensions(&mut ir, body, &HashSet::new(), &[], &mut HashMap::new());

        let IrExpr::Block { stmts, .. } = &ir.exprs[body as usize] else {
            panic!("hoisting must retain the function body block")
        };
        let suspension_bindings = stmts
            .iter()
            .filter(|&&statement| {
                let IrExpr::Variable {
                    init: Some(initializer),
                    ..
                } = ir.exprs[statement as usize]
                else {
                    return false;
                };
                ir.intrinsic_suspension_points.contains_key(&initializer)
            })
            .count();
        assert_eq!(
            suspension_bindings, 2,
            "each reference to a shared expression is a distinct evaluation"
        );
    }

    #[test]
    fn every_remaining_single_operand_node_recurses() {
        for shape in ["ref-get", "enclosing-instance", "lateinit-initialized"] {
            let mut ir = IrFile::default();
            let suspension = ir.add_expr(IrExpr::UnitInstance);
            ir.intrinsic_suspension_points.insert(
                suspension,
                IrIntrinsicSuspensionPoint {
                    result: Ty::String,
                    kind: IrIntrinsicSuspensionKind::Safe,
                },
            );
            let expression = match shape {
                "ref-get" => IrExpr::RefGet {
                    holder: suspension,
                    elem: Ty::String,
                },
                "enclosing-instance" => IrExpr::EnclosingInstance {
                    receiver: suspension,
                    inner: type_name("example/Outer$Inner"),
                    outer: type_name("example/Outer"),
                },
                "lateinit-initialized" => IrExpr::LateinitInitialized {
                    receiver: suspension,
                    class: 0,
                    index: 0,
                },
                _ => unreachable!(),
            };
            let expression = ir.add_expr(expression);
            let returned = ir.add_expr(IrExpr::Return(Some(expression)));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![returned],
                value: None,
            });

            crate::ir::make_expression_children_unique(&mut ir, body);
            hoist_suspensions(&mut ir, body, &HashSet::new(), &[], &mut HashMap::new());

            let IrExpr::Block { stmts, .. } = &ir.exprs[body as usize] else {
                panic!("hoisting must retain the function body block")
            };
            assert_eq!(
                stmts
                    .iter()
                    .filter(|&&statement| {
                        matches!(
                            ir.exprs[statement as usize],
                            IrExpr::Variable { init: Some(initializer), .. }
                                if ir.intrinsic_suspension_points.contains_key(&initializer)
                        )
                    })
                    .count(),
                1,
                "{shape} must expose its unconditional suspending operand"
            );
        }
    }
}
