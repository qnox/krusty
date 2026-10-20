//! Lowering of checked references to dependency declarations.

use super::*;
use crate::types::{type_name, TyKind};

#[test]
fn external_unbound_reference_trusts_the_checked_receiver_widening() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(19);
    let function_type = Ty::from(TyKind::Fun(crate::types::intern_fnsig(
        crate::types::FnSig {
            params: vec![Ty::String],
            ret: Ty::Boolean,
            context_count: 0,
            has_receiver: false,
            suspend: false,
        },
    )));
    let mut body = FirBody::new(BodyOwnerId::from_raw(5));
    let reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(function_type),
        kind: FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                singleton_dispatch: None,
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

#[test]
fn external_singleton_references_keep_distinct_dispatches_for_one_declaration() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(23);
    let singleton = type_name("fixture/Numbers$Companion");
    let inherited_singleton = type_name("fixture/InheritedNumbers");
    let function_type = Ty::from(TyKind::Fun(crate::types::intern_fnsig(
        crate::types::FnSig {
            params: vec![Ty::Int],
            ret: Ty::Float,
            context_count: 0,
            has_receiver: false,
            suspend: false,
        },
    )));
    let mut body = FirBody::new(BodyOwnerId::from_raw(7));
    let singleton_value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj_name(singleton)),
        kind: FirExprKind::SingletonValue {
            classifier: singleton,
        },
    });
    let reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(function_type),
        kind: FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                singleton_dispatch: Some(singleton),
                default_provider: None,
                receiver: Some(resolved(Ty::obj_name(singleton))),
                declared_receiver: None,
                extension_receiver: false,
                parameters: Box::new([resolved(Ty::Int)]),
                result: resolved(Ty::Float),
                declared_result: None,
                suspend: false,
                semantic_role: None,
                overridden_declarations: Box::new([]),
            },
            function_type: resolved(function_type),
            reflective: false,
            binding: FirCallableReferenceBinding::Bound,
            dispatch_receiver: Some(FirReceiver {
                value: singleton_value,
                conversion: None,
            }),
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
    let inherited_singleton_value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj_name(inherited_singleton)),
        kind: FirExprKind::SingletonValue {
            classifier: inherited_singleton,
        },
    });
    let inherited_reference = body.add_expr(FirExpr {
        origin,
        ty: resolved(function_type),
        kind: FirExprKind::CallableReference {
            target: FirCallableReferenceTarget::External {
                declaration,
                singleton_dispatch: Some(inherited_singleton),
                default_provider: None,
                receiver: Some(resolved(Ty::obj_name(inherited_singleton))),
                declared_receiver: None,
                extension_receiver: false,
                parameters: Box::new([resolved(Ty::Int)]),
                result: resolved(Ty::Float),
                declared_result: None,
                suspend: false,
                semantic_role: None,
                overridden_declarations: Box::new([]),
            },
            function_type: resolved(function_type),
            reflective: false,
            binding: FirCallableReferenceBinding::Bound,
            dispatch_receiver: Some(FirReceiver {
                value: inherited_singleton_value,
                conversion: None,
            }),
            extension_receiver: None,
            substitutions: Box::new([]),
            adaptation: None,
            reflection_owner: None,
        },
    });
    let inherited_root = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(inherited_reference),
    });
    body.push_root(inherited_root);

    let mut ir = IrFile::default();
    lower_body(body, &ResolvedModuleIndex::default(), &mut ir).expect("reference lowers");

    assert_eq!(ir.external_singleton_dispatches.len(), 2);
    assert_eq!(
        ir.external_singleton_dispatches
            .values()
            .filter(|&&selected| selected == singleton)
            .count(),
        1
    );
    assert_eq!(
        ir.external_singleton_dispatches
            .values()
            .filter(|&&selected| selected == inherited_singleton)
            .count(),
        1,
        "two inherited singleton selections may share one external declaration identity"
    );
}
