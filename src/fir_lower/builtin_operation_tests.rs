//! Lowering of checked builtin operations: an operation over runtime operands stays an operation,
//! and one whose operands are all constants lowers to its value, as kotlinc's
//! `ConstEvaluationLowering` folds it.

use super::lower_body;
use super::tests::resolved;
use crate::fir::{
    BodyOwnerId, FirBinaryOperation, FirBody, FirConstant, FirExpr, FirExprKind, FirStatement,
    FirStatementKind, FirUnaryOperation, OriginId, ResolvedModuleIndex,
};
use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrNodeOrigin};
use crate::types::Ty;

#[test]
fn consuming_lowering_materializes_common_ir_roots() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let operand_value = body.allocate_local_value();
    let local_value = body.allocate_local_value();
    // The operand is a local read: an operation over constants alone would fold to its value.
    let initial = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(1)),
    });
    let operand = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: operand_value,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(initial),
            conversion: None,
        },
    });
    body.push_root(operand);
    let one = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::ValueRead(operand_value),
    });
    let two = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(2)),
    });
    let sum = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Binary {
            operation: FirBinaryOperation::Add,
            lhs: one,
            rhs: two,
        },
    });
    let local = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: local_value,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(sum),
            conversion: None,
        },
    });
    body.push_root(local);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    assert_eq!(lowered.owner, BodyOwnerId::from_raw(7));
    assert_eq!(lowered.owner, BodyOwnerId::from_raw(7));
    assert_eq!(lowered.roots.as_ref(), &[1, 5]);
    assert!(matches!(ir.expr(2), IrExpr::GetValue(0)));
    assert!(matches!(ir.expr(3), IrExpr::Const(IrConst::Int(2))));
    assert!(matches!(
        ir.expr(4),
        IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: 2,
            rhs: 3
        }
    ));
    assert!(matches!(
        ir.expr(5),
        IrExpr::Variable {
            index: 1,
            ty: Ty::Int,
            init: Some(4),
            named: true
        }
    ));
    assert!(ir.folded_constants.is_empty());
    assert_eq!(ir.fir_origins.len(), ir.exprs.len());
    assert_eq!(ir.fir_origins.get(&5), Some(&IrNodeOrigin::Fir(origin)));
}

#[test]
fn an_operation_over_constants_lowers_to_its_value() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let local_value = body.allocate_local_value();
    let one = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(1)),
    });
    let two = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(2)),
    });
    let sum = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Binary {
            operation: FirBinaryOperation::Add,
            lhs: one,
            rhs: two,
        },
    });
    let local = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: local_value,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(sum),
            conversion: None,
        },
    });
    body.push_root(local);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();

    assert_eq!(lowered.owner, BodyOwnerId::from_raw(7));
    // `1 + 2` is an operation over constants, which folds to its value before its operands lower.
    assert_eq!(lowered.roots.as_ref(), &[1]);
    assert!(matches!(ir.expr(0), IrExpr::Const(IrConst::Int(3))));
    assert!(ir.folded_constants.contains(&0));
    assert!(matches!(
        ir.expr(1),
        IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(0),
            named: true
        }
    ));
    assert_eq!(ir.fir_origins.len(), ir.exprs.len());
    assert_eq!(ir.fir_origins.get(&1), Some(&IrNodeOrigin::Fir(origin)));
}

#[test]
fn checked_increment_lowers_without_recovering_an_operator() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(2));
    // A local read, not a constant: an increment of a constant folds to its value.
    let local = body.allocate_local_value();
    let initial = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(41)),
    });
    let declaration = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Local {
            target: local,
            ty: resolved(Ty::Int),
            mutable: false,
            lateinit: false,
            initializer: Some(initial),
            conversion: None,
        },
    });
    body.push_root(declaration);
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::ValueRead(local),
    });
    let increment = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Unary {
            operation: FirUnaryOperation::Increment,
            operand: value,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(increment),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[1]),
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            type_operand: Ty::Int,
            ..
        }
    ));
}
