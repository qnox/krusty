//! Checked local value parameters never include the lifted capture or extension-receiver prefix.

use super::*;

#[test]
fn a_local_call_with_a_mismatched_checked_parameter_count_is_rejected() {
    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let callable = body.allocate_local_callable();
    let declaration = crate::fir::BodyLocalCallableDeclarationId::new(body.owner(), 0);
    let mut nested = FirBody::new_local(body.owner(), callable);
    nested.set_debug_name("local");
    nested.set_result_type(resolved(Ty::Int));
    nested.set_implicit_return();
    let value = nested.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::Constant(FirConstant::Int(42)),
    });
    let result = nested.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(value),
    });
    nested.push_root(result);
    let function = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::LocalFunction {
            declaration,
            callable,
            suspend: false,
            tailrec: false,
            body: Box::new(nested),
        },
    });
    body.push_root(function);
    let target = FirLocalCallableRef {
        body_depth: 0,
        callable,
        declaration: Some(declaration),
        external_capture_arguments: None,
    };
    let call = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::Int),
        kind: FirExprKind::LocalCall {
            target: target.clone(),
            extension_receiver: None,
            arguments: Box::new([]),
            parameter_types: Box::new([resolved(Ty::Int)]),
        },
    });
    let result = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(result);
    let result = lower_body(
        body,
        &ResolvedModuleIndex::default(),
        &mut IrFile::default(),
    );
    assert_eq!(
        result.unwrap_err(),
        crate::fir_lower::FirLoweringFailure::MalformedLocalCallParameters {
            target,
            checked: 1,
            declared: 0,
        }
    );
}
