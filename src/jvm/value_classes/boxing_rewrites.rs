//! Rewrites that put an unboxed value class where its box or its text is wanted.
//!
//! Each replaces an expression in place with the same value boxed: a `box-impl` call, the
//! null-preserving form of it for a nullable carrier, or the class's `toString-impl` rendering.

use super::*;

/// Replace the unboxed value at `id` with `X.toString-impl(value)`.
pub(super) fn to_string_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: x,
            name: "toString-impl".to_string(),
            descriptor: format!("({})Ljava/lang/String;", desc(&u)),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![new_id],
    };
    ir.physical_types.insert(id, Ty::String);
    // The part is still a value-class operand: its text is appended as that type, not as a String.
    ir.logical_types.insert(id, Ty::obj_name(x));
}

/// Replace the unboxed value at `id` with `X.box-impl(value)`.
pub(super) fn box_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    let owner_rendered = x.render();
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: x,
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{owner_rendered};"),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![new_id],
    };
    ir.physical_types.insert(id, Ty::obj_name(x));
}

/// Null-safe box: replace the expr at `id` with `{ tmp = <orig>; if (tmp == null) null else box-impl(tmp) }`
/// — boxing a nullable (reference-underlying) value class without hitting the ctor null-check on `null`.
/// A carrier recorded physically nullable (a decoded `Int?` for `X(Int)`) is held as that nullable
/// value, so the test sees its null before `box-impl` takes the primitive.
pub(super) fn box_wrap_nullable(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    slot: u32,
) {
    let nullable_carrier = ir
        .physical_types
        .get(&id)
        .is_some_and(|ty| ty.is_nullable());
    let orig_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let var = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Variable {
        index: slot,
        ty: if nullable_carrier { Ty::nullable(u) } else { u },
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
    let get_for_box = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let d = desc(&u);
    let owner_rendered = x.render();
    let boxed = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Call {
        callee: Box::new(Callee::Static {
            owner: x,
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{owner_rendered};"),
            inline: InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![get_for_box],
    });
    let when = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::When {
        branches: vec![(Some(is_null), null2), (None, boxed)],
    });
    ir.null_guards.insert(when);
    ir.jvm_value_class_null_guards.insert(when);
    ir.exprs[id as usize] = IrExpr::Block {
        stmts: vec![var],
        value: Some(when),
    };
    ir.physical_types.insert(id, Ty::obj_name(x));
}
