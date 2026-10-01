//! Checked integral-constant widths preserved by common FIR lowering.

use super::*;

/// A constant's CHECKED TYPE is what says which integral type it is.
///
/// `FirConstant` has no signed case narrower than `Int` and no `UByte`/`UShort` case, so the type is
/// the only thing carrying those widths. Recording a `Byte`-typed constant as an `Int` loses it, and
/// a consumer that reads a constant's SHAPE rather than the type it is assigned to then gets the
/// wrong width — a box made from a `Byte` constant came out an `Int`, so an `is Byte` test answered
/// false and two equal bytes compared unequal once boxed.
#[test]
fn a_narrow_integral_constant_keeps_the_width_named_by_its_checked_type() {
    for (ty, expected) in [
        (Ty::Byte, IrConst::Byte(-128)),
        (Ty::Short, IrConst::Short(-128)),
        (Ty::Int, IrConst::Int(-128)),
        (Ty::nullable(Ty::Byte), IrConst::Byte(-128)),
    ] {
        assert_eq!(
            lowered_int_constant(-128, ty),
            expected,
            "a constant checked as `{ty:?}`"
        );
    }
}

/// A value the named width cannot hold is a lowering FAILURE, not a truncation. The checker does
/// not produce one; stating it here is what keeps a later widening of this path from silently
/// wrapping.
#[test]
fn an_integral_constant_too_wide_for_its_checked_type_fails() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(0));
    let constant = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Byte),
        kind: FirExprKind::Constant(FirConstant::Int(128)),
    });
    let target = body.allocate_local_value();
    let local = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target,
            ty: resolved(Ty::Byte),
            mutable: false,
            lateinit: false,
            deferred: false,
            initializer: Some(constant),
            conversion: None,
        },
    });
    body.push_root(local);

    let mut ir = IrFile::default();
    assert!(lower_body(body, &ResolvedModuleIndex::default(), &mut ir).is_err());
}

/// Lower one `FirConstant::Int` checked at `ty`, and answer the constant common IR recorded.
fn lowered_int_constant(value: i64, ty: Ty) -> IrConst {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(0));
    let constant = body.add_expr(FirExpr {
        origin,
        ty: resolved(ty),
        kind: FirExprKind::Constant(FirConstant::Int(value)),
    });
    let target = body.allocate_local_value();
    let local = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target,
            ty: resolved(ty),
            mutable: false,
            lateinit: false,
            deferred: false,
            initializer: Some(constant),
            conversion: None,
        },
    });
    body.push_root(local);

    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    match ir.expr(0) {
        IrExpr::Const(constant) => constant.clone(),
        other => panic!("expected a constant, got {other:?}"),
    }
}
