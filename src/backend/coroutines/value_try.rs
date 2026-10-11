//! Bind a suspending value-position `try` to a local, copying each catch's semantic type.
//!
//! Return-bound and local-bound forms share this reconstruction so a catch specialization cannot
//! drift between them. The parent state machine decides where the bound local is consumed.

use std::collections::HashSet;

use crate::ir::{Callee, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::control_flow::stmt_diverges;
use super::suspension_points::expr_calls_suspend;
use super::value_namespace::{max_value_index, zero_value};
use super::value_when::owned_nested_blocks;
use super::CoroutineRepresentation;

/// The semantic pieces of a value-position `try`, after peeling the optional result coercion that
/// must instead be applied to each selected branch. Keeping this extraction in one place ensures
/// return-bound and local-bound forms recognize exactly the same IR shapes.
pub(super) struct ValueTryParts {
    body: ExprId,
    catches: Vec<crate::ir::IrCatch>,
    finally: Option<ExprId>,
    branch_wrap: Option<ValueBranchWrap>,
}

#[derive(Clone)]
pub(super) enum ValueBranchWrap {
    TypeOp(IrTypeOp, Ty),
    /// A value-class representation wrapper a target pass already selected before this
    /// normalization. It is a pure one-argument conversion, so applying it to each selected `try` value preserves
    /// the wrapper around the value while exposing the suspension to the state-machine normalizer.
    ValueClassBox(Callee),
}

pub(super) fn bind_value_try_to_fresh_local(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    expression: ExprId,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> Option<(ExprId, ExprId, ExprId)> {
    let parts = suspending_value_try(ir, expression, suspend_set)?;
    let target = max_value_index(ir) + 1;
    let dflt = zero_value(ir, representation, ty);
    let declaration = ir.add_expr(IrExpr::Variable {
        index: target,
        ty: *ty,
        init: Some(dflt),
        named: false,
    });
    // Publish the declaration before rewriting branches: a suspending branch may allocate another
    // temporary via `max_value_index`, which must not reuse the result slot.
    let value_try = bind_value_try_to_local(ir, representation, parts, target, ty, suspend_set);
    let value = ir.add_expr(IrExpr::GetValue(target));
    Some((declaration, value_try, value))
}

/// Recognize the one semantic value-`try` shape supported by the state-machine desugar. The source
/// context (`return` versus a local initializer) is intentionally absent: both consumers must peel
/// the same optional result coercion and apply the same suspension predicate.
pub(super) fn suspending_value_try(
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
pub(super) fn bind_value_try_to_local(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    parts: ValueTryParts,
    target: u32,
    ty: &Ty,
    suspend_set: &HashSet<u32>,
) -> ExprId {
    let new_body = assign_branch_to_tmp(
        ir,
        representation,
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
            ty: catch.ty,
            line: catch.line,
            body: assign_branch_to_tmp(
                ir,
                representation,
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

/// Rewrite a `try`/`catch` branch into a value-LESS block that runs its statements and assigns its VALUE
/// to `tmp`. A suspending value is bound to a fresh `Variable` (so the flattener handles the suspension),
/// then copied to `tmp`; a non-suspending value is assigned directly. A branch with no value (a divergent
/// `return`/`throw`) is left unchanged.
pub(super) fn assign_branch_to_tmp(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
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
                bind_value_try_to_fresh_local(ir, representation, v, ty, suspend_set)
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
/// Desugar a VALUE-position `try` whose body suspends into a STATEMENT-position one binding a temp, so the
/// flattener (which models a `try` STATEMENT) can handle it: `return try { … } catch { … }` becomes
/// `var tmp = <default>; try { … tmp = <body value> } catch { … tmp = <catch value> }; return tmp`. A
/// suspending branch value is bound to a fresh `Variable` first (the flattener's `stmt_suspension` handles
/// a suspend `Variable` init, not a `SetValue`), then copied to `tmp`. Both `return <try>` and a bound
/// `val v = <try>` are rewritten (the latter targets the bound local directly), and a result COERCION
/// wrapping the `try` (`suspend fun f(): Base = try { sub() } …`) moves onto each selected branch, like
/// `desugar_value_when`'s branch wrap. A `SetValue` of a suspending `try` is left to skip the file.
pub(crate) fn desugar_value_try(
    ir: &mut IrFile,
    representation: &dyn CoroutineRepresentation,
    b: ExprId,
    suspend_set: &HashSet<u32>,
    ret_ty: &Ty,
) {
    for nested in owned_nested_blocks(ir, b) {
        desugar_value_try(ir, representation, nested, suspend_set, ret_ty);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[b as usize].clone() else {
        return;
    };
    let mut new_stmts: Vec<ExprId> = Vec::with_capacity(stmts.len() + 2);
    let mut changed = false;
    for s in stmts {
        if let IrExpr::Return(Some(e)) = ir.exprs[s as usize] {
            if let Some((decl, new_try, get)) =
                bind_value_try_to_fresh_local(ir, representation, e, ret_ty, suspend_set)
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
                let dflt = zero_value(ir, representation, &ty);
                let decl = ir.add_expr(IrExpr::Variable {
                    index,
                    ty,
                    init: Some(dflt),
                    named,
                });
                let new_try =
                    bind_value_try_to_local(ir, representation, parts, index, &ty, suspend_set);
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
                        bind_value_try_to_fresh_local(ir, representation, value, &ty, suspend_set)
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
                        bind_value_try_to_fresh_local(ir, representation, value, &ty, suspend_set)
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
                    bind_value_try_to_fresh_local(ir, representation, value, &elem, suspend_set)
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
