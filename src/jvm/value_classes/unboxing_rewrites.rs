//! Rewrites that read a boxed value class where its carrier is wanted.
//!
//! Each replaces an expression in place with the same value taken out of its box: a checked
//! `unbox-impl` call, the null-preserving form of it for a nullable carrier, or a narrowing cast
//! to the box alone.

use super::*;

/// Add the selected virtual unbox operation and record its exact type-operation receiver edge.
pub(super) fn unbox_call(
    ir: &mut IrFile,
    receiver: ExprId,
    owner: TypeName,
    carrier: &Ty,
) -> ExprId {
    let call = ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){}", desc(carrier)),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        },
        dispatch_receiver: Some(receiver),
        args: vec![],
    });
    super::type_operation_roles::record_unbox_type_operation_edge(ir, call, receiver);
    call
}

/// Replace the expr at `id` with `(X)<orig>.unbox-impl()` — checkcast then unbox a boxed `X`.
pub(super) fn unbox_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let cast = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg: new_id,
        type_operand: boxed_value_ty(x),
    });
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        },
        dispatch_receiver: Some(cast),
        args: vec![],
    };
    super::type_operation_roles::record_unbox_type_operation_edge(ir, id, cast);
    // `id` now denotes the `unbox-impl` result (here and in `unbox_wrap_nullable`), so its physical
    // fact changes with the node rather than still claim the erased reference or the box.
    ir.physical_types.insert(id, u);
}

/// Replace an erased-reference expression with an explicit cast to its known boxed value class.
pub(super) fn narrow_wrap(ir: &mut IrFile, id: ExprId, x: TypeName) {
    let arg = clone_below_representation_wrapper(ir, id);
    ir.exprs[id as usize] = IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg,
        type_operand: boxed_value_ty(x),
    };
}

pub(super) fn unbox_wrap_nullable(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    slot: u32,
) {
    let orig_id = clone_below_representation_wrapper(ir, id);
    let boxed_ty = Ty::nullable(Ty::obj_name(x));
    let var = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Variable {
        index: slot,
        ty: boxed_ty,
        init: Some(orig_id),
        named: false,
    });
    let get_for_test = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let null1 = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Const(crate::ir::IrConst::Null));
    let is_null = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Eq,
        lhs: get_for_test,
        rhs: null1,
    });
    let null2 = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Const(crate::ir::IrConst::Null));
    let get_for_unbox = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    let unboxed = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Call {
        callee: Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        },
        dispatch_receiver: Some(get_for_unbox),
        args: vec![],
    });
    let when = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::When {
        branches: vec![(Some(is_null), null2), (None, unboxed)],
    });
    ir.null_guards.insert(when);
    ir.exprs[id as usize] = IrExpr::Block {
        stmts: vec![var],
        value: Some(when),
    };
    ir.physical_types.insert(id, Ty::nullable(u));
}
