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
        callee: Box::new(Callee::Virtual {
            owner,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){}", desc(carrier)),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        }),
        dispatch_receiver: Some(receiver),
        args: vec![],
    });
    super::type_operation_roles::record_unbox_type_operation_edge(ir, call, receiver);
    call
}

/// Replace the expr at `id` with `(X)<orig>.unbox-impl()` — checkcast then unbox a boxed `X`.
///
/// A value that is already a cast to the box (`a as X`) is the checked box itself, so it becomes
/// the receiver as it is. A second cast would test the same box again, and the written cast would
/// no longer be the type operation the unbox reads, which for a primitive-array value class
/// (`UIntArray`) would name its carrier (`checkcast [I`) instead of the box.
pub(super) fn unbox_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let cast = if is_cast_to_box(ir, new_id, x) {
        new_id
    } else {
        ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: new_id,
            type_operand: boxed_value_ty(x),
        })
    };
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Box::new(Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        }),
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

/// Whether `id` is a checked cast to the non-null box of `x`: a written `as X` (`CastNonNull`)
/// or the success branch of a safe cast (`Cast`).
fn is_cast_to_box(ir: &IrFile, id: ExprId, x: TypeName) -> bool {
    matches!(
        ir.exprs.get(id as usize),
        Some(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull,
            type_operand,
            ..
        }) if *type_operand == boxed_value_ty(x)
    )
}

/// The chain from `id` to a safe cast's `when` and its cast branches, when `id` is that shape:
/// `when { value is X -> value as X; else -> null }`, possibly the value of the blocks that hold
/// the operand's temporary. Every branch is either `null` or a cast to the box.
fn safe_cast_branches(ir: &IrFile, id: ExprId, x: TypeName) -> Option<(Vec<ExprId>, Vec<ExprId>)> {
    let mut chain = vec![id];
    let mut at = id;
    while let Some(IrExpr::Block {
        value: Some(value), ..
    }) = ir.exprs.get(at as usize)
    {
        at = *value;
        chain.push(at);
    }
    let Some(IrExpr::When { branches }) = ir.exprs.get(at as usize) else {
        return None;
    };
    let mut casts = Vec::new();
    for &(_, value) in branches {
        match ir.exprs.get(value as usize) {
            Some(IrExpr::Const(crate::ir::IrConst::Null)) => {}
            _ if is_cast_to_box(ir, value, x) => casts.push(value),
            _ => return None,
        }
    }
    (!casts.is_empty()).then_some((chain, casts))
}

/// Replace a nullable boxed `X?` at `id` with its nullable carrier.
///
/// A safe cast (`a as? X`) is unboxed where it succeeds, as kotlinc's lowering of its `when`
/// does: the cast branch reads `unbox-impl` of the checked box and the `null` branch is already
/// the carrier's `null`. Any other value is unboxed behind a null check of a temporary.
pub(super) fn unbox_wrap_nullable(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    slot: u32,
) {
    if let Some((chain, casts)) = safe_cast_branches(ir, id, x) {
        for cast in casts {
            unbox_wrap(ir, cast, x, under);
        }
        let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
        for node in chain {
            ir.physical_types.insert(node, Ty::nullable(u));
        }
        return;
    }
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
        callee: Box::new(Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
            module_target: None,
            target: None,
        }),
        dispatch_receiver: Some(get_for_unbox),
        args: vec![],
    });
    super::type_operation_roles::record_null_safe_unbox_type_operation_edge(ir, unboxed, orig_id);
    let when = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::When {
        branches: vec![(Some(is_null), null2), (None, unboxed)],
    });
    ir.null_guards.insert(when);
    ir.jvm_value_class_null_guards.insert(when);
    ir.exprs[id as usize] = IrExpr::Block {
        stmts: vec![var],
        value: Some(when),
    };
    ir.physical_types.insert(id, Ty::nullable(u));
}
