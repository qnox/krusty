//! The return a callable body ends in: the terminal return of an implicit result, and the
//! closing line a `Unit` body that falls off its end marks on the return it gets.

use super::*;

#[test]
fn implicit_non_unit_callable_result_is_a_terminal_return_statement() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(10));
    body.set_result_type(resolved(Ty::Int));
    body.set_implicit_return();
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(12)),
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(value),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let callable_body = super::super::finish_callable_body(
        &mut ir,
        lowered.roots.into_vec(),
        lowered.result_type.unwrap(),
        lowered.implicit_return,
        false,
        0,
        origin,
    )
    .unwrap();

    let IrExpr::Block { stmts, value } = ir.expr(callable_body) else {
        panic!("callable body must be a block")
    };
    assert!(value.is_none());
    assert!(matches!(
        stmts.as_slice(),
        [returned] if matches!(ir.expr(*returned), IrExpr::Return(Some(_)))
    ));
}

#[test]
fn a_unit_body_falling_off_its_end_marks_its_closing_line_on_the_return() {
    let origin = OriginId::from_raw(0);
    let mut ir = IrFile::default();
    let callable_body =
        super::super::finish_callable_body(&mut ir, Vec::new(), Ty::Unit, true, true, 9, origin)
            .unwrap();

    let IrExpr::Block { stmts, .. } = ir.expr(callable_body) else {
        panic!("callable body must be a block")
    };
    let [returned] = stmts.as_slice() else {
        panic!("a Unit body appends exactly its return")
    };
    assert!(matches!(ir.expr(*returned), IrExpr::Return(Some(_))));
    assert_eq!(ir.fallthrough_return_line(*returned), Some(9));
}
