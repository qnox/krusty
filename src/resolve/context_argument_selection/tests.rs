use super::*;
use crate::resolve::scope::ScopeKind;
use crate::resolve::{ErrorProvenance, ScopeBinding};

fn local(ty: Ty, flow: u32, lexical: u32) -> Local {
    Local {
        ty,
        declared_ty: ty,
        write_ty: None,
        is_var: false,
        error_provenance: ErrorProvenance::None,
        origin: ReceiverFnValueOrigin::Local,
        is_context_parameter: false,
        callable_reference_type: None,
        delegate_storage_ty: None,
        shared_storage_cell: false,
        lexical_capture_identity: Some(lexical),
        lexical_declaration: None,
        flow_identity: flow,
        safe_call_origin: None,
    }
}

fn context(ty: Ty, flow: u32, lexical: u32) -> Local {
    Local {
        is_context_parameter: true,
        ..local(ty, flow, lexical)
    }
}

#[test]
fn selected_outer_context_retains_its_type_and_exact_capture_identity_under_shadowing() {
    let root = CheckerScope::root();
    let function = root.child(ScopeKind::Function { receiver: None });
    let ctx = Ty::obj("Ctx");
    function.rebind("c", Ns::Value, ScopeBinding::Value(context(ctx, 11, 71)));
    let block = function.child(ScopeKind::Block);
    block.rebind(
        "c",
        Ns::Value,
        ScopeBinding::Value(local(Ty::String, 22, 72)),
    );
    let class = block.child(ScopeKind::Class {
        ty: Ty::obj("Local"),
        carries_outer: true,
    });
    let member = class.child(ScopeKind::Function { receiver: None });
    member.rebind("c", Ns::Value, ScopeBinding::Value(local(Ty::Int, 33, 73)));
    let (selected, source) = selected_context_binding(&member, "c", 11).unwrap();
    assert_eq!(selected.ty, ctx);
    assert_eq!(selected.flow_identity, 11);
    assert_eq!(
        source,
        ContextBindingSource::RequiredClassCapture {
            lexical_binding: 71
        }
    );
    assert!(selected_context_binding(&member, "c", 999).is_none());
}

#[test]
fn a_context_declared_inside_the_member_is_positively_local() {
    let root = CheckerScope::root();
    let class = root.child(ScopeKind::Class {
        ty: Ty::obj("Local"),
        carries_outer: true,
    });
    let member = class.child(ScopeKind::Function { receiver: None });
    let ctx = Ty::obj("Ctx");
    member.rebind("c", Ns::Value, ScopeBinding::Value(context(ctx, 44, 74)));
    let body = member.child(ScopeKind::Block);
    body.rebind(
        "c",
        Ns::Value,
        ScopeBinding::Value(local(Ty::String, 55, 75)),
    );
    let (selected, source) = selected_context_binding(&body, "c", 44).unwrap();
    assert_eq!(selected.ty, ctx);
    assert_eq!(
        source,
        ContextBindingSource::ProvenLocalDepth {
            shadow_depth: 1,
            lexical_binding: Some(74),
        }
    );
}

#[test]
fn nested_callable_normalization_never_turns_a_required_capture_into_a_local() {
    let captured = ContextBindingSource::RequiredClassCapture {
        lexical_binding: 71,
    };
    assert_eq!(captured.outside_nested_bindings(99), Some(captured));
    let local = ContextBindingSource::ProvenLocalDepth {
        shadow_depth: 1,
        lexical_binding: Some(74),
    };
    assert_eq!(
        local.outside_nested_bindings(1),
        Some(ContextBindingSource::ProvenLocalDepth {
            shadow_depth: 0,
            lexical_binding: Some(74),
        })
    );
    assert_eq!(local.outside_nested_bindings(2), None);
}

#[test]
fn named_alias_absence_is_not_a_receiver_certificate() {
    let root = CheckerScope::root();
    assert_eq!(
        context_receiver_binding(&root, Some("c".into()), 0, None),
        Err(())
    );
    let span = crate::diag::Span::new(0, 0);
    assert_eq!(
        context_receiver_binding(
            &root,
            Some("c".into()),
            0,
            Some(ReceiverDeclarationRole::ContextParameter(span))
        ),
        Err(())
    );
    for role in [
        ReceiverDeclarationRole::Lambda {
            expression: span,
            slot: crate::resolve::LambdaReceiverSlot::Context(0),
        },
        ReceiverDeclarationRole::Extension(span),
    ] {
        assert_eq!(
            context_receiver_binding(&root, Some("receiver".into()), 0, Some(role)),
            Ok(None)
        );
    }
}

#[test]
fn named_alias_class_certificate_requires_the_selected_lexical_identity() {
    let root = CheckerScope::root();
    root.rebind(
        "c",
        Ns::Value,
        ScopeBinding::Value(context(Ty::obj("Ctx"), 11, 71)),
    );
    let class = root.child(ScopeKind::Class {
        ty: Ty::obj("Local"),
        carries_outer: true,
    });
    assert_eq!(
        context_receiver_binding(&class, Some("c".into()), 0, None),
        Ok(Some(ContextBinding {
            name: "c".into(),
            source: ContextBindingSource::RequiredClassCapture {
                lexical_binding: 71
            },
            flow_identity: Some(11),
        }))
    );
    let mut missing_identity = context(Ty::obj("Ctx"), 11, 71);
    missing_identity.lexical_capture_identity = None;
    root.rebind("c", Ns::Value, ScopeBinding::Value(missing_identity));
    assert_eq!(
        context_receiver_binding(&class, Some("c".into()), 0, None),
        Err(())
    );
}
