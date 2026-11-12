use super::*;
use krusty::backend::BackendCallableFact;
use krusty::libraries::{
    ExternalCallableKind, InlineKind, MemberRealization, RuntimeExceptionKind,
};
use krusty::types::SemanticCallableOwner;

fn declaration() -> BackendCallableFact {
    let owner = RuntimeExceptionKind::IllegalState.classifier();
    BackendCallableFact {
        name: "<init>".into(),
        physical_name: None,
        reflection_name: None,
        physical_owner: owner,
        visibility: krusty::types::Visibility::Public,
        declaration_owner: Some(SemanticCallableOwner::Classifier(owner)),
        singleton_dispatch: None,
        kind: ExternalCallableKind::Constructor,
        owner_is_interface: false,
        compiler_intrinsic: None,
        semantic_role: None,
        overridden_declarations: Box::new([]),
        primitive_implementations: Box::new([]),
        member_realization: MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor {
            kind: RuntimeExceptionKind::IllegalState,
            message: true,
        }),
        params: vec![Ty::nullable(Ty::String)],
        physical_params: vec![Ty::nullable(Ty::String)],
        parameter_identities: Box::new([]),
        physical_parameter_plan: None,
        ret: Ty::Unit,
        physical_ret: Ty::Unit,
        descriptor: String::new(),
        inline: InlineKind::None,
        source_receiver: None,
        context_count: 0,
        declared_params: None,
        declared_ret: None,
        inline_modifiers: Box::new([]),
        default_realization: None,
        nonvirtual_realization: None,
        overridden_call_realizations: Box::new([]),
        generic_sig: None,
        reified_type_parameter_ordinals: Box::new([]),
        declaration_signature: None,
        dependency_callable: None,
        direct_c_function: None,
        klib_body_companion_receiver: None,
        klib_body_type_parameters: Box::new([]),
        klib_body_declaration_modifiers: None,
        klib_body_class_contract: None,
        klib_body_accessor: None,
        klib_body_vararg_index: None,
        klib_body_parameter_defaults: None,
        declares_parameter_defaults: false,
        annotations: Box::new([]),
    }
}

#[test]
fn exception_constructor_frames_preserve_the_selected_owner_and_message_ordinal() {
    let mut fact = declaration();
    let kind = RuntimeExceptionKind::IllegalState;
    assert_eq!(
        constructor_frame(kind.classifier(), Some(&fact.params), 1, kind, true, &fact),
        Ok(())
    );
    fact.params.clear();
    assert_eq!(
        constructor_frame(kind.classifier(), Some(&[]), 0, kind, false, &fact),
        Ok(())
    );
}

#[test]
fn a_different_owner_or_incomplete_constructor_frame_declines() {
    let fact = declaration();
    let kind = RuntimeExceptionKind::IllegalState;
    let parameters = fact.params.clone();
    let expected =
        Err("an authenticated exception constructor with a mismatched declaration frame".into());
    for (internal, selected, count) in [
        (
            RuntimeExceptionKind::Throwable.classifier(),
            Some(parameters.as_slice()),
            1,
        ),
        (kind.classifier(), None, 1),
        (kind.classifier(), Some(&[][..]), 1),
        (kind.classifier(), Some(parameters.as_slice()), 0),
        (kind.classifier(), Some(parameters.as_slice()), 2),
    ] {
        assert_eq!(
            constructor_frame(internal, selected, count, kind, true, &fact),
            expected
        );
    }
    let non_null = [Ty::String];
    assert_eq!(
        constructor_frame(kind.classifier(), Some(&non_null), 1, kind, true, &fact),
        expected
    );
    type Mutation = fn(&mut BackendCallableFact);
    let mutations: &[Mutation] = &[
        |fact| fact.declaration_owner = None,
        |fact| {
            fact.declaration_owner = Some(SemanticCallableOwner::Classifier(
                RuntimeExceptionKind::Throwable.classifier(),
            ))
        },
        |fact| fact.kind = ExternalCallableKind::Member,
        |fact| fact.params = vec![Ty::String],
        |fact| fact.ret = Ty::String,
        |fact| fact.context_count = 1,
        |fact| fact.source_receiver = Some(Ty::String),
    ];
    for mutate in mutations {
        let mut wrong = fact.clone();
        mutate(&mut wrong);
        assert_eq!(
            constructor_frame(kind.classifier(), Some(&parameters), 1, kind, true, &wrong),
            expected
        );
    }
}

#[test]
fn finally_preflight_finds_every_control_transfer_before_emission() {
    for transfer in [
        IrExpr::Return(None),
        IrExpr::Break { label: None },
        IrExpr::Continue {
            label: Some("outer".into()),
        },
    ] {
        let exprs = vec![
            IrExpr::UnitInstance,
            transfer,
            IrExpr::Block {
                stmts: vec![1],
                value: Some(0),
            },
        ];
        for roots in [[2, 0, 0], [0, 2, 0], [0, 0, 2]] {
            assert!(finally_has_transfer(&exprs, roots));
        }
        assert!(!finally_has_transfer(&exprs, [0]));
    }
}

#[test]
fn finally_input_requires_the_completion_proof_for_its_actual_use() {
    use krusty::ir::IrBottomValueCompletion;
    let mut ir = IrFile::default();
    let producer = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
    ir.set_logical_type(producer, Ty::Int);
    let fallthrough = ir.add_expr(IrExpr::BottomValue {
        producer,
        completion: IrBottomValueCompletion::FallThroughWhenDiscarded,
    });
    ir.set_logical_type(fallthrough, Ty::Nothing);
    let diverging = ir.add_expr(IrExpr::BottomValue {
        producer,
        completion: IrBottomValueCompletion::Diverge,
    });
    ir.set_logical_type(diverging, Ty::Nothing);
    let declared = HashMap::default();
    assert_eq!(finally_input(&ir, &declared, fallthrough, false), Ok(true));
    assert_eq!(
        finally_input(&ir, &declared, fallthrough, true),
        Err("a `finally` region without a proven completion carrier".into())
    );
    assert_eq!(finally_input(&ir, &declared, diverging, true), Ok(true));
    assert_eq!(finally_input(&ir, &declared, producer, false), Ok(false));
    assert_eq!(finally_input(&ir, &declared, producer, true), Ok(false));
}
