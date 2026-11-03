use super::*;
use crate::backend::BackendCallableFact;
use crate::libraries::{ExternalCallableKind, InlineKind, MemberRealization, RuntimeExceptionKind};
use crate::types::SemanticCallableOwner;

fn declaration() -> BackendCallableFact {
    let owner = RuntimeExceptionKind::IllegalState.classifier();
    BackendCallableFact {
        name: "<init>".into(),
        physical_name: None,
        reflection_name: None,
        physical_owner: owner,
        visibility: crate::types::Visibility::Public,
        declaration_owner: Some(SemanticCallableOwner::Classifier(owner)),
        singleton_dispatch: None,
        kind: ExternalCallableKind::Constructor,
        owner_is_interface: false,
        compiler_intrinsic: None,
        semantic_role: None,
        overridden_declarations: Box::new([]),
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
