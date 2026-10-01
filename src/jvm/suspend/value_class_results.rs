//! A value-class result that crosses a suspension as its carrier
//! ([`crate::ir::IrValueClassSuspendResult::Carrier`]). The function returns the carrier where it
//! does not suspend, and its continuation hands the value to the completion as its box, typed
//! `Any?`, as kotlinc does. A caller resumed with the value therefore unboxes it; the IR machine,
//! whose direct path re-enters the state its resume takes, boxes the direct result first.

use crate::ir::{
    Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp, IrValueClassSuspendResult,
};
use crate::types::{Ty, TypeName};
use std::collections::HashSet;

/// The value class and the carrier it unboxes to, when `result` crosses as that carrier. A nullable
/// value class whose carrier cannot hold `null` crosses as its own box, which needs neither.
pub(super) fn unboxed_carrier(result: Option<IrValueClassSuspendResult>) -> Option<(TypeName, Ty)> {
    match result? {
        IrValueClassSuspendResult::Carrier {
            classifier,
            carrier,
        } if carrier.non_null().obj_internal() != Some(classifier) => Some((classifier, carrier)),
        _ => None,
    }
}

/// The value class and carrier of a suspend call whose callee's continuation completes with the box:
/// a callee that returns the carrier, in this module or a dependency (a `$default` stub included).
/// A call to a generic callee is recorded as receiving the box on either path, which its call-site
/// representation already consumes.
pub(super) fn boxed_on_resume(
    ir: &IrFile,
    call: ExprId,
    suspend_functions: &HashSet<u32>,
) -> Option<(TypeName, Ty)> {
    unboxed_carrier(super::value_class_suspension_result(
        ir,
        call,
        suspend_functions,
    ))
}

/// The carrier of the boxed value `resumed` reads: `checkcast X; unbox-impl`, with the `null` of a
/// nullable carrier passing through, since `unbox-impl` is an instance call.
pub(super) fn resumed_carrier(
    ir: &mut IrFile,
    resumed: ExprId,
    classifier: TypeName,
    carrier: Ty,
) -> ExprId {
    let physical = crate::jvm::physical_type::ir_ty_to_jvm(&carrier);
    let descriptor = format!(
        "(){}",
        crate::jvm::names::type_descriptor(physical.non_null())
    );
    let boxed = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: resumed,
        type_operand: Ty::nullable(Ty::obj_name(classifier)),
    });
    let unboxed = ir.add_expr(IrExpr::Call {
        callee: Callee::realized_virtual(
            classifier,
            "unbox-impl".to_string(),
            descriptor,
            None,
            false,
        ),
        dispatch_receiver: Some(boxed),
        args: Vec::new(),
    });
    if !carrier.is_nullable() {
        return unboxed;
    }
    let tested = ir.add_expr(ir.exprs[resumed as usize].clone());
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: tested,
        rhs: null,
    });
    let passed = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(is_null), passed), (None, unboxed)],
    })
}

/// The box of the carrier `value` holds: `X.box-impl`, with the `null` of a nullable carrier
/// passing through, since `box-impl` rejects it.
pub(super) fn boxed_carrier(
    ir: &mut IrFile,
    value: ExprId,
    classifier: TypeName,
    carrier: Ty,
) -> ExprId {
    let physical = crate::jvm::physical_type::ir_ty_to_jvm(&carrier);
    let owner = classifier.render();
    let descriptor = format!(
        "({})L{owner};",
        crate::jvm::names::type_descriptor(physical.non_null())
    );
    let cast = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: value,
        type_operand: carrier,
    });
    let boxed = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: classifier,
            name: "box-impl".to_string(),
            descriptor,
            inline: crate::libraries::InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![cast],
    });
    if !carrier.is_nullable() {
        return boxed;
    }
    let tested = ir.add_expr(ir.exprs[value as usize].clone());
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let is_null = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: tested,
        rhs: null,
    });
    let passed = ir.add_expr(IrExpr::Const(IrConst::Null));
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(is_null), passed), (None, boxed)],
    })
}
