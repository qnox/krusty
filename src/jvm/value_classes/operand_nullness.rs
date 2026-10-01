//! Whether a checked value is statically non-null or null-only, read from the IR node and the
//! declared slot, field and return types the frontend fixed. Boxing a value class picks
//! `box-impl` or its null-guarded form from these facts.

use crate::ir::{Callee, ExprId, IrExpr};
use crate::types::Ty;
use std::collections::HashMap;

/// Whether the value the expr at `id` produces is statically NON-NULL — so boxing it (`box-impl`) can't
/// hit the value class's non-null ctor check. A construction/`!!`/non-nullable slot or return qualifies.
pub(super) fn operand_nonnull(
    exprs: &[IrExpr],
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    id: ExprId,
) -> bool {
    let non_null_ty = |t: &Ty| matches!(t, Ty::Obj(..));
    match &exprs[id as usize] {
        // An array allocation is a fresh non-null reference. Treating `newarray` as nullable
        // dropped the `box-impl` a null-safe coercion to a supertype has to emit.
        IrExpr::New { .. } | IrExpr::NewArray { .. } | IrExpr::Vararg { .. } => true,
        // A read of a non-nullable field yields a non-null value (a `val a: X` data-class property is
        // never null — box it with the plain `box-impl`, no null guard).
        IrExpr::GetField { class, index, .. } => fields
            .get(*class as usize)
            .and_then(|fs| fs.get(*index as usize))
            .is_some_and(non_null_ty),
        // The same read as a property: its declared type is what says whether the value can be null.
        IrExpr::PropertyRead { ty, .. } => non_null_ty(ty),
        IrExpr::NotNullAssert { .. } => true,
        // A successful cast to a non-null reference has a non-null result; `CastNonNull` states the
        // same contract directly. This matters for a non-null generic value class whose unboxed
        // carrier itself may contain null (`Ag<T>(null)` is still not a null `Ag<T>`).
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            type_operand,
            ..
        } => non_null_ty(type_operand),
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::CastNonNull,
            ..
        } => true,
        // A checked coercion to a non-null type has that type: a generic `T` slot read as `R<Int>`
        // holds an `R`, never a null reference, even when `R`'s own carrier admits null.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } => non_null_ty(type_operand),
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name == "constructor-impl" || name == "box-impl" => true,
        IrExpr::Call { callee, .. } if callee.source_function().is_some() => rets
            .get(
                callee
                    .source_function()
                    .expect("guarded same-file function call") as usize,
            )
            .is_some_and(non_null_ty),
        IrExpr::GetValue(i) => slots.get(i).is_some_and(non_null_ty),
        IrExpr::Block { value: Some(v), .. } => operand_nonnull(exprs, rets, fields, slots, *v),
        _ => false,
    }
}

/// Whether a checked value has Kotlin's null-only bottom type. This is representation evidence,
/// not data-flow inference: it follows only common-IR-transparent wrappers and declared slot/call
/// result types already fixed by the frontend.
pub(super) fn operand_null_only(
    exprs: &[IrExpr],
    rets: &[Ty],
    slots: &HashMap<u32, Ty>,
    id: ExprId,
) -> bool {
    let null_only_ty = |ty: &Ty| *ty == Ty::Null || ty.non_null() == Ty::Nothing;
    match &exprs[id as usize] {
        IrExpr::Const(crate::ir::IrConst::Null) => true,
        IrExpr::GetValue(slot) => slots.get(slot).is_some_and(null_only_ty),
        IrExpr::Call { callee, .. } if callee.source_function().is_some() => rets
            .get(callee.source_function().expect("guarded source function") as usize)
            .is_some_and(null_only_ty),
        IrExpr::TypeOp { arg, .. } => operand_null_only(exprs, rets, slots, *arg),
        IrExpr::Block {
            value: Some(value), ..
        } => operand_null_only(exprs, rets, slots, *value),
        _ => false,
    }
}
