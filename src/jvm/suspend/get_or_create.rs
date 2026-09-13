//! Construction of a named suspend function's reusable-continuation prologue.

use super::{max_value_index, I32_MIN};
use crate::ir::{ClassId, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Build `$completion instanceof Cont && (label & MIN_VALUE) != 0` ⇒ reuse the continuation
/// (clearing the resume bit), else `new Cont($completion)`. Nested `when`s preserve short-circuiting:
/// the cast/getfield must not run when `$completion` is not our continuation type.
pub(super) fn build_get_or_create(
    ir: &mut IrFile,
    completion_idx: u32,
    cont_ty: &Ty,
    cont_id: ClassId,
    receiver_this: Option<u32>,
    param_caps: &[(u32, u32)],
) -> ExprId {
    let k = |ir: &mut IrFile, expression: IrExpr| ir.add_expr(expression);
    let cast = |ir: &mut IrFile| {
        let completion = ir.add_expr(IrExpr::GetValue(completion_idx));
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: completion,
            type_operand: *cont_ty,
        })
    };
    let label_of = |ir: &mut IrFile, receiver: ExprId| {
        ir.add_expr(IrExpr::GetField {
            receiver,
            class: cont_id,
            index: 1,
        })
    };
    // A fresh continuation captures live value parameters before the dispatch loop's first restore.
    // A resumed continuation keeps its previously saved fields.
    let new_cont = |ir: &mut IrFile| {
        let mut args = Vec::new();
        if let Some(this_idx) = receiver_this {
            args.push(ir.add_expr(IrExpr::GetValue(this_idx)));
        }
        args.push(ir.add_expr(IrExpr::GetValue(completion_idx)));
        let cont_internal = ir.classes[cont_id as usize].fq_name_id();
        let new = ir.add_expr(IrExpr::New {
            internal: cont_internal,
            args,
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        if param_caps.is_empty() {
            return new;
        }
        let temp = max_value_index(ir) + 1;
        let mut statements = vec![ir.add_expr(IrExpr::Variable {
            index: temp,
            ty: *cont_ty,
            init: Some(new),
            named: false,
        })];
        for &(local, field) in param_caps {
            let receiver = ir.add_expr(IrExpr::GetValue(temp));
            let value = ir.add_expr(IrExpr::GetValue(local));
            statements.push(ir.add_expr(IrExpr::SetField {
                receiver,
                class: cont_id,
                index: field,
                value,
            }));
        }
        let value = ir.add_expr(IrExpr::GetValue(temp));
        ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(value),
        })
    };

    let completion = k(ir, IrExpr::GetValue(completion_idx));
    let is_instance = k(
        ir,
        IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: completion,
            type_operand: *cont_ty,
        },
    );
    // Cast once inside the `instanceof` branch; every subsequent read uses this binding.
    let reused = max_value_index(ir) + 1;
    let cast_once = cast(ir);
    let bind_reused = k(
        ir,
        IrExpr::Variable {
            index: reused,
            ty: *cont_ty,
            init: Some(cast_once),
            named: false,
        },
    );
    let reused_value = |ir: &mut IrFile| ir.add_expr(IrExpr::GetValue(reused));

    let reused_for_label = reused_value(ir);
    let label = label_of(ir, reused_for_label);
    let min_value = k(ir, IrExpr::Const(IrConst::Int(I32_MIN)));
    let masked = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::BitAnd,
            lhs: label,
            rhs: min_value,
        },
    );
    let zero = k(ir, IrExpr::Const(IrConst::Int(0)));
    let resume_bit_set = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Ne,
            lhs: masked,
            rhs: zero,
        },
    );

    // `cont.label -= MIN_VALUE; cont`.
    let receiver = reused_value(ir);
    let reused_for_old_label = reused_value(ir);
    let old_label = label_of(ir, reused_for_old_label);
    let min_value = k(ir, IrExpr::Const(IrConst::Int(I32_MIN)));
    let new_label = k(
        ir,
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Sub,
            lhs: old_label,
            rhs: min_value,
        },
    );
    let update_label = k(
        ir,
        IrExpr::SetField {
            receiver,
            class: cont_id,
            index: 1,
            value: new_label,
        },
    );
    let reused_result = reused_value(ir);
    let reuse = k(
        ir,
        IrExpr::Block {
            stmts: vec![update_label],
            value: Some(reused_result),
        },
    );
    let new_when_bit_clear = new_cont(ir);
    let inside_instance = k(
        ir,
        IrExpr::When {
            branches: vec![(Some(resume_bit_set), reuse), (None, new_when_bit_clear)],
        },
    );
    let instance_branch = k(
        ir,
        IrExpr::Block {
            stmts: vec![bind_reused],
            value: Some(inside_instance),
        },
    );
    let new_when_not_instance = new_cont(ir);
    k(
        ir,
        IrExpr::When {
            branches: vec![
                (Some(is_instance), instance_branch),
                (None, new_when_not_instance),
            ],
        },
    )
}
