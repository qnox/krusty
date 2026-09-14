use super::lower_body;
use crate::fir::{
    BodyOwnerId, FirBody, FirConstant, FirExpr, FirExprKind, FirStatement, FirStatementKind,
    FirTypeOperation, OriginId, ResolvedModuleIndex, ResolvedTy,
};
use crate::ir::{IrBottomValueCompletion, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

#[test]
fn null_assertion_publishes_its_bottom_completion() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(0));
    let null = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(Ty::Null).unwrap(),
        kind: FirExprKind::Constant(FirConstant::Null),
    });
    let assertion = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(Ty::Nothing).unwrap(),
        kind: FirExprKind::TypeOperation {
            operation: FirTypeOperation::NotNullAssertion,
            operand: null,
            target: ResolvedTy::new(Ty::Nothing).unwrap(),
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(assertion),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let [root] = lowered.roots.as_ref() else {
        panic!("one bottom expression root expected")
    };
    let IrExpr::BottomValue {
        producer,
        completion: IrBottomValueCompletion::Diverge,
    } = ir.expr(*root)
    else {
        panic!("FIR Nothing expression must select divergent completion")
    };
    let IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg,
        type_operand: Ty::Nothing,
    } = ir.expr(*producer)
    else {
        panic!("null assertion must retain its checked Nothing conversion")
    };
    assert!(matches!(ir.expr(*arg), IrExpr::NotNullAssert { .. }));
}
