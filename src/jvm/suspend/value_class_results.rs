//! A value-class result that crosses a suspension as its carrier
//! ([`crate::ir::IrValueClassSuspendResult::Carrier`]). The function returns the carrier where it
//! does not suspend, and its continuation hands the value to the completion as its box, typed
//! `Any?`, as kotlinc does. A direct caller unboxes that box. A caller of a suspend function value
//! does not: `invoke` already returned the box, and the use lowered against that call unboxes it.
//! The IR machine re-enters the state its resume takes, so a direct synchronous result is boxed
//! first and a function value's box is stored as it is.

use crate::fx_hash::HashSet;
use crate::ir::{
    Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp, IrValueClassSuspendResult,
};
use crate::types::{Ty, TypeName};

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

/// The value class and carrier of suspend function `function`, when it returns the carrier where it
/// does not suspend: its continuation completes with the box. With them, whether the value class
/// may be null ([`box_may_be_null`]), whose null carrier completes as `null`.
pub(crate) fn completion_box(ir: &IrFile, function: u32) -> Option<(TypeName, Ty, bool)> {
    let (value_class, carrier) =
        unboxed_carrier(ir.value_class_suspend_returns.get(&function).copied())?;
    Some((
        value_class,
        carrier,
        box_may_be_null(ir, value_class, carrier),
    ))
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

/// The value class and carrier of suspension point `call` when its resumption delivers the box
/// where a call that did not suspend returns the carrier: a direct call to a callee that returns
/// the carrier ([`boxed_on_resume`]). A suspend function value's `invoke` returns the box on both
/// paths, which the use lowered against that call unboxes.
pub(super) fn resumed_box(
    ir: &IrFile,
    call: ExprId,
    suspend_functions: &HashSet<u32>,
) -> Option<(TypeName, Ty)> {
    match ir.exprs[call as usize] {
        IrExpr::InvokeFunction { .. } => None,
        _ => boxed_on_resume(ir, call, suspend_functions),
    }
}

/// Whether the box of `value_class` that crosses a suspension as `carrier` may be null: the declared
/// value class is nullable, and its `null` is the carrier's, which boxing and unboxing pass on
/// rather than calling `box-impl` or `unbox-impl`. A nullable carrier shows that, unless the
/// underlying type is nullable itself: such a value class is never null, and its null carrier is
/// the box of `X(null)`.
pub(crate) fn box_may_be_null(ir: &IrFile, value_class: TypeName, carrier: Ty) -> bool {
    carrier.is_nullable()
        && !ir
            .value_class_underlying_name(value_class)
            .expect("a carrier result's value class has its underlying type")
            .is_nullable()
}

/// The carrier of the boxed value `resumed` reads: `checkcast X; unbox-impl`, with the `null` of a
/// nullable carrier passing through, since `unbox-impl` is an instance call. A direct call needs
/// this. A suspend function value does not: its `invoke` already returned the box, and the use
/// lowered against that call is the unbox.
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
        callee: Box::new(Callee::realized_virtual(
            classifier,
            "unbox-impl".to_string(),
            descriptor,
            None,
            false,
        )),
        dispatch_receiver: Some(boxed),
        args: Vec::new(),
    });
    if !box_may_be_null(ir, classifier, carrier) {
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
        callee: Box::new(Callee::Static {
            owner: classifier,
            name: "box-impl".to_string(),
            descriptor,
            inline: crate::libraries::InlineKind::None,
        }),
        dispatch_receiver: None,
        args: vec![cast],
    });
    if !box_may_be_null(ir, classifier, carrier) {
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
