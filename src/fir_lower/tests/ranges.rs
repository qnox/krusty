//! Checked range loops and membership at the common FIR-to-IR boundary.

use super::{lower_body, lower_single_source, resolved};
use crate::fir::{
    BodyOwnerId, FirBody, FirConstant, FirExpr, FirExprKind, FirRangeOperation, FirStatement,
    FirStatementKind, OriginId, ResolvedModuleIndex,
};
use crate::ir::{IrCheckedOperation, IrConst, IrExpr, IrFile};
use crate::types::Ty;

#[test]
fn literal_until_loop_stays_checked_until_backend_realization() {
    let ir = lower_single_source(
        "fun sum(): Int { var sum = 0; for (value in 0 until 10) sum += value; return sum }\n",
        "LiteralUntilLoop",
    );
    let loops = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::RangeLoop {
                    variable,
                    variable_name,
                    counter,
                    source:
                        crate::ir::IrProgressionSource::Literal {
                            operation,
                            start,
                            end,
                        },
                    unsigned_compare: None,
                    body,
                    label,
                    with_index: None,
                } = &**checked_op =>
            {
                Some((
                    *variable,
                    variable_name.as_deref(),
                    *counter,
                    *operation,
                    *start,
                    *end,
                    *body,
                    label.as_str(),
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(loops.len(), 1);
    let (variable, name, counter, operation, start, end, body, label) = loops[0];
    assert_eq!(name, Some("value"));
    assert_eq!(counter, Ty::Int);
    assert_eq!(operation, FirRangeOperation::Until);
    assert!(matches!(ir.expr(start), IrExpr::Const(IrConst::Int(0))));
    assert!(matches!(ir.expr(end), IrExpr::Const(IrConst::Int(10))));
    assert_eq!(label, "$fir_control_0_1");
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        panic!("range-loop body must retain its checked statement block")
    };
    assert_eq!(stmts.len(), 1);
    assert!(matches!(ir.expr(stmts[0]), IrExpr::SetValue { .. }));
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::While { .. }))
            .count(),
        0
    );
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::Variable { index, .. } if *index == variable))
            .count(),
        0
    );
}

#[test]
fn range_loop_records_an_exact_type_stable_bound_read() {
    let ir = lower_single_source(
        "fun sum(limit: Int): Int { var sum = 0; for (value in 0 until limit) sum += value; return sum }\n",
        "StableRangeBound",
    );
    let loops = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::RangeLoop {
                    source: crate::ir::IrProgressionSource::Literal { end, .. },
                    ..
                } = &**checked_op =>
            {
                Some(*end)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(loops.len(), 1);
    let end = loops[0];
    assert!(matches!(ir.expr(end), IrExpr::GetValue(0)));
    assert_eq!(
        ir.binding_read_stability.get(&end),
        Some(&crate::ir::IrBindingStability::Stable)
    );
    assert_eq!(ir.logical_type(end), Some(Ty::Int));
}

#[test]
fn unsigned_range_containment_stays_semantic_until_backend_realization() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(12));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(2)),
    });
    let start = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(1)),
    });
    let end = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::UInt),
        kind: FirExprKind::Constant(FirConstant::UInt(3)),
    });
    let contains = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Boolean),
        kind: FirExprKind::InRange {
            operation: FirRangeOperation::Through,
            provenance: crate::fir::FirRangeComparisonProvenance::RangeConstruction {
                open_end: false,
            },
            comparison: resolved(Ty::UInt),
            unsigned_compare: None,
            value,
            start,
            end,
            negated: false,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(contains),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Checked(checked_op) if matches!(&**checked_op, IrCheckedOperation::RangeContains {
            operation: FirRangeOperation::Through,
            counter: Ty::UInt,
            negated: false,
            ..
        })
    ));
}
