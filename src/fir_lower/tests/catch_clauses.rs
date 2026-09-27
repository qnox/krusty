//! What a checked catch clause carries into common IR.

use super::*;

#[test]
fn catch_parameter_name_survives_checked_fir_lowering() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let parameter = body.allocate_local_value();
    body.set_debug_value_name(parameter, "failure");
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
    let attempt = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Try {
            body: one,
            catches: Box::new([FirCatch {
                origin,
                parameter,
                parameter_ty: resolved(Ty::obj("java/lang/Exception")),
                body: two,
                debug_line: 0,
            }]),
            finally: None,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(attempt),
    });
    body.push_root(root);
    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let catch = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Try { catches, .. } => catches.first(),
            _ => None,
        })
        .expect("checked try expression must retain its catch clause");
    assert_eq!(catch.binding_name(), Some("failure"));
}
