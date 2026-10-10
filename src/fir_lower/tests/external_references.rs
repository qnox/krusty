//! Lowering of checked references to dependency declarations.

use super::*;

#[test]
fn external_unbound_reference_trusts_the_checked_receiver_widening() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(19);
    let function_type = Ty::Fun(crate::types::intern_fnsig(crate::types::FnSig {
        params: vec![Ty::String],
        ret: Ty::Boolean,
        context_count: 0,
        has_receiver: false,
        suspend: false,
    }));
    let mut body = FirBody::new(BodyOwnerId::from_raw(5));
    let reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(function_type),
        kind: FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                default_provider: None,
                receiver: Some(resolved(Ty::nullable(Ty::String))),
                declared_receiver: Some(resolved(Ty::nullable(Ty::String))),
                extension_receiver: true,
                parameters: Box::new([]),
                result: resolved(Ty::Boolean),
                declared_result: Some(resolved(Ty::Boolean)),
                suspend: false,
                semantic_role: None,
                overridden_declarations: Box::new([]),
            },
            function_type: resolved(function_type),
            reflective: false,
            binding: FirCallableReferenceBinding::Unbound,
            dispatch_receiver: None,
            extension_receiver: None,
            substitutions: Box::new([]),
            adaptation: None,
            reflection_owner: None,
        },
    });
    let root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(reference),
    });
    body.push_root(root);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let IrExpr::CallableReference(reference) = ir.expr(lowered.roots[0]) else {
        panic!("an external reference lowers to a callable reference");
    };
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::External {
            declaration: target,
            receiver: Some(receiver),
        } if target == declaration && receiver == Ty::nullable(Ty::String)
    ));
    assert!(reference.captures.is_empty() && reference.bound_receiver.is_none());
    // The adapter takes the function type's `String`; the declaration's `String?` receiver accepts
    // it as checked, with no cast of its own.
    assert_eq!(
        ir.functions[reference.adapter as usize].params,
        [Ty::String]
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::External { target, .. },
            ..
        } if *target == declaration
    )));
}
