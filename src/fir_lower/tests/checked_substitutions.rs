use crate::fir::{
    BodyOwnerId, ExternalCallableId, FirBody, FirCall, FirExpr, FirExprKind, FirStatement,
    FirStatementKind, FirTypeParameterRef, FirTypeSubstitution, OriginId, ResolvedModuleIndex,
};
use crate::ir::{IrExpr, IrFile};
use crate::types::Ty;

use super::{lower_body, resolved};

#[test]
fn external_call_keeps_checked_type_substitutions_until_provider_realization() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(23);
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let call = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Call(FirCall {
            target: crate::fir::FirCallTarget::External {
                declaration,
                default_provider: None,
                receiver: None,
                declared_receiver: None,
                parameters: Box::new([]),
                result: resolved(Ty::String),
                declared_result: None,
                overridden_results: Box::new([]),
                suspend: false,
                can_inline: true,
                inline_plan: None,
                extension_receiver_parameter: None,
                semantic_role: Some(crate::types::SemanticCallRole::KotlinAnyToString),
                overridden_declarations: Box::new([]),
            },
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: Box::new([]),
            arguments: Box::new([]),
            substitutions: Box::new([FirTypeSubstitution {
                parameter: FirTypeParameterRef::External {
                    callable: declaration,
                    ordinal: 0,
                },
                reified: false,
                value: resolved(Ty::String),
                reified_runtime: resolved(Ty::String),
                additional_bounds: Box::new([]),
            }]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert_eq!(
        ir.semantic_call_roles.get(&lowered.roots[0]),
        Some(&crate::types::SemanticCallRole::KotlinAnyToString)
    );
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::Call {
            callee: crate::ir::Callee::External { substitutions, .. },
            ..
        } if matches!(
            substitutions.as_slice(),
            [crate::ir::IrCheckedSubstitution {
                parameter: FirTypeParameterRef::External { callable, ordinal: 0 },
                reified: false,
                value: Ty::String,
                reified_runtime: Ty::String,
                additional_bounds,
            }] if *callable == declaration && additional_bounds.is_empty()
        )
    ));
}
